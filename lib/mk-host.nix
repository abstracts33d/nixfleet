{
  inputs,
  lib,
}: let
  hostSpecModule = ../contracts/host-spec.nix;

  coreNixos = ../modules/core/_nixos.nix;
  coreDarwin = ../modules/core/_darwin.nix;

  agentModule = ../modules/scopes/nixfleet/_agent.nix;
  agentDarwinModule = ../modules/scopes/nixfleet/_agent-darwin.nix;
  controlPlaneModule = ../modules/scopes/nixfleet/_control-plane.nix;
  cacheModule = ../modules/scopes/nixfleet/_cache.nix;
  microvmHostModule = ../modules/scopes/nixfleet/_microvm-host.nix;
  operatorModule = ../modules/scopes/nixfleet/_operator.nix;

  # Schema only; auto-imported so service modules can contribute to
  # nixfleet.persistence.directories via option merging without an impl.
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
    # Passthrough for consumer code; framework no longer reads it.
    isVm ? false,
    # LOADBEARING: extraInputs merged BENEATH framework inputs so framework wins (inputs.self -> nixfleet).
    extraInputs ? {},
    # `mkFleet`'s `.resolved` attrset, ALWAYS supplied by the framework
    # `mkFleet` wrapper in `lib/default.nix` (never by the operator).
    # The wrapper iterates `cfg.hosts` and calls `mkHost` with this
    # value pre-bound; operators consume the result as
    # `fleet.nixosConfigurations` and never name `fleetResolved`
    # themselves. This closes DEFECT-001/-002: an entire class of
    # silent no-op bugs ("operator forgot to wire fleetResolved →
    # probes silently disappear") is impossible by design
    # (RFC-0011 §2.2 + §3 anti-pattern #4).
    #
    # If you find yourself calling `mkHost` directly with
    # `fleetResolved = ...`, you're outside the framework path —
    # consider whether `mkFleet { hosts = {...}; }` would work
    # instead. Direct `mkHost` is supported for one-off / test rigs
    # (tests/lib/mk-test-host.nix uses it that way) but is not the
    # operator path.
    fleetResolved,
  }: let
    isDarwin = isDarwinPlatform platform;

    effectiveHostSpec =
      {inherit hostName;}
      // hostSpec
      // lib.optionalAttrs isDarwin {inherit isDarwin;};

    # RFC-0010 §4 closure-driven probe topology. The resolved set lives
    # at `fleet.resolved.effectiveHealthChecks.<hostName>` per
    # `lib/mk-fleet.nix`'s `resolveHealthChecks` (host > tag > fleet
    # precedence). The framework wrapper passes `fleet.resolved` here;
    # `_agent.nix` reads the resulting attrset to render the on-disk
    # `/etc/nixfleet/agent/health-checks.json`.
    effectiveHealthChecks = fleetResolved.effectiveHealthChecks.${hostName} or {};

    healthChecksModule = {
      services.nixfleet-agent.effectiveHealthChecks = effectiveHealthChecks;
    };

    frameworkNixosModules =
      [
        {nixpkgs.hostPlatform = platform;}
        hostSpecModule
        {hostSpec = lib.mapAttrs (_: v: lib.mkDefault v) effectiveHostSpec;}
        # LOADBEARING: hostName is not mkDefault; must match exactly.
        {hostSpec.hostName = hostName;}
        persistenceModule
        coreNixos
        agentModule
        healthChecksModule
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

    frameworkDarwinModules = [
      {nixpkgs.hostPlatform = platform;}
      hostSpecModule
      {hostSpec = lib.mapAttrs (_: v: lib.mkDefault v) effectiveHostSpec;}
      {hostSpec.hostName = hostName;}
      {hostSpec.isDarwin = true;}
      coreDarwin
      agentDarwinModule
      healthChecksModule
      operatorModule
    ];

    effectiveInputs = extraInputs // inputs;

    buildNixos = inputs.nixpkgs.lib.nixosSystem {
      specialArgs = {inputs = effectiveInputs;};
      modules = [{system.stateVersion = lib.mkDefault stateVersion;}] ++ frameworkNixosModules ++ modules;
    };

    # stateVersion is Darwin-specific (integer); consumers set it themselves.
    buildDarwin = inputs.darwin.lib.darwinSystem {
      specialArgs = {inputs = effectiveInputs;};
      modules = frameworkDarwinModules ++ modules;
    };
  in
    if isDarwin
    then buildDarwin
    else buildNixos
