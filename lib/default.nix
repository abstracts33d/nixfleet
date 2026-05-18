# LOADBEARING: mk-fleet.nix takes only {lib}; pure consumers import it directly to avoid the inputs dependency.
{
  inputs,
  lib,
}: let
  mkFleetImpl = import ./mk-fleet.nix {inherit lib;};
  mkHost = import ./mk-host.nix {inherit inputs lib;};

  # Framework-owned mkFleet → nixosConfigurations wiring (RFC-0004 §2.2
  # + §3 anti-pattern #4). The user authors `hosts.<name>.nixosArgs`
  # once; the framework computes `nixosConfigurations` by calling
  # `mkHost` for each host with `fleetResolved = fleet.resolved`
  # pre-bound. Operators never pass `fleetResolved` themselves — a
  # whole class of silent-no-op bugs (DEFECT-001 / DEFECT-002) is
  # closed by framework design.
  #
  # Operator pattern:
  #
  #   let fleet = mkFleet {
  #         hosts.cp = {
  #           system = "x86_64-linux";
  #           channel = "stable";
  #           nixosArgs.modules = [./hosts/cp];
  #         };
  #         channels.stable = {...};
  #         rolloutPolicies = {...};
  #       };
  #   in {
  #     nixosConfigurations = fleet.nixosConfigurations;
  #   }
  #
  # The wrapper preserves `mk-fleet.nix`'s `{lib}`-only invariant —
  # the lift lives here in `lib/default.nix` where `inputs` is in
  # scope. The pure mkFleet output is still accessible at
  # `fleet.resolved` for tests that don't need built nixosSystems.
  mkFleet = input: let
    rawFleet = mkFleetImpl.mkFleet input;
    nixosConfigurations =
      lib.mapAttrs (
        hostName: hostCfg:
          mkHost (
            {
              inherit hostName;
              platform = hostCfg.system;
              fleetResolved = rawFleet.resolved;
            }
            // (hostCfg.nixosArgs or {})
          )
      )
      (rawFleet.hosts or {});
  in
    rawFleet
    // {
      inherit nixosConfigurations;
    };
in {
  inherit mkHost mkFleet;
  inherit (mkFleetImpl) mergeFleets withSignature;
  mkVmApps = import ./mk-vm-apps.nix {inherit inputs;};
}
