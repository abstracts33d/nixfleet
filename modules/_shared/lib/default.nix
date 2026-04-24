# Public API of the NixFleet framework library.
{
  inputs,
  lib,
}: let
  mkFleetImpl = import ../../../lib/mkFleet.nix {inherit lib;};
in {
  mkHost = import ./mk-host.nix {inherit inputs lib;};
  mkVmApps = import ./mk-vm-apps.nix {inherit inputs;};
  inherit (mkFleetImpl) mkFleet mergeFleets withSignature;
}
