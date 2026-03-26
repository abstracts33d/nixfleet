# NixFleet Framework Export
#
# Auto-imported by import-tree. Exposes the framework API and
# exports flakeModules.default for external client consumption.
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
    # For external clients: imports = [inputs.nixfleet.flakeModules.default];
    # Bakes in framework inputs so consumers get the right nixpkgs/HM/etc.
    flakeModules.default = import ./_shared/lib/flake-module.nix {frameworkInputs = inputs;};

    # For non-flake-parts consumers: inputs.nixfleet.lib.nixfleet.mkFleet
    lib.nixfleet = nixfleetLib;
  };
}
