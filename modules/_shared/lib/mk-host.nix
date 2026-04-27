# mkHost - the single NixFleet API function.
# Returns a nixosSystem or darwinSystem with framework-level mechanism only.
#
# Opinions (base CLI tools, firewall, secrets, backup, monitoring,
# impermanence, home-manager, disko) live in `arcanesys/nixfleet-scopes`.
# Consumers compose them via the `modules` argument:
#
#     mkHost {
#       hostName = "myhost"; platform = "x86_64-linux";
#       hostSpec = { userName = "alice"; };
#       modules = [
#         inputs.nixfleet-scopes.scopes.roles.workstation  # WHAT it is
#         ./modules/profiles/developer.nix                  # WHO uses it / HOW
#         ./modules/hardware/desktop-amd-nvidia.nix         # WHAT hardware
#         ./modules/hosts/myhost/hardware-configuration.nix
#       ];
#     };
#
# Per Decision 2 + 3 of the scopes-extraction plan (rev 4):
# - Home Manager is a scope (not a framework service); consumers pull it
#   in via `nixfleet-scopes.scopes.home-manager` (usually indirectly
#   through a role) and add their own user-level HM imports.
# - disko + impermanence are scopes too; mkHost does not auto-import
#   their NixOS modules any more.
{
  inputs,
  lib,
}: let
  hostSpecModule = ../host-spec-module.nix;

  # Core modules (plain NixOS/Darwin modules)
  coreNixos = ../../core/_nixos.nix;
  coreDarwin = ../../core/_darwin.nix;

  # Service modules (auto-included, disabled by default)
  agentModule = ../../scopes/nixfleet/_agent.nix;
  agentDarwinModule = ../../scopes/nixfleet/_agent_darwin.nix;
  controlPlaneModule = ../../scopes/nixfleet/_control-plane.nix;
  cacheModule = ../../scopes/nixfleet/_cache.nix;
  microvmHostModule = ../../scopes/nixfleet/_microvm-host.nix;
  operatorModule = ../../scopes/nixfleet/_operator.nix;

  # Framework-level scopes absorbed from former nixfleet-scopes.
  # `_impermanence.nix` declares + wires `nixfleet.impermanence.*` so
  # nixfleet's own service modules can contribute to environment.persistence.
  # `_operators.nix` declares the `nixfleet.operators.*` schema referenced
  # by core/_nixos.nix and host-spec; user creation implementation stays in
  # nixfleet-scopes for fleets that opt into it.
  impermanenceModule = ../../scopes/nixfleet/_impermanence.nix;
  operatorsModule = ../../scopes/nixfleet/_operators.nix;

  isDarwinPlatform = platform:
    builtins.elem platform ["aarch64-darwin" "x86_64-darwin"];
in
  {
    hostName,
    platform,
    stateVersion ? "24.11",
    hostSpec ? {},
    modules ? [],
    isVm ? false,
  }: let
    isDarwin = isDarwinPlatform platform;

    # Merge hostName + isDarwin into hostSpec (always present).
    effectiveHostSpec =
      {inherit hostName;}
      // hostSpec
      // lib.optionalAttrs isDarwin {inherit isDarwin;};

    # Framework NixOS modules injected by mkHost.
    # Mechanism only: core system config + hostSpec + nixfleet service
    # modules. No HM injection, no disko auto-import.
    #
    # `_impermanence.nix` is auto-imported because nixfleet's own
    # internal service modules (agent, control-plane, microvm-host)
    # conditionally contribute to `environment.persistence`, and the
    # NixOS module system validates option paths even inside
    # `lib.mkIf false`. The module declares the option (via the upstream
    # `impermanence` flake input) and is inert until
    # `nixfleet.impermanence.enable = true`, so the cost is zero.
    # `_operators.nix` is auto-imported for the same reason — core
    # modules read `config.nixfleet.operators.*` for the primary user
    # and root SSH keys, so the schema must always exist.
    frameworkNixosModules =
      [
        {nixpkgs.hostPlatform = platform;}
        hostSpecModule
        {hostSpec = lib.mapAttrs (_: v: lib.mkDefault v) effectiveHostSpec;}
        # Override hostName without mkDefault (must match)
        {hostSpec.hostName = hostName;}
        impermanenceModule
        operatorsModule
        coreNixos
        agentModule
        controlPlaneModule
        cacheModule
        microvmHostModule
        operatorModule
      ]
      ++ lib.optionals isVm [
        ../../_hardware/qemu/disk-config.nix
        ../../_hardware/qemu/hardware-configuration.nix
        ({
          lib,
          pkgs,
          ...
        }: {
          services.spice-vdagentd.enable = true;
          networking.useDHCP = lib.mkForce true;
          environment.variables.LIBGL_ALWAYS_SOFTWARE = "1";
          environment.systemPackages = [pkgs.mesa];
        })
      ];

    # Framework Darwin modules injected by mkHost.
    #
    # The operator scope is platform-agnostic (just systemPackages +
    # an env var) so it gets the same module file as NixOS. Same
    # `enable=false` default means Darwin hosts that don't need
    # operator tooling aren't impacted; aether enables it via fleet
    # wiring (see fleet/modules/nixfleet/operator.nix).
    frameworkDarwinModules = [
      {nixpkgs.hostPlatform = platform;}
      hostSpecModule
      {hostSpec = lib.mapAttrs (_: v: lib.mkDefault v) effectiveHostSpec;}
      {hostSpec.hostName = hostName;}
      {hostSpec.isDarwin = true;}
      operatorsModule
      coreDarwin
      agentDarwinModule
      operatorModule
    ];

    # Build NixOS system. Framework inputs passed via specialArgs so
    # consumer-imported modules (including nixfleet-scopes scopes) can
    # reach inputs.home-manager, inputs.disko, inputs.impermanence, …
    buildNixos = inputs.nixpkgs.lib.nixosSystem {
      specialArgs = {inherit inputs;};
      modules = [{system.stateVersion = lib.mkDefault stateVersion;}] ++ frameworkNixosModules ++ modules;
    };

    # Build Darwin system. stateVersion is Darwin-specific (integer);
    # consumers set `system.stateVersion` in their host modules.
    buildDarwin = inputs.darwin.lib.darwinSystem {
      specialArgs = {inherit inputs;};
      modules = frameworkDarwinModules ++ modules;
    };
  in
    if isDarwin
    then buildDarwin
    else buildNixos
