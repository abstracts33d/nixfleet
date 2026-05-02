# mkHost - the single NixFleet API function.
# Returns a nixosSystem or darwinSystem with framework-level mechanism only.
#
# Service deployment opinions (firewall config, monitoring, home-manager
# wiring, disko layouts, role bundles) live in the consuming fleet — not
# in nixfleet. mkHost ships the framework runtime (agent, control-plane,
# cache, microvm-host service modules) plus the host-spec + persistence
# + trust contract schemas, and nothing else. Consumers compose the rest
# via the `modules` argument:
#
#     mkHost {
#       hostName = "myhost"; platform = "x86_64-linux";
#       hostSpec = { userName = "alice"; };
#       modules = [
#         # Contract impls — opt in to those you want
#         inputs.nixfleet.scopes.persistence.impermanence
#         inputs.nixfleet.scopes.keyslots.tpm
#         # Fleet-local service modules
#         ./modules/scopes/firewall
#         ./modules/scopes/monitoring
#         ./modules/profiles/developer.nix
#         ./modules/hardware/desktop-amd-nvidia.nix
#         ./modules/hosts/myhost/hardware-configuration.nix
#       ];
#     };
#
# - Home Manager is fleet-side: consumers wire in their preferred HM
#   modules and per-user imports themselves; mkHost does not inject HM.
# - disko + impermanence are fleet-side too; the framework declares the
#   persistence schema (contracts/persistence.nix) but ships only the
#   impermanence impl as opt-in (flake.scopes.persistence.impermanence).
{
  inputs,
  lib,
}: let
  hostSpecModule = ../contracts/host-spec.nix;

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
  # Persistence impls (impermanence + future ZFS rollback, snapper, …)
  # live under `impls/persistence/` and are exposed at
  # `flake.scopes.persistence.<impl>`; consumer fleets pick one.
  # The framework reads only `hostSpec.{userName, rootSshKeys}` for
  # identity; consumers populate hostSpec however they like (directly,
  # via a fleet-side operators schema, etc.).
  persistenceModule = ../contracts/persistence.nix;

  isDarwinPlatform = platform:
    builtins.elem platform ["aarch64-darwin" "x86_64-darwin"];
in
  {
    hostName,
    platform,
    stateVersion ? "24.11",
    hostSpec ? {},
    modules ? [],
    # Forward-compatible flag callers may pass to signal "this host is
    # a VM". Preserved as a passthrough for consumer code that already
    # threads it through; the framework no longer reads it. Qemu /
    # mesa / spice test-rig opinions moved to
    # `tests/lib/mk-test-host.nix`, which wraps mkHost. Consumer fleets
    # that want VM-specific config compose it via `modules`.
    isVm ? false,
    # Override the `inputs` attrset injected into NixOS/Darwin
    # specialArgs. Defaults to the framework's own flake inputs
    # (sufficient for hosts that consume only nixfleet). Consumer
    # fleets that want their *own* inputs visible to imported
    # modules — e.g. so a fleet-side role can do
    # `imports = [inputs.<some-fleet-input>...]` — pass
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
    # `contracts/persistence.nix` is auto-imported because nixfleet's
    # own internal service modules (agent, control-plane, microvm-host)
    # conditionally contribute to `nixfleet.persistence.directories`,
    # and the NixOS module system validates option paths even inside
    # `lib.mkIf false`. The module declares the schema only — pure
    # data — so nothing materialises unless the consumer also imports
    # a persistence implementation (e.g.
    # `inputs.nixfleet.scopes.persistence.impermanence`) that reads
    # the schema and applies its mechanism.
    #
    # The framework reads only `hostSpec.{userName, rootSshKeys}` for
    # primary-user identity and root SSH access. Consumers populate
    # hostSpec however they like — directly, via a fleet-side
    # operators schema, or any other mechanism.
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
        ../tests/fixtures/qemu/disk-config.nix
        ../tests/fixtures/qemu/hardware-configuration.nix
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
    # extra inputs (fleet-specific flakes) merged BENEATH the
    # framework's own inputs. Framework wins on collision so that:
    #   - `inputs.self` resolves to nixfleet for framework modules
    #     (which read `inputs.self.packages.<sys>.nixfleet-{agent,
    #     control-plane,cli}` to find their binaries),
    #   - common inputs (nixpkgs, home-manager, disko, …) come from
    #     the framework's pinned versions.
    # Consumer-only attrs (fleet-specific flakes the framework doesn't
    # declare) survive the merge unshadowed.
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
