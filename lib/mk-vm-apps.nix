# mkVmApps — generate VM lifecycle apps for fleet repos.
#
# Usage in fleet flake.nix:
#   apps = nixfleet.lib.mkVmApps { inherit pkgs; };
#
# Returns: { build-vm, start-vm, stop-vm, clean-vm, test-vm } on Linux.
# Darwin: empty attrset. The Darwin path was speculative —
# aarch64-darwin's `pkgs.OVMF` is marked broken upstream, and there's
# no fleet use-case where a Darwin operator workstation is expected to
# host a fleet VM (the deployment surface is `nixos-anywhere` against
# a remote Linux host).
#
# This file is a thin orchestrator. The implementation is split across
# three siblings so each piece is small enough to read on one screen:
#
#   ./vm-platform.nix    — qemu/firmware/pkg abstractions, mkScript helper.
#   ./vm-helpers.sh      — bash helper library shared by every script
#                          (`assign_port`, `wait_ssh`,
#                          `provision_identity_key`, `build_iso`,
#                          `compute_vlan_args`, `compute_display_args`).
#                          Loaded once via `builtins.readFile` and
#                          interpolated as `${sharedHelpers}` into each
#                          script body.
#   ./vm-scripts/*.nix   — one Nix file per emitted app. Each takes
#                          `{platform, pkgs}` and returns the flake-app
#                          attr produced by `platform.mkScript`.
{inputs}: {pkgs}: let
  platform = import ./vm-platform.nix {inherit inputs;} {inherit pkgs;};
  scripts = {
    "build-vm" = import ./vm-scripts/build.nix {inherit platform pkgs;};
    "start-vm" = import ./vm-scripts/start.nix {inherit platform pkgs;};
    "stop-vm" = import ./vm-scripts/stop.nix {inherit platform pkgs;};
    "clean-vm" = import ./vm-scripts/clean.nix {inherit platform pkgs;};
    "test-vm" = import ./vm-scripts/test.nix {inherit platform pkgs;};
  };
in
  pkgs.lib.optionalAttrs platform.isLinux scripts
