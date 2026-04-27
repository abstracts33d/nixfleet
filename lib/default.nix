# NixFleet framework library — wired entry point.
#
# Returns the full public API. Takes both `inputs` (for mkHost /
# mkVmApps, which need the flake's nixpkgs / darwin / home-manager
# inputs) and `lib` (for mkFleet's pure module evaluation).
#
# Pure consumers that only need mkFleet (e.g. the canonicalize binary,
# eval-only tests) import `./mk-fleet.nix` directly with just `{lib}`
# — that file's signature stays pure on purpose.
{
  inputs,
  lib,
}: let
  mkFleetImpl = import ./mk-fleet.nix {inherit lib;};
in {
  mkHost = import ./mk-host.nix {inherit inputs lib;};
  mkVmApps = import ./mk-vm-apps.nix {inherit inputs;};
  inherit (mkFleetImpl) mkFleet mergeFleets withSignature;
}
