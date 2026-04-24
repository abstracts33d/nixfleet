# lib/flake-module.nix
#
# Exposes `config.flake.lib.mkFleet` via flake-parts so consumers can call
# `nixfleet.lib.mkFleet { ... }` from their own flakes.
{lib, ...}: {
  config.flake.lib.mkFleet =
    (import ./mkFleet.nix {inherit lib;}).mkFleet;
}
