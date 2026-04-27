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
  hostSpecModule = ../modules/host-spec.nix;

  # Core modules (plain NixOS/Darwin modules)
  coreNixos = ../modules/core/_nixos.nix;
  coreDarwin = ../modules/core/_darwin.nix;

  # Service modules (auto-included, disabled by default)
  agentModule = ../modules/scopes/nixfleet/_agent.nix;
  agentDarwinModule = ../modules/scopes/nixfleet/_agent-darwin.nix;
  controlPlaneModule = ../modules/scopes/nixfleet/_control-plane.nix;
  cacheModule = ../modules/scopes/nixfleet/_cache.nix;
  microvmHostModule = ../modules/scopes/nixfleet/_microvm-host.nix;
  operatorModule = ../modules/scopes/nixfleet/_operator.nix;

  # Framework-level persistence schema (pure schema, no impl).
  # Auto-imported so nixfleet's service modules can contribute to
  # `nixfleet.persistence.directories` via standard option merging.
  # The actual persistence implementation (impermanence flake bind-
  # mounts, ZFS rollback, snapper, …) lives in `nixfleet-scopes/
  # modules/scopes/persistence/*`; consumer fleets import exactly one.
  # The operators schema is *not* in the framework — it lives in
  # nixfleet-scopes/modules/scopes/operators/. The framework reads
  # only `hostSpec.{userName, rootSshKeys}`; the operators scope (when
  # imported) populates those fields from its own option tree.
  persistenceModule = ../modules/scopes/nixfleet/_persistence.nix;

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
    # Override the `inputs` attrset injected into NixOS/Darwin
    # specialArgs. Defaults to the framework's own flake inputs
    # (sufficient for hosts that consume only nixfleet). Consumer
    # fleets that want their *own* inputs visible to imported
    # modules — e.g. so a fleet-side role can do
    # `imports = [inputs.nixfleet-scopes.scopes.roles.X]` — pass
    # `extraInputs = inputs` from their flake's outputs lambda.
    # Merged into the framework inputs so framework-side modules
    # still see what they need (impermanence, disko, nixpkgs, …).
    extraInputs ? {},
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
    # `_persistence.nix` is auto-imported because nixfleet's own
    # internal service modules (agent, control-plane, microvm-host)
    # conditionally contribute to `nixfleet.persistence.directories`,
    # and the NixOS module system validates option paths even inside
    # `lib.mkIf false`. The module declares the schema only — pure
    # data — so nothing materialises unless the consumer also imports
    # a persistence implementation (e.g.
    # `nixfleet-scopes.scopes.persistence.impermanence`) that reads
    # the schema and applies its mechanism.
    #
    # The framework reads only `hostSpec.{userName, rootSshKeys}` for
    # primary-user identity and root SSH access. The operators scope
    # (in nixfleet-scopes) populates those fields when imported; bare
    # fleets without the scope set them directly.
    frameworkNixosModules =
      [
        {nixpkgs.hostPlatform = platform;}
        hostSpecModule
        {hostSpec = lib.mapAttrs (_: v: lib.mkDefault v) effectiveHostSpec;}
        # Override hostName without mkDefault (must match)
        {hostSpec.hostName = hostName;}
        persistenceModule
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
      coreDarwin
      agentDarwinModule
      operatorModule
    ];

    # specialArgs.inputs visible to all imported modules — consumer's
    # extra inputs (e.g. `nixfleet-scopes`, fleet-specific flakes)
    # merged BENEATH the framework's own inputs. Framework wins on
    # collision so that:
    #   - `inputs.self` resolves to nixfleet for framework modules
    #     (which read `inputs.self.packages.<sys>.nixfleet-{agent,
    #     control-plane,cli}` to find their binaries),
    #   - common inputs (nixpkgs, home-manager, disko, …) come from
    #     the framework's pinned versions.
    # Consumer-only attrs (`nixfleet-scopes`, fleet-specific flakes
    # the framework doesn't declare) survive the merge unshadowed.
    # Fleet-side modules that need fleet's own self read it via
    # closure capture from the `outputs = inputs: …` lambda; that
    # path is unaffected by specialArgs.
    effectiveInputs = extraInputs // inputs;

    # Build NixOS system.
    buildNixos = inputs.nixpkgs.lib.nixosSystem {
      specialArgs = {inputs = effectiveInputs;};
      modules = [{system.stateVersion = lib.mkDefault stateVersion;}] ++ frameworkNixosModules ++ modules;
    };

    # Build Darwin system. stateVersion is Darwin-specific (integer);
    # consumers set `system.stateVersion` in their host modules.
    buildDarwin = inputs.darwin.lib.darwinSystem {
      specialArgs = {inputs = effectiveInputs;};
      modules = frameworkDarwinModules ++ modules;
    };
  in
    if isDarwin
    then buildDarwin
    else buildNixos
