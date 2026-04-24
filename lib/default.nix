# lib/default.nix
#
# nixfleet library entry point. Imports are keyed by capability so consumers
# can depend on narrow slices (e.g. just `mkFleet`) without pulling the full
# framework module graph.
{lib}: let
  impl = import ./mkFleet.nix {inherit lib;};
in {
  inherit (impl) mkFleet withSignature;
}
