# mkVmApps — generate VM lifecycle apps for fleet repos.
#
# Usage in fleet flake.nix:
#   apps = nixfleet.lib.mkVmApps { inherit pkgs; };
#
# Returns: { build-vm, start-vm, stop-vm, clean-vm, test-vm, provision }
#
# ── Shared bash helpers (injected into every script via sharedHelpers) ───────
#
# GREEN/YELLOW/RED/NC — ANSI colour codes
# VM_DIR              — ''${XDG_DATA_HOME:-$HOME/.local/share}/nixfleet/vms
# SSH_OPTS            — StrictHostKeyChecking=no, UserKnownHostsFile=/dev/null, ConnectTimeout=2
#
# assign_port HOST
#   Sets SSH_PORT from sorted nixosConfigurations index (base 2201).
#   Honours PORT_OVERRIDE env var.
#
# wait_ssh PORT TIMEOUT_SECONDS
#   Polls SSH until ready, exits 1 on timeout.
#
# provision_identity_key HOST [KEY_PATH]
#   Copies an identity key into a temp dir for nixos-anywhere --extra-files.
#   Resolution order: explicit arg > ~/.keys/id_ed25519 > ~/.ssh/id_ed25519 > skip (warning).
#   Sets: EXTRA_FILES_DIR, EXTRA_FILES_ARGS
#
# build_iso
#   Runs `nix build .#iso`, sets ISO_FILE.
#
# all_hosts
#   Prints sorted nixosConfigurations names, one per line.
#
# ── Platform helpers ─────────────────────────────────────────────────────────
#
# qemuBin    — qemu-system-{x86_64,aarch64} for the current system
# qemuAccel  — -enable-kvm (Linux) | -accel hvf (Darwin)
# basePkgs   — [qemu coreutils openssh nix git]
# mkScript   — name -> description -> bash text -> flake app attrset
# nixos-anywhere-bin — path to nixos-anywhere (Linux only, Task 6)
#
# ─────────────────────────────────────────────────────────────────────────────
{inputs}: {pkgs}: let
  system = pkgs.stdenv.hostPlatform.system;
  isLinux = builtins.elem system ["x86_64-linux" "aarch64-linux"];
  isDarwin = builtins.elem system ["aarch64-darwin" "x86_64-darwin"];
  lib = pkgs.lib;

  # nixos-anywhere binary path — used by provision (Task 6).
  # Defined here to keep inputs in scope; referenced in the provision stub comment below.

  # All tasks reference nixos-anywhere-bin to avoid dead-code removal before Task 6.
in
  lib.optionalAttrs (isLinux || isDarwin) {
    # ── build-vm (Task 2) ──
    # ── start-vm (Task 3) ──
    # ── stop-vm (Task 4) ──
    # ── clean-vm (Task 4) ──
    # ── test-vm (Task 5) ──
  }
  // lib.optionalAttrs isLinux {
    # ── provision (Task 6, Linux-only — nixos-anywhere path: ${_na}) ──
  }
