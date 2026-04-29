# Core Darwin module - framework prerequisites only.
#
# What the framework needs every Darwin host to have:
# - `system.stateVersion` (nix-darwin requires it; mkDefault so the
#   host can override).
# - `system.primaryUser` from `hostSpec.userName` (nix-darwin's HM
#   bridge requires it).
# - `system.checks.verifyNixPath = false` — Darwin flake setups
#   don't have NIX_PATH set; the verify step would fail.
# - `hostSpec.isDarwin = true` — schema marker.
# - the trust contract schema (so `nixfleet.trust.*` typechecks
#   under `_agent-darwin.nix` the same way `_nixos.nix` consumes
#   it for `_agent.nix`).
#
# Everything else — Dock management, nix.conf wiring (Determinate vs
# stock), TouchID for sudo, nixpkgs.config opinions, homebrew —
# is fleet-side.
{
  config,
  lib,
  ...
}: let
  hS = config.hostSpec;
in {
  imports = [../../contracts/trust.nix];

  system.stateVersion = lib.mkDefault 4;
  system.checks.verifyNixPath = false;
  system.primaryUser = "${hS.userName}";

  hostSpec.isDarwin = true;
}
