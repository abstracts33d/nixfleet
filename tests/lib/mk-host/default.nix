# mk-host regression tests. Currently covers the dispatch/declaration
# parity assert introduced when the framework stopped injecting
# nixpkgs.hostPlatform. Extend as mkHost surface grows.
{
  inputs,
  lib,
}: let
  args = {inherit inputs lib;};
  match = import ./dispatch-match.nix args;
  mismatch = import ./dispatch-assert.nix args;
in {
  results = [match mismatch];
}
