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

  mkScript = name: description: text: {
    type = "app";
    program = "${pkgs.writeShellScriptBin name text}/bin/${name}";
    meta.description = description;
  };

  nixos-anywhere-bin =
    if inputs.nixos-anywhere.packages ? ${system}
    then "${inputs.nixos-anywhere.packages.${system}.default}/bin/nixos-anywhere"
    else "echo 'nixos-anywhere not available on ${system}'; exit 1";

  qemuBin =
    {
      "x86_64-linux" = "qemu-system-x86_64";
      "aarch64-linux" = "qemu-system-aarch64";
      "aarch64-darwin" = "qemu-system-aarch64";
      "x86_64-darwin" = "qemu-system-x86_64";
    }.${
      system
    } or (throw "unsupported system: ${system}");

  qemuAccel =
    if isLinux
    then "-enable-kvm"
    else if isDarwin
    then "-accel hvf"
    else throw "unsupported system: ${system}";

  basePkgs = with pkgs; [qemu coreutils openssh nix git];

  sharedHelpers = ''
    GREEN='\033[1;32m'
    YELLOW='\033[1;33m'
    RED='\033[1;31m'
    NC='\033[0m'

    VM_DIR="''${XDG_DATA_HOME:-''$HOME/.local/share}/nixfleet/vms"
    SSH_OPTS="-o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o ConnectTimeout=2"

    assign_port() {
      local host="$1"
      if [ -n "''${PORT_OVERRIDE:-}" ]; then
        SSH_PORT="''$PORT_OVERRIDE"
        return
      fi
      local hosts
      hosts=$(nix eval .#nixosConfigurations --apply 'x: builtins.concatStringsSep "\n" (builtins.sort builtins.lessThan (builtins.attrNames x))' --raw 2>/dev/null)
      local idx=0
      while IFS= read -r name; do
        if [ "$name" = "$host" ]; then
          SSH_PORT=$((2201 + idx))
          return
        fi
        idx=$((idx + 1))
      done <<< "$hosts"
      echo -e "''${RED}Host '$host' not found in nixosConfigurations''${NC}" >&2
      exit 1
    }

    wait_ssh() {
      local port="$1" timeout="$2"
      local elapsed=0
      while ! ssh ''$SSH_OPTS -p "$port" root@localhost true 2>/dev/null; do
        sleep 1
        elapsed=$((elapsed + 1))
        if [ "$elapsed" -ge "$timeout" ]; then
          echo -e "''${RED}SSH timeout after ''${timeout}s''${NC}" >&2
          return 1
        fi
      done
      echo -e "''${GREEN}SSH ready (''${elapsed}s)''${NC}"
    }

    provision_identity_key() {
      local host="$1"
      local explicit_key="''${2:-}"
      EXTRA_FILES_DIR=$(mktemp -d)
      EXTRA_FILES_ARGS=""

      local key_src=""
      if [ -n "$explicit_key" ]; then
        if [ ! -f "$explicit_key" ]; then
          echo -e "''${RED}Identity key not found: $explicit_key''${NC}" >&2
          exit 1
        fi
        key_src="$explicit_key"
      elif [ -f "''$HOME/.keys/id_ed25519" ]; then
        key_src="''$HOME/.keys/id_ed25519"
      elif [ -f "''$HOME/.ssh/id_ed25519" ]; then
        key_src="''$HOME/.ssh/id_ed25519"
      fi

      if [ -n "$key_src" ]; then
        local vm_user
        vm_user="$(nix eval ".#nixosConfigurations.''${host}.config.hostSpec.userName" --raw 2>/dev/null || echo "root")"
        for prefix in "persist/home/$vm_user" "home/$vm_user"; do
          mkdir -p "''$EXTRA_FILES_DIR/$prefix/.keys"
          cp "$key_src" "''$EXTRA_FILES_DIR/$prefix/.keys/id_ed25519"
          chmod 600 "''$EXTRA_FILES_DIR/$prefix/.keys/id_ed25519"
        done
        EXTRA_FILES_ARGS="--extra-files ''$EXTRA_FILES_DIR"
        echo -e "''${GREEN}Provisioning identity key for ''$vm_user (from $key_src)''${NC}"
      else
        echo -e "''${YELLOW}No identity key found — secrets requiring host decryption will not work''${NC}"
        echo -e "''${YELLOW}Provide one with --identity-key PATH, or place at ~/.keys/id_ed25519''${NC}"
      fi
    }

    build_iso() {
      echo -e "''${YELLOW}Building custom ISO...''${NC}"
      local iso_path
      iso_path=$(nix build .#iso --no-link --print-out-paths)
      ISO_FILE=$(find "''$iso_path/iso" -name '*.iso' | head -1)
      if [ -z "''$ISO_FILE" ]; then
        echo -e "''${RED}No ISO found in output''${NC}" >&2
        exit 1
      fi
      echo -e "''${GREEN}ISO: ''$ISO_FILE''${NC}"
    }

    all_hosts() {
      nix eval .#nixosConfigurations --apply 'x: builtins.concatStringsSep "\n" (builtins.sort builtins.lessThan (builtins.attrNames x))' --raw 2>/dev/null
    }
  '';
in
  lib.optionalAttrs (isLinux || isDarwin) {
    # ── build-vm (Task 2) ──
    build-vm = mkScript "build-vm" "Install a VM host via nixos-anywhere (ISO boot + disko)" ''
      set -euo pipefail
      export PATH="${lib.makeBinPath basePkgs}:$PATH"

      ${sharedHelpers}

      HOST=""
      ALL=0
      REBUILD=0
      PORT_OVERRIDE=""
      IDENTITY_KEY=""
      RAM=4096
      CPUS=2
      DISK_SIZE="20G"

      while [[ $# -gt 0 ]]; do
        case "$1" in
          -h|--host) HOST="$2"; shift 2 ;;
          --all) ALL=1; shift ;;
          --rebuild) REBUILD=1; shift ;;
          --identity-key) IDENTITY_KEY="$2"; shift 2 ;;
          --ssh-port) PORT_OVERRIDE="$2"; shift 2 ;;
          --ram) RAM="$2"; shift 2 ;;
          --cpus) CPUS="$2"; shift 2 ;;
          --disk-size) DISK_SIZE="$2"; shift 2 ;;
          *) echo "Unknown argument: $1" >&2; exit 1 ;;
        esac
      done

      if [[ $ALL -eq 0 && -z "$HOST" ]]; then
        echo "Usage: nix run .#build-vm -- -h HOST [options]" >&2
        echo "       nix run .#build-vm -- --all [options]" >&2
        echo "" >&2
        echo "Options:" >&2
        echo "  -h HOST            Host to install" >&2
        echo "  --all              Install all hosts in nixosConfigurations" >&2
        echo "  --rebuild          Wipe and reinstall existing disk" >&2
        echo "  --identity-key PATH  Path to identity key for secrets decryption" >&2
        echo "  --ssh-port N       Override SSH port (default: auto-assigned)" >&2
        echo "  --ram MB           RAM in MB (default: 4096)" >&2
        echo "  --cpus N           CPU count (default: 2)" >&2
        echo "  --disk-size S      Disk size (default: 20G)" >&2
        exit 1
      fi

      build_one() {
        local host="$1"
        echo -e "''${YELLOW}==> Building VM for host: $host''${NC}"

        assign_port "$host"
        echo -e "''${GREEN}SSH port: ''$SSH_PORT''${NC}"

        local disk_path="''$VM_DIR/''${host}.qcow2"
        mkdir -p "''$VM_DIR"

        if [[ -f "''$disk_path" && $REBUILD -eq 0 ]]; then
          echo -e "''${YELLOW}Disk already exists at ''$disk_path — skipping (use --rebuild to reinstall)''${NC}"
          return 0
        fi

        if [[ -f "''$disk_path" ]]; then
          echo -e "''${YELLOW}Removing existing disk for rebuild...''${NC}"
          rm -f "''$disk_path"
        fi

        echo -e "''${YELLOW}Creating disk image (''${DISK_SIZE})...''${NC}"
        qemu-img create -f qcow2 "''$disk_path" "''$DISK_SIZE"

        echo -e "''${YELLOW}Booting ISO (headless)...''${NC}"
        ${qemuBin} \
          ${qemuAccel} \
          -m "''$RAM" \
          -smp "''$CPUS" \
          -drive file="''$disk_path",format=qcow2,if=virtio \
          -nic user,model=virtio-net-pci,hostfwd=tcp::"''$SSH_PORT"-:22 \
          -display none -serial null \
          -bios ${pkgs.OVMF.fd}/FV/OVMF.fd \
          -cdrom "''$ISO_FILE" -boot d \
          -daemonize \
          -pidfile "''$VM_DIR/''${host}.pid"

        echo -e "''${YELLOW}Waiting for SSH...''${NC}"
        wait_ssh "''$SSH_PORT" 120

        provision_identity_key "$host" "''${IDENTITY_KEY:-}"

        echo -e "''${YELLOW}Installing via nixos-anywhere...''${NC}"
        ${nixos-anywhere-bin} \
          --flake ".#''${host}" \
          --ssh-port "''$SSH_PORT" \
          --no-reboot \
          ''$EXTRA_FILES_ARGS \
          root@localhost
        [ -n "''${EXTRA_FILES_DIR:-}" ] && rm -rf "''$EXTRA_FILES_DIR"

        echo -e "''${YELLOW}Stopping ISO VM...''${NC}"
        if [[ -f "''$VM_DIR/''${host}.pid" ]]; then
          kill "$(cat "''$VM_DIR/''${host}.pid")" 2>/dev/null || true
          rm -f "''$VM_DIR/''${host}.pid"
        fi

        echo "''$SSH_PORT" > "''$VM_DIR/''${host}.port"
        echo -e "''${GREEN}==> ''${host} installed successfully (port ''$SSH_PORT)''${NC}"
      }

      build_iso

      if [[ $ALL -eq 1 ]]; then
        while IFS= read -r host; do
          [[ -n "$host" ]] && build_one "$host"
        done <<< "$(all_hosts)"
      else
        build_one "$HOST"
      fi
    '';

    # ── start-vm (Task 3) ──
    # ── stop-vm (Task 4) ──
    # ── clean-vm (Task 4) ──
    # ── test-vm (Task 5) ──
  }
  // lib.optionalAttrs isLinux {
    # ── provision (Task 6, Linux-only — nixos-anywhere path: ${nixos-anywhere-bin}) ──
  }
