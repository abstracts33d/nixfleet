{inputs, ...}: {
  perSystem = {
    pkgs,
    system,
    lib,
    ...
  }: let
    isLinux = builtins.elem system ["x86_64-linux" "aarch64-linux"];
    isDarwin = builtins.elem system ["aarch64-darwin" "x86_64-darwin"];
    mkScript = name: text: {
      type = "app";
      program = "${pkgs.writeShellScriptBin name text}/bin/${name}";
    };
    nixos-anywhere-bin =
      if inputs.nixos-anywhere.packages ? ${system}
      then "${inputs.nixos-anywhere.packages.${system}.default}/bin/nixos-anywhere"
      else "echo 'nixos-anywhere not available on ${system}'; exit 1";
  in {
    devShells.default = pkgs.mkShell {
      nativeBuildInputs = with pkgs; [bashInteractive git age];
      shellHook = ''
        export EDITOR=vim
        git config core.hooksPath .githooks 2>/dev/null || true
      '';
    };

    # Dashboard removed — replaced by Rust control plane (control-plane/)

    apps =
      {
        "validate" = mkScript "validate" ''
          set -uo pipefail

          GREEN='\033[1;32m'
          RED='\033[1;31m'
          YELLOW='\033[1;33m'
          NC='\033[0m'

          PASS=0
          FAIL=0
          SKIP=0
          FAST=0
          VM=0

          while [[ ''${#} -gt 0 ]]; do
            case "''${1}" in
              --fast) FAST=1; shift ;;
              --vm) VM=1; shift ;;
              *) echo "Unknown option: ''${1}"; exit 1 ;;
            esac
          done

          check() {
            local name="$1"
            shift
            printf "%-30s" "$name"
            if OUTPUT=$("$@" 2>&1); then
              echo -e "''${GREEN}OK''${NC}"
              PASS=$((PASS + 1))
            else
              echo -e "''${RED}FAIL''${NC}"
              echo "$OUTPUT" | tail -3
              FAIL=$((FAIL + 1))
            fi
          }

          check_eval() {
            local name="$1"
            local attr="$2"
            printf "%-30s" "$name"
            if nix eval "$attr" --apply 'x: x.config.system.build.toplevel.name or "ok"' 2>/dev/null 1>/dev/null; then
              echo -e "''${GREEN}OK (eval)''${NC}"
              PASS=$((PASS + 1))
            else
              echo -e "''${YELLOW}SKIP (cross-platform)''${NC}"
              SKIP=$((SKIP + 1))
            fi
          }

          echo "=== Formatting ==="
          check "nix fmt" nix fmt -- --fail-on-change

          echo ""
          echo "=== Eval Tests ==="
          ${
            if isLinux
            then ''
              for t in eval-hostspec-defaults eval-ssh-hardening eval-org-field-exists eval-org-defaults eval-org-all-hosts eval-secrets-agnostic eval-batch-hosts eval-test-matrix eval-role-defaults eval-username-org-default eval-locale-timezone eval-ssh-authorized eval-theme-defaults eval-password-files eval-extensions-empty; do
                check "$t" nix build ".#checks.${system}.$t" --no-link
              done
            ''
            else ''
              echo -e "''${YELLOW}SKIP (Linux-only checks)''${NC}"
              SKIP=$((SKIP + 1))
            ''
          }

          echo ""
          echo "=== NixOS Test Hosts (build) ==="
          HOSTS=$(nix eval .#nixosConfigurations --apply 'x: builtins.concatStringsSep " " (builtins.attrNames x)' --raw 2>/dev/null)
          for host in $HOSTS; do
            check "$host" nix build ".#nixosConfigurations.$host.config.system.build.toplevel" --no-link
          done

          # Cross-platform hosts (aether, utm) are fleet-specific — removed from framework validate

          ${
            if isLinux
            then ''
              if [ "$VM" = "1" ]; then
                echo ""
                echo "=== VM Integration Tests ==="
                for t in vm-core vm-minimal; do
                  check "$t" nix build ".#checks.${system}.$t" --no-link
                done
              fi
            ''
            else ""
          }

          echo ""
          echo "==================================="
          echo -e "''${GREEN}Passed: $PASS''${NC}  ''${RED}Failed: $FAIL''${NC}  ''${YELLOW}Skipped: $SKIP''${NC}"
          if [ "$FAIL" -gt 0 ]; then exit 1; fi
        '';
        "docs" = mkScript "docs" ''
          set -euo pipefail
          PATH=${lib.makeBinPath [pkgs.mdbook]}:''$PATH

          echo "Starting NixFleet docs at http://localhost:3000"
          echo "  Sections: Guide | Reference | Business"
          echo "Press Ctrl-C to stop"
          cd "$(git rev-parse --show-toplevel)/docs/src"
          mdbook serve --open
        '';
        "install" = mkScript "install" ''
          set -euo pipefail

          GREEN='\033[1;32m'
          YELLOW='\033[1;33m'
          RED='\033[1;31m'
          NC='\033[0m'

          PATH=${lib.makeBinPath (with pkgs; [git openssh nix coreutils])}:''$PATH

          usage() {
            echo "Usage: ''$0 -h <hostname> -u <username> [--target user@host]"
            echo ""
            echo "Options:"
            echo "  -h          Target hostname (must match a host in the flake)"
            echo "  -u          Username (default: current user)"
            echo "  --target    SSH target for NixOS remote install (e.g., root@192.168.1.50)"
            echo "  -p          SSH port (default: 22)"
            echo ""
            echo "Examples:"
            echo "  macOS (local):  nix run .#install -- -h <hostname> -u <username>"
            echo "  NixOS (remote): nix run .#install -- --target root@<ip> -h <hostname> -u <username>"
            echo "  QEMU VM:       nix run .#install -- --target root@localhost -p 2222 -h qemu"
            exit 1
          }

          HOST=""
          USERNAME="''${USERNAME:-$(whoami)}"
          TARGET=""
          SSH_PORT="22"
          FLAKE_DIR=""

          # Parse args
          while [[ ''$# -gt 0 ]]; do
            case "''$1" in
              -h) HOST="''$2"; shift 2 ;;
              -u) USERNAME="''$2"; shift 2 ;;
              -p) SSH_PORT="''$2"; shift 2 ;;
              --target) TARGET="''$2"; shift 2 ;;
              *) usage ;;
            esac
          done

          [ -z "''$HOST" ] && echo -e "''${RED}Error: -h <hostname> is required''${NC}" && usage

          # Find the flake directory
          if [ -f flake.nix ]; then
            FLAKE_DIR="."
          elif [ -f "''$HOME/fleet/flake.nix" ]; then
            FLAKE_DIR="''$HOME/fleet"
          else
            echo -e "''${YELLOW}Cloning fleet repo...''${NC}"
            CLONE_URL="''$(git remote get-url origin 2>/dev/null || { echo "Error: not in a git repo. Clone your fleet repo first."; exit 1; })"
            git clone "''$CLONE_URL" "''$HOME/fleet"
            FLAKE_DIR="''$HOME/fleet"
          fi

          #
          # -- NixOS Remote Install (via nixos-anywhere) --
          #
          if [ -n "''$TARGET" ]; then
            echo -e "''${YELLOW}NixOS remote install to ''$TARGET as host ''$HOST...''${NC}"

            # Check SSH agent has keys
            if ! ssh-add -l &>/dev/null; then
              echo -e "''${RED}Error: No SSH keys in agent. Run: ssh-add ~/.ssh/id_ed25519''${NC}"
              exit 1
            fi

            # Check SSH connectivity
            echo -e "''${YELLOW}Testing SSH to ''$TARGET (port ''$SSH_PORT)...''${NC}"
            # TOFU: accepts new host keys on first connect — inherent to provisioning fresh installs
            if ! ssh -p "''$SSH_PORT" -o ConnectTimeout=5 -o StrictHostKeyChecking=accept-new "''$TARGET" true 2>/dev/null; then
              echo -e "''${RED}Error: Cannot SSH to ''$TARGET on port ''$SSH_PORT''${NC}"
              echo "Ensure the target is reachable and SSH is running."
              echo "If booting from ISO: set a root password with 'passwd' on the target."
              exit 1
            fi

            echo -e "''${GREEN}SSH OK. Starting nixos-anywhere...''${NC}"
            SSH_PORT_ARGS=""
            if [ "''$SSH_PORT" != "22" ]; then
              SSH_PORT_ARGS="--ssh-port ''$SSH_PORT"
            fi

            # Prepare extra-files: provision agenix decryption key
            EXTRA_FILES=$(mktemp -d)
            EXTRA_FILES_ARGS=""
            KEY_SRC="''$HOME/.keys/id_ed25519"
            if [ ! -f "''$KEY_SRC" ]; then
              KEY_SRC="''$HOME/.ssh/id_ed25519"
            fi
            if [ -f "''$KEY_SRC" ]; then
              KEY_DEST="''$EXTRA_FILES/persist/home/''$USERNAME/.keys"
              mkdir -p "''$KEY_DEST"
              cp "''$KEY_SRC" "''$KEY_DEST/id_ed25519"
              chmod 600 "''$KEY_DEST/id_ed25519"
              EXTRA_FILES_ARGS="--extra-files ''$EXTRA_FILES"
              echo -e "''${GREEN}Provisioning agenix decryption key from ''$KEY_SRC''${NC}"
            else
              echo -e "''${YELLOW}Warning: No decryption key found at ~/.keys/id_ed25519 or ~/.ssh/id_ed25519''${NC}"
              echo -e "''${YELLOW}Secrets (agenix) will not be available after install.''${NC}"
            fi

            # Build on remote if cross-compiling (e.g. Darwin -> Linux)
            BUILD_ARGS=""
            if [ "$(uname)" = "Darwin" ]; then
              BUILD_ARGS="--build-on-remote"
              echo -e "''${YELLOW}Building on remote (cross-platform install)...''${NC}"
            fi

            ${nixos-anywhere-bin} \
              --flake "''$FLAKE_DIR#''$HOST" \
              ''$SSH_PORT_ARGS \
              ''$EXTRA_FILES_ARGS \
              ''$BUILD_ARGS \
              "''$TARGET"

            rm -rf "''$EXTRA_FILES"

            echo -e "''${GREEN}NixOS installation complete!''${NC}"
            echo -e "''${YELLOW}After reboot:''${NC}"
            echo "  1. SSH in: ssh ''$USERNAME@<ip>"
            echo "  2. Set user password: sudo passwd ''$USERNAME"
            exit 0
          fi

          #
          # -- macOS Local Install --
          #
          if [ "''$(uname)" = "Darwin" ]; then
            echo -e "''${YELLOW}macOS install for host ''$HOST...''${NC}"

            # Key bootstrap
            if [ ! -f "''$HOME/.ssh/id_ed25519" ]; then
              echo -e "''${YELLOW}SSH key not found at ~/.ssh/id_ed25519''${NC}"
              echo -n "Enter path to your ed25519 key: "
              read -r KEY_PATH
              if [ ! -f "''$KEY_PATH" ]; then
                echo -e "''${RED}Error: File not found: ''$KEY_PATH''${NC}"
                exit 1
              fi
              mkdir -p "''$HOME/.ssh"
              cp "''$KEY_PATH" "''$HOME/.ssh/id_ed25519"
              chmod 600 "''$HOME/.ssh/id_ed25519"
              echo -e "''${GREEN}Key copied to ~/.ssh/id_ed25519''${NC}"
            fi

            # Load key into agent
            ssh-add -l &>/dev/null || ssh-add "''$HOME/.ssh/id_ed25519" 2>/dev/null || true

            # Test GitHub access
            echo -e "''${YELLOW}Testing GitHub access...''${NC}"
            if ! ssh -T git@github.com 2>&1 | grep -q "successfully authenticated"; then
              echo -e "''${RED}Error: Cannot authenticate to GitHub.''${NC}"
              echo "Ensure ~/.ssh/id_ed25519 has access to the secrets repo."
              exit 1
            fi
            echo -e "''${GREEN}GitHub access OK.''${NC}"

            # Set hostname
            echo -e "''${YELLOW}Setting hostname to ''$HOST...''${NC}"
            sudo scutil --set LocalHostName "''$HOST"
            sudo scutil --set HostName "''$HOST"
            sudo scutil --set ComputerName "''$HOST"

            # Handle /etc/ conflicts
            echo -e "''${YELLOW}Backing up /etc/ files that nix-darwin may overwrite...''${NC}"
            for f in /etc/nix/nix.conf /etc/bashrc /etc/zshrc; do
              if [ -f "''$f" ] && [ ! -f "''$f.before-nix-darwin" ]; then
                sudo mv "''$f" "''$f.before-nix-darwin"
                echo "  Backed up ''$f"
              fi
            done

            cd "''$FLAKE_DIR"
            git add -A 2>/dev/null || true

            # Build
            echo -e "''${YELLOW}Building configuration...''${NC}"
            export NIXPKGS_ALLOW_UNFREE=1
            nix --extra-experimental-features 'nix-command flakes' build ".#darwinConfigurations.''$HOST.system"

            # Switch
            echo -e "''${YELLOW}Switching to new generation...''${NC}"
            sudo ./result/sw/bin/darwin-rebuild switch --flake ".#''$HOST"
            unlink ./result

            echo -e "''${GREEN}macOS installation complete!''${NC}"
            exit 0
          fi

          echo -e "''${RED}Error: Not on macOS and no --target specified.''${NC}"
          echo "For NixOS install, use: --target root@<ip>"
          usage
        '';
      }
      // lib.optionalAttrs isLinux {
        "spawn-qemu" = mkScript "spawn-qemu" ''
          set -euo pipefail

          GREEN='\033[1;32m'
          YELLOW='\033[1;33m'
          RED='\033[1;31m'
          NC='\033[0m'

          PATH=${lib.makeBinPath (with pkgs; [qemu coreutils openssh virt-viewer])}:''$PATH
          export LIBGL_DRIVERS_PATH="${pkgs.mesa}/lib/dri"
          export __EGL_VENDOR_LIBRARY_DIRS="${pkgs.mesa}/share/glvnd/egl_vendor.d"

          DISK="qemu-disk.qcow2"
          ISO=""
          RAM="4096"
          CPUS="2"
          SSH_PORT="2222"
          DISK_SIZE="20G"
          MODE="graphical"

          usage() {
            echo "Usage: nix run .#spawn-qemu [-- [options]]"
            echo ""
            echo "Options:"
            echo "  --iso PATH      Boot from ISO (for initial install)"
            echo "  --disk PATH     Disk image path (default: qemu-disk.qcow2)"
            echo "  --ram MB        RAM in MB (default: 4096)"
            echo "  --cpus N        CPU count (default: 2)"
            echo "  --ssh-port N    Host port for SSH forwarding (default: 2222)"
            echo "  --disk-size S   Disk size for new images (default: 20G)"
            echo "  --console       Headless mode (serial console, no GUI window)"
            echo "  --graphical     GPU-accelerated GUI via SPICE (default)"
            echo ""
            echo "Examples:"
            echo "  First boot (install): nix run .#spawn-qemu -- --iso iso/nixos-x86_64.iso"
            echo "  Headless install:     nix run .#spawn-qemu -- --console --iso nixos-x86_64.iso"
            echo "  After install:        nix run .#spawn-qemu"
            echo "  SSH into VM:          ssh -p 2222 root@localhost"
            exit 0
          }

          while [[ ''$# -gt 0 ]]; do
            case "''$1" in
              --iso) ISO="''$2"; shift 2 ;;
              --disk) DISK="''$2"; shift 2 ;;
              --ram) RAM="''$2"; shift 2 ;;
              --cpus) CPUS="''$2"; shift 2 ;;
              --ssh-port) SSH_PORT="''$2"; shift 2 ;;
              --disk-size) DISK_SIZE="''$2"; shift 2 ;;
              --console) MODE="console"; shift ;;
              --graphical) MODE="graphical"; shift ;;
              --help|-h) usage ;;
              *) echo -e "''${RED}Unknown option: ''$1''${NC}"; usage ;;
            esac
          done

          if [ ! -f "''$DISK" ]; then
            echo -e "''${YELLOW}Creating disk image: ''$DISK (''$DISK_SIZE)...''${NC}"
            qemu-img create -f qcow2 "''$DISK" "''$DISK_SIZE"
          fi

          BOOT_ARGS=""
          if [ -n "''$ISO" ]; then
            if [ ! -f "''$ISO" ]; then
              echo -e "''${RED}Error: ISO not found: ''$ISO''${NC}"
              exit 1
            fi
            BOOT_ARGS="-cdrom ''$ISO -boot d"
            echo -e "''${YELLOW}Booting from ISO: ''$ISO''${NC}"
          else
            echo -e "''${YELLOW}Booting from disk: ''$DISK''${NC}"
          fi

          echo -e "''${GREEN}VM: ''${CPUS} CPUs, ''${RAM}MB RAM, SSH on localhost:''${SSH_PORT} (''${MODE})''${NC}"

          DISPLAY_ARGS=""
          CLEANUP=""
          if [ "''$MODE" = "console" ]; then
            DISPLAY_ARGS="-nographic"
            echo -e "''${YELLOW}Press Ctrl-A X to exit QEMU''${NC}"
          else
            # SPICE without auth — acceptable for local dev VMs (localhost only)
            DISPLAY_ARGS="-device virtio-vga-gl -display egl-headless,rendernode=/dev/dri/renderD128 -spice port=5900,disable-ticketing=on"

            # On non-NixOS, QEMU needs /run/opengl-driver for GBM drivers.
            if [ ! -d /run/opengl-driver/lib/gbm ]; then
              if ! sudo -n true 2>/dev/null; then
                echo -e "''${RED}Graphical mode requires sudo to set up OpenGL drivers in /run/opengl-driver.''${NC}"
                echo -e "''${YELLOW}Options:''${NC}"
                echo "  1. Run again with sudo available (e.g. enter password when prompted)"
                echo "  2. Use --console for headless mode: nix run .#spawn-qemu -- --console"
                echo ""
              fi
              sudo mkdir -p /run/opengl-driver/lib
              sudo ln -sf ${pkgs.mesa}/lib/gbm /run/opengl-driver/lib/gbm
              CLEANUP="sudo rm -rf /run/opengl-driver"
            fi

            # Auto-launch SPICE viewer
            (sleep 3 && remote-viewer spice://localhost:5900 2>/dev/null) &
            VIEWER_PID=''$!
            trap "kill ''$VIEWER_PID 2>/dev/null; ''$CLEANUP" EXIT
          fi

          qemu-system-x86_64 \
            -enable-kvm \
            -m "''$RAM" \
            -smp "''$CPUS" \
            -drive file="''$DISK",format=qcow2,if=virtio \
            -nic user,model=virtio-net-pci,hostfwd=tcp::''${SSH_PORT}-:22 \
            ''$DISPLAY_ARGS \
            -bios ${pkgs.OVMF.fd}/FV/OVMF.fd \
            ''$BOOT_ARGS
        '';
        "test-vm" = mkScript "test-vm" ''
          set -euo pipefail

          GREEN='\033[1;32m'
          YELLOW='\033[1;33m'
          RED='\033[1;31m'
          NC='\033[0m'

          PATH=${lib.makeBinPath (with pkgs; [qemu coreutils openssh nix git])}:''$PATH

          HOST="qemu"
          KEEP=0
          SSH_PORT="2222"
          RAM="4096"
          CPUS="2"

          usage() {
            echo "Usage: nix run .#test-vm [-- [options]]"
            echo ""
            echo "Automated VM test cycle: build ISO -> boot -> install -> verify -> cleanup"
            echo ""
            echo "Options:"
            echo "  -h HOST        Host config to install (default: qemu)"
            echo "  --keep         Keep temp dir and disk after test"
            echo "  --ssh-port N   Host port for SSH (default: 2222)"
            echo "  --ram MB       RAM in MB (default: 4096)"
            echo "  --cpus N       CPU count (default: 2)"
            echo "  --help         Show this help"
            echo ""
            echo "Examples:"
            echo "  nix run .#test-vm                          # test with 'qemu' host"
            echo "  nix run .#test-vm -- -h krach-qemu         # test with krach-qemu host"
            echo "  nix run .#test-vm -- -h qemu --keep        # keep disk for inspection"
            exit 0
          }

          while [[ ''$# -gt 0 ]]; do
            case "''$1" in
              -h) HOST="''$2"; shift 2 ;;
              --keep) KEEP=1; shift ;;
              --ssh-port) SSH_PORT="''$2"; shift 2 ;;
              --ram) RAM="''$2"; shift 2 ;;
              --cpus) CPUS="''$2"; shift 2 ;;
              --help) usage ;;
              *) echo -e "''${RED}Unknown option: ''$1''${NC}"; usage ;;
            esac
          done

          TMPDIR=$(mktemp -d -t test-vm-XXXXXX)
          DISK="''$TMPDIR/disk.qcow2"
          PIDFILE="''$TMPDIR/qemu.pid"
          SSH_OPTS="-o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o ConnectTimeout=2"

          cleanup() {
            echo -e "''${YELLOW}Cleaning up...''${NC}"
            if [ -f "''$PIDFILE" ]; then
              kill "$(cat "''$PIDFILE")" 2>/dev/null || true
              rm -f "''$PIDFILE"
            fi
            if [ "''$KEEP" = "0" ]; then
              rm -rf "''$TMPDIR"
              echo -e "''${GREEN}Temp dir removed.''${NC}"
            else
              echo -e "''${YELLOW}Kept temp dir: ''$TMPDIR''${NC}"
            fi
          }
          trap cleanup EXIT

          # --- Step 1: Build ISO ---
          echo -e "''${YELLOW}[1/6] Building custom ISO...''${NC}"
          ISO_PATH=$(nix build .#iso --no-link --print-out-paths)
          ISO_FILE=$(find "''$ISO_PATH/iso" -name '*.iso' | head -1)
          if [ -z "''$ISO_FILE" ]; then
            echo -e "''${RED}Error: No ISO file found in ''$ISO_PATH''${NC}"
            exit 1
          fi
          echo -e "''${GREEN}ISO: ''$ISO_FILE''${NC}"

          # --- Step 2: Create ephemeral disk ---
          echo -e "''${YELLOW}[2/6] Creating ephemeral disk...''${NC}"
          qemu-img create -f qcow2 "''$DISK" 20G

          # --- Step 3: Boot QEMU with ISO ---
          echo -e "''${YELLOW}[3/6] Booting QEMU (ISO install)...''${NC}"
          qemu-system-x86_64 \
            -enable-kvm \
            -m "''$RAM" \
            -smp "''$CPUS" \
            -drive file="''$DISK",format=qcow2,if=virtio \
            -nic user,model=virtio-net-pci,hostfwd=tcp::''${SSH_PORT}-:22 \
            -display none \
            -serial null \
            -bios ${pkgs.OVMF.fd}/FV/OVMF.fd \
            -cdrom "''$ISO_FILE" \
            -boot d \
            -daemonize \
            -pidfile "''$PIDFILE"

          echo -e "''${YELLOW}Waiting for SSH (timeout 120s)...''${NC}"
          ELAPSED=0
          while ! ssh ''$SSH_OPTS -p "''$SSH_PORT" root@localhost true 2>/dev/null; do
            sleep 1
            ELAPSED=$((ELAPSED + 1))
            if [ "''$ELAPSED" -ge 120 ]; then
              echo -e "''${RED}Error: SSH timeout after 120s''${NC}"
              exit 1
            fi
          done
          echo -e "''${GREEN}SSH ready (''${ELAPSED}s)''${NC}"

          # --- Step 4: Run nixos-anywhere ---
          # Provision agenix decryption key
          EXTRA_FILES=$(mktemp -d)
          EXTRA_FILES_ARGS=""
          KEY_SRC="''$HOME/.keys/id_ed25519"
          [ ! -f "''$KEY_SRC" ] && KEY_SRC="''$HOME/.ssh/id_ed25519"
          if [ -f "''$KEY_SRC" ]; then
            VM_USER="''$(nix eval ".#nixosConfigurations.''$HOST.config.hostSpec.userName" --raw 2>/dev/null || echo "root")"
            for prefix in "persist/home/''$VM_USER" "home/''$VM_USER"; do
              mkdir -p "''$EXTRA_FILES/''$prefix/.keys"
              cp "''$KEY_SRC" "''$EXTRA_FILES/''$prefix/.keys/id_ed25519"
              chmod 600 "''$EXTRA_FILES/''$prefix/.keys/id_ed25519"
            done
            EXTRA_FILES_ARGS="--extra-files ''$EXTRA_FILES"
            echo -e "''${GREEN}Provisioning agenix key for ''$VM_USER''${NC}"
          fi

          echo -e "''${YELLOW}[4/6] Running nixos-anywhere (host: ''$HOST)...''${NC}"
          ${nixos-anywhere-bin} \
            --flake ".#''$HOST" \
            --ssh-port "''$SSH_PORT" \
            --no-reboot \
            ''$EXTRA_FILES_ARGS \
            root@localhost
          rm -rf "''$EXTRA_FILES"

          # --- Step 5: Reboot from disk ---
          echo -e "''${YELLOW}[5/6] Rebooting from disk...''${NC}"
          kill "$(cat "''$PIDFILE")" 2>/dev/null || true
          sleep 2

          qemu-system-x86_64 \
            -enable-kvm \
            -m "''$RAM" \
            -smp "''$CPUS" \
            -drive file="''$DISK",format=qcow2,if=virtio \
            -nic user,model=virtio-net-pci,hostfwd=tcp::''${SSH_PORT}-:22 \
            -display none \
            -serial null \
            -bios ${pkgs.OVMF.fd}/FV/OVMF.fd \
            -daemonize \
            -pidfile "''$PIDFILE"

          echo -e "''${YELLOW}Waiting for SSH after install (timeout 180s)...''${NC}"
          ELAPSED=0
          while ! ssh ''$SSH_OPTS -p "''$SSH_PORT" root@localhost true 2>/dev/null; do
            sleep 1
            ELAPSED=$((ELAPSED + 1))
            if [ "''$ELAPSED" -ge 180 ]; then
              echo -e "''${RED}Error: SSH timeout after install (180s)''${NC}"
              exit 1
            fi
          done
          echo -e "''${GREEN}SSH ready after install (''${ELAPSED}s)''${NC}"

          # --- Step 6: Verify ---
          echo -e "''${YELLOW}[6/6] Verifying installation...''${NC}"
          FAILURES=0

          VM_HOSTNAME=$(ssh ''$SSH_OPTS -p "''$SSH_PORT" root@localhost hostname 2>/dev/null)
          if [ "''$VM_HOSTNAME" = "''$HOST" ]; then
            echo -e "  hostname: ''${GREEN}OK''${NC} (''$VM_HOSTNAME)"
          else
            echo -e "  hostname: ''${RED}FAIL''${NC} (expected ''$HOST, got ''$VM_HOSTNAME)"
            FAILURES=$((FAILURES + 1))
          fi

          MULTI_USER=$(ssh ''$SSH_OPTS -p "''$SSH_PORT" root@localhost systemctl is-active multi-user.target 2>/dev/null)
          if [ "''$MULTI_USER" = "active" ]; then
            echo -e "  multi-user.target: ''${GREEN}OK''${NC}"
          else
            echo -e "  multi-user.target: ''${RED}FAIL''${NC} (''$MULTI_USER)"
            FAILURES=$((FAILURES + 1))
          fi

          SSHD=$(ssh ''$SSH_OPTS -p "''$SSH_PORT" root@localhost systemctl is-active sshd 2>/dev/null)
          if [ "''$SSHD" = "active" ]; then
            echo -e "  sshd: ''${GREEN}OK''${NC}"
          else
            echo -e "  sshd: ''${RED}FAIL''${NC} (''$SSHD)"
            FAILURES=$((FAILURES + 1))
          fi

          if [ "''$FAILURES" -gt 0 ]; then
            echo -e "''${RED}Verification failed (''$FAILURES errors)''${NC}"
            exit 1
          fi

          echo -e "''${GREEN}All checks passed! VM test successful.''${NC}"
        '';
        "launch-vm" = mkScript "launch-vm" ''
          set -euo pipefail

          GREEN='\033[1;32m'
          YELLOW='\033[1;33m'
          RED='\033[1;31m'
          NC='\033[0m'

          PATH=${lib.makeBinPath (with pkgs; [qemu coreutils openssh nix git virt-viewer])}:''$PATH
          export LIBGL_DRIVERS_PATH="${pkgs.mesa}/lib/dri"
          export __EGL_VENDOR_LIBRARY_DIRS="${pkgs.mesa}/share/glvnd/egl_vendor.d"

          HOST="krach-qemu"
          RAM="4096"
          CPUS="2"
          SSH_PORT="2222"
          DISK_SIZE="20G"
          DISK_DIR="''${XDG_DATA_HOME:-''$HOME/.local/share}/nixfleet/vms"

          usage() {
            echo "Usage: nix run .#launch-vm [-- [options]]"
            echo ""
            echo "Build, install, and launch a graphical VM for visual verification."
            echo "Persistent disk — survives reboots. Rebuild with --rebuild."
            echo ""
            echo "Options:"
            echo "  -h HOST        Host config (default: krach-qemu)"
            echo "  --rebuild      Wipe disk and reinstall from scratch"
            echo "  --ram MB       RAM in MB (default: 4096)"
            echo "  --cpus N       CPU count (default: 2)"
            echo "  --ssh-port N   SSH port (default: 2222)"
            echo "  --help         Show this help"
            echo ""
            echo "Examples:"
            echo "  nix run .#launch-vm                          # launch krach-qemu"
            echo "  nix run .#launch-vm -- -h ohm                # launch ohm config"
            echo "  nix run .#launch-vm -- --rebuild              # fresh install"
            exit 0
          }

          REBUILD=0
          while [[ ''$# -gt 0 ]]; do
            case "''$1" in
              -h) HOST="''$2"; shift 2 ;;
              --rebuild) REBUILD=1; shift ;;
              --ram) RAM="''$2"; shift 2 ;;
              --cpus) CPUS="''$2"; shift 2 ;;
              --ssh-port) SSH_PORT="''$2"; shift 2 ;;
              --help) usage ;;
              *) echo -e "''${RED}Unknown option: ''$1''${NC}"; usage ;;
            esac
          done

          DISK="''$DISK_DIR/''$HOST.qcow2"
          mkdir -p "''$DISK_DIR"

          SSH_OPTS="-o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o ConnectTimeout=2"

          # Setup OpenGL for SPICE on non-NixOS hosts
          setup_gl() {
            if [ ! -d /run/opengl-driver/lib/gbm ]; then
              echo -e "''${YELLOW}Setting up OpenGL drivers (requires sudo)...''${NC}"
              sudo mkdir -p /run/opengl-driver/lib
              sudo ln -sf ${pkgs.mesa}/lib/gbm /run/opengl-driver/lib/gbm
            fi
          }

          # If disk exists and no rebuild, just boot graphically
          if [ -f "''$DISK" ] && [ "''$REBUILD" = "0" ]; then
            echo -e "''${GREEN}Booting existing VM: ''$HOST (''$DISK)''${NC}"
            echo -e "''${GREEN}VM: ''${CPUS} CPUs, ''${RAM}MB RAM, SSH on localhost:''${SSH_PORT}''${NC}"
            setup_gl
            (sleep 3 && remote-viewer spice://localhost:5900 2>/dev/null) &
            exec qemu-system-x86_64 \
              -enable-kvm \
              -m "''$RAM" \
              -smp "''$CPUS" \
              -drive file="''$DISK",format=qcow2,if=virtio \
              -nic user,model=virtio-net-pci,hostfwd=tcp::''${SSH_PORT}-:22 \
              -device virtio-vga-gl -display egl-headless,rendernode=/dev/dri/renderD128 \
              -spice port=5900,disable-ticketing=on \
              -bios ${pkgs.OVMF.fd}/FV/OVMF.fd
          fi

          # Fresh install: build ISO, install headless, then reboot graphically
          echo -e "''${YELLOW}[1/5] Building custom ISO...''${NC}"
          ISO_PATH=$(nix build .#iso --no-link --print-out-paths)
          ISO_FILE=$(find "''$ISO_PATH/iso" -name '*.iso' | head -1)
          if [ -z "''$ISO_FILE" ]; then
            echo -e "''${RED}Error: No ISO file found''${NC}"
            exit 1
          fi
          echo -e "''${GREEN}ISO: ''$ISO_FILE''${NC}"

          echo -e "''${YELLOW}[2/5] Creating disk: ''$DISK (''$DISK_SIZE)...''${NC}"
          rm -f "''$DISK"
          qemu-img create -f qcow2 "''$DISK" "''$DISK_SIZE"

          echo -e "''${YELLOW}[3/5] Booting from ISO (headless install)...''${NC}"
          PIDFILE="''$(mktemp)"
          qemu-system-x86_64 \
            -enable-kvm \
            -m "''$RAM" \
            -smp "''$CPUS" \
            -drive file="''$DISK",format=qcow2,if=virtio \
            -nic user,model=virtio-net-pci,hostfwd=tcp::''${SSH_PORT}-:22 \
            -display none \
            -serial null \
            -bios ${pkgs.OVMF.fd}/FV/OVMF.fd \
            -cdrom "''$ISO_FILE" \
            -boot d \
            -daemonize \
            -pidfile "''$PIDFILE"

          echo -e "''${YELLOW}Waiting for SSH (timeout 120s)...''${NC}"
          ELAPSED=0
          while ! ssh ''$SSH_OPTS -p "''$SSH_PORT" root@localhost true 2>/dev/null; do
            sleep 1
            ELAPSED=$((ELAPSED + 1))
            if [ "''$ELAPSED" -ge 120 ]; then
              echo -e "''${RED}Error: SSH timeout after 120s''${NC}"
              kill "$(cat "''$PIDFILE")" 2>/dev/null || true
              exit 1
            fi
          done
          echo -e "''${GREEN}SSH ready (''${ELAPSED}s)''${NC}"

          # Provision agenix decryption key for the VM
          EXTRA_FILES=$(mktemp -d)
          EXTRA_FILES_ARGS=""
          KEY_SRC="''$HOME/.keys/id_ed25519"
          if [ ! -f "''$KEY_SRC" ]; then
            KEY_SRC="''$HOME/.ssh/id_ed25519"
          fi
          if [ -f "''$KEY_SRC" ]; then
            # Detect username from fleet.nix org defaults
            VM_USER="''$(nix eval ".#nixosConfigurations.''$HOST.config.hostSpec.userName" --raw 2>/dev/null || echo "root")"
            KEY_DEST="''$EXTRA_FILES/persist/home/''$VM_USER/.keys"
            mkdir -p "''$KEY_DEST"
            cp "''$KEY_SRC" "''$KEY_DEST/id_ed25519"
            chmod 600 "''$KEY_DEST/id_ed25519"
            # Also put in non-persist path for non-impermanent hosts
            KEY_DEST2="''$EXTRA_FILES/home/''$VM_USER/.keys"
            mkdir -p "''$KEY_DEST2"
            cp "''$KEY_SRC" "''$KEY_DEST2/id_ed25519"
            chmod 600 "''$KEY_DEST2/id_ed25519"
            EXTRA_FILES_ARGS="--extra-files ''$EXTRA_FILES"
            echo -e "''${GREEN}Provisioning agenix key for ''$VM_USER from ''$KEY_SRC''${NC}"
          else
            echo -e "''${YELLOW}Warning: No decryption key found. Secrets will not work in VM.''${NC}"
          fi

          echo -e "''${YELLOW}[4/5] Installing ''$HOST via nixos-anywhere...''${NC}"
          ${nixos-anywhere-bin} \
            --flake ".#''$HOST" \
            --ssh-port "''$SSH_PORT" \
            --no-reboot \
            ''$EXTRA_FILES_ARGS \
            root@localhost

          rm -rf "''$EXTRA_FILES"

          kill "$(cat "''$PIDFILE")" 2>/dev/null || true
          rm -f "''$PIDFILE"
          sleep 2

          echo -e "''${YELLOW}[5/5] Launching graphical VM...''${NC}"
          setup_gl
          echo -e "''${GREEN}VM: ''${CPUS} CPUs, ''${RAM}MB RAM, SSH on localhost:''${SSH_PORT}''${NC}"
          echo -e "''${GREEN}SPICE viewer will open automatically.''${NC}"
          (sleep 3 && remote-viewer spice://localhost:5900 2>/dev/null) &
          exec qemu-system-x86_64 \
            -enable-kvm \
            -m "''$RAM" \
            -smp "''$CPUS" \
            -drive file="''$DISK",format=qcow2,if=virtio \
            -nic user,model=virtio-net-pci,hostfwd=tcp::''${SSH_PORT}-:22 \
            -device virtio-vga-gl -display egl-headless,rendernode=/dev/dri/renderD128 \
            -spice port=5900,disable-ticketing=on \
            -bios ${pkgs.OVMF.fd}/FV/OVMF.fd
        '';
        "build-switch" = mkScript "build-switch" ''
          set -e
          PATH=${pkgs.git}/bin:$PATH
          HOST=$(${pkgs.hostname}/bin/hostname)
          echo -e '\033[1;33mStarting...\033[0m'
          sudo /run/current-system/sw/bin/nixos-rebuild switch --flake .#$HOST "$@"
          echo -e '\033[1;32mSwitch to new generation complete!\033[0m'
        '';
      }
      // lib.optionalAttrs isDarwin {
        "spawn-utm" = mkScript "spawn-utm" ''
          set -euo pipefail

          GREEN='\033[1;32m'
          YELLOW='\033[1;33m'
          RED='\033[1;31m'
          NC='\033[0m'

          PATH=${lib.makeBinPath (with pkgs; [coreutils openssh])}:''$PATH

          VM_NAME="nixos"
          HOST=""
          ACTION="setup"

          usage() {
            echo "Usage: nix run .#spawn-utm [-- [options]]"
            echo ""
            echo "Options:"
            echo "  --name NAME     VM name in UTM (default: nixos)"
            echo "  --host NAME     NixOS host config to install (required)"
            echo "  --start         Start existing VM and show IP"
            echo "  --ip            Show IP of running VM"
            echo ""
            echo "Setup flow:"
            echo "  1. nix run .#spawn-utm                    # shows setup guide"
            echo "  2. Create VM in UTM, boot ISO, passwd"
            echo "  3. nix run .#spawn-utm -- --ip            # get VM IP"
            echo "  4. nix run .#install -- --target root@<ip> -h krach-utm"
            echo ""
            echo "Download ISO:"
            echo "  curl -L -o iso/nixos-aarch64.iso https://channels.nixos.org/nixos-unstable/latest-nixos-minimal-aarch64-linux.iso"
            exit 0
          }

          while [[ ''$# -gt 0 ]]; do
            case "''$1" in
              --name) VM_NAME="''$2"; shift 2 ;;
              --host) HOST="''$2"; shift 2 ;;
              --start) ACTION="start"; shift ;;
              --ip) ACTION="ip"; shift ;;
              --help|-h) usage ;;
              *) echo -e "''${RED}Unknown option: ''$1''${NC}"; usage ;;
            esac
          done

          if [ -z "''$HOST" ] && [ "''$ACTION" != "ip" ]; then
            echo -e "''${RED}Error: --host is required''${NC}"
            usage
          fi

          UTMCTL="/Applications/UTM.app/Contents/MacOS/utmctl"
          if [ ! -x "''$UTMCTL" ]; then
            echo -e "''${RED}Error: UTM not found. Install from https://mac.getutm.app''${NC}"
            exit 1
          fi

          get_ip() {
            ''$UTMCTL ip-address "''$VM_NAME" 2>/dev/null | head -1
          }

          case "''$ACTION" in
            ip)
              IP=$(get_ip)
              if [ -n "''$IP" ]; then
                echo "''$IP"
              else
                echo -e "''${RED}VM ''$VM_NAME not running or IP not available''${NC}"
                exit 1
              fi
              ;;
            start)
              ''$UTMCTL start "''$VM_NAME" 2>/dev/null || true
              echo -e "''${YELLOW}Waiting for IP...''${NC}"
              for i in $(seq 1 30); do
                IP=$(get_ip)
                if [ -n "''$IP" ]; then
                  echo -e "''${GREEN}VM running at ''$IP''${NC}"
                  echo "SSH: ssh root@''$IP"
                  echo "Install: nix run .#install -- --target root@''$IP -h ''$HOST"
                  exit 0
                fi
                sleep 2
              done
              echo -e "''${RED}Could not detect IP. Check UTM window.''${NC}"
              ;;
            setup)
              echo -e "''${GREEN}UTM VM Setup Guide''${NC}"
              echo ""
              echo -e "''${YELLOW}1. Download the ISO (if not done):''${NC}"
              echo "   curl -L -o iso/nixos-aarch64.iso https://channels.nixos.org/nixos-unstable/latest-nixos-minimal-aarch64-linux.iso"
              echo ""
              echo -e "''${YELLOW}2. Create VM in UTM:''${NC}"
              echo "   - Open UTM > Create New VM > Virtualize > Linux"
              echo "   - Boot ISO: select iso/nixos-aarch64.iso"
              echo "   - RAM: 4096 MB, CPUs: 2, Disk: 64 GB"
              echo "   - Network: Shared Network"
              echo "   - Name: ''$VM_NAME"
              echo ""
              echo -e "''${YELLOW}3. Boot and prepare:''${NC}"
              echo "   - Start the VM in UTM"
              echo "   - In the VM console: passwd"
              echo ""
              echo -e "''${YELLOW}4. Get VM IP and install:''${NC}"
              echo "   nix run .#spawn-utm -- --ip"
              echo "   nix run .#install -- --target root@<ip> -h ''$HOST"
              echo ""
              echo -e "''${YELLOW}5. After install:''${NC}"
              echo "   - Stop VM in UTM"
              echo "   - Edit VM > remove ISO drive"
              echo "   - Start VM"
              ;;
          esac
        '';
        "build-switch" = mkScript "build-switch" ''
          set -e
          PATH=${pkgs.git}/bin:$PATH
          HOST=$(${pkgs.hostname}/bin/hostname)
          FLAKE_SYSTEM="darwinConfigurations.''${HOST}.system"
          export NIXPKGS_ALLOW_UNFREE=1
          echo -e '\033[1;33mStarting build...\033[0m'
          nix --extra-experimental-features 'nix-command flakes' build .#$FLAKE_SYSTEM "$@"
          echo -e '\033[1;33mSwitching to new generation...\033[0m'
          sudo ./result/sw/bin/darwin-rebuild switch --flake .#''${HOST} "$@"
          unlink ./result 2>/dev/null || true
          echo -e '\033[1;32mSwitch to new generation complete!\033[0m'
        '';
        "rollback" = mkScript "rollback" ''
          set -e
          PATH=${pkgs.git}/bin:$PATH
          FLAKE=$(${pkgs.hostname}/bin/hostname)
          echo -e '\033[1;33mAvailable generations:\033[0m'
          /run/current-system/sw/bin/darwin-rebuild --list-generations
          echo 'Enter the generation number for rollback:'
          read GEN_NUM
          if [ -z "$GEN_NUM" ]; then
            echo -e '\033[1;31mNo generation number entered. Aborting.\033[0m'
            exit 1
          fi
          echo -e "\033[1;33mRolling back to generation $GEN_NUM...\033[0m"
          /run/current-system/sw/bin/darwin-rebuild switch --flake .#$FLAKE --switch-generation $GEN_NUM
          echo -e "\033[1;32mRollback to generation $GEN_NUM complete!\033[0m"
        '';
      };
  };
}
