# NixFleet Framework Export
#
# Auto-imported by import-tree. Exposes the framework API and
# exports flakeModules for external client consumption.
#
# Exported flakeModules:
#   default   — lib + core (deferred NixOS/Darwin/HM modules)
#   apps      — install, launch-vm, build-switch, validate, etc.
#   tests     — eval + VM checks (nix flake check)
#   iso       — custom NixOS installer ISO
#   formatter — treefmt-nix (alejandra + shfmt + deadnix)
{
  inputs,
  config,
  lib,
  ...
}: let
  nixfleetLib = import ./_shared/lib/default.nix {inherit inputs config lib;};
in {
  options.nixfleet.lib = lib.mkOption {
    type = lib.types.attrs;
    default = nixfleetLib;
    readOnly = true;
    description = "NixFleet library (mkFleet, mkOrg, mkRole, mkHost, mkBatchHosts, mkTestMatrix)";
  };

  config.flake = {
    flakeModules = {
      # Core: lib + deferred NixOS/Darwin/HM modules.
      # For external clients: imports = [inputs.nixfleet.flakeModules.default];
      # Bakes in framework inputs so consumers get the right nixpkgs/HM/etc.
      default = import ./_shared/lib/flake-module.nix {frameworkInputs = inputs;};

      # Apps: install, launch-vm, build-switch, validate, test-vm, etc.
      apps = ./apps.nix;

      # Tests: eval assertions + VM integration tests.
      # Uses consumer's self.nixosConfigurations (correct — tests validate the fleet).
      tests = {
        imports = [
          ./tests/eval.nix
          ./tests/vm.nix
        ];
      };

      # ISO: custom NixOS minimal installer with SSH keys.
      iso = ./iso.nix;

      # Formatter: treefmt-nix (alejandra + shfmt + deadnix).
      formatter = ./formatter.nix;
    };

    # For non-flake-parts consumers: inputs.nixfleet.lib.nixfleet.mkFleet
    lib.nixfleet = nixfleetLib;
  };
}
