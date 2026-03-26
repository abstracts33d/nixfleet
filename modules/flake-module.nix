# NixFleet Framework Export (monorepo wrapper)
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
    flakeModules.default = import ./_shared/lib/flake-module.nix {frameworkInputs = null;};

    # For non-flake-parts consumers: inputs.nixfleet.lib.nixfleet.mkFleet
    lib.nixfleet = nixfleetLib;
  };
}
