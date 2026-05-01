GREEN='\033[1;32m'
YELLOW='\033[1;33m'
RED='\033[1;31m'
NC='\033[0m'

VM_DIR="${XDG_DATA_HOME:-$HOME/.local/share}/nixfleet/vms"
SSH_OPTS="-o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o ConnectTimeout=2"

assign_port() {
  local host="$1"
  local hosts
  hosts=$(nix eval .#nixosConfigurations --apply 'x: builtins.concatStringsSep "\n" (builtins.sort builtins.lessThan (builtins.attrNames x))' --raw 2>/dev/null)
  local idx=0
  while IFS= read -r name; do
    if [ "$name" = "$host" ]; then
      HOST_INDEX=$idx
      if [ -n "${PORT_OVERRIDE:-}" ]; then
        SSH_PORT="$PORT_OVERRIDE"
      else
        SSH_PORT=$((2201 + idx))
      fi
      return
    fi
    idx=$((idx + 1))
  done <<<"$hosts"
  echo -e "${RED}Host '$host' not found in nixosConfigurations${NC}" >&2
  exit 1
}

wait_ssh() {
  local port="$1" timeout="$2"
  local elapsed=0
  while ! ssh $SSH_OPTS -p "$port" root@localhost true 2>/dev/null; do
    sleep 1
    elapsed=$((elapsed + 1))
    if [ "$elapsed" -ge "$timeout" ]; then
      echo -e "${RED}SSH timeout after ${timeout}s${NC}" >&2
      return 1
    fi
  done
  echo -e "${GREEN}SSH ready (${elapsed}s)${NC}"
}

provision_identity_key() {
  local host="$1"
  local explicit_key="${2:-}"
  EXTRA_FILES_DIR=$(mktemp -d)
  EXTRA_FILES_ARGS=""

  local key_src=""
  if [ -n "$explicit_key" ]; then
    if [ ! -f "$explicit_key" ]; then
      echo -e "${RED}Identity key not found: $explicit_key${NC}" >&2
      exit 1
    fi
    key_src="$explicit_key"
  elif [ -f "$HOME/.ssh/id_ed25519" ]; then
    key_src="$HOME/.ssh/id_ed25519"
  fi

  if [ -n "$key_src" ]; then
    local vm_user
    vm_user="$(nix eval ".#nixosConfigurations.${host}.config.hostSpec.userName" --raw 2>/dev/null || echo "root")"
    # Mirror the secrets impl's default `userKey` path —
    # `${hS.home}/.ssh/id_ed25519`. Both paths are written so
    # impermanent hosts (key bind-mounted from /persist/home/...)
    # and non-impermanent hosts (key in plain /home/...) work.
    for prefix in "persist/home/$vm_user" "home/$vm_user"; do
      mkdir -p "$EXTRA_FILES_DIR/$prefix/.ssh"
      cp "$key_src" "$EXTRA_FILES_DIR/$prefix/.ssh/id_ed25519"
      chmod 600 "$EXTRA_FILES_DIR/$prefix/.ssh/id_ed25519"
    done
    EXTRA_FILES_ARGS="--extra-files $EXTRA_FILES_DIR"
    echo -e "${GREEN}Provisioning identity key for $vm_user (from $key_src)${NC}"
  else
    echo -e "${YELLOW}No identity key found - secrets requiring host decryption will not work${NC}"
    echo -e "${YELLOW}Provide one with --identity-key PATH, or place at ~/.ssh/id_ed25519${NC}"
  fi
}

build_iso() {
  echo -e "${YELLOW}Building custom ISO...${NC}"
  local iso_path
  if ! iso_path=$(nix build .#iso --no-link --print-out-paths 2>/dev/null); then
    echo -e "${RED}No ISO package found. Set nixfleet.isoSshKeys in your fleet config.${NC}" >&2
    exit 1
  fi
  ISO_FILE=$(find "$iso_path/iso" -name '*.iso' | head -1)
  if [ -z "$ISO_FILE" ]; then
    echo -e "${RED}No ISO file found in build output${NC}" >&2
    exit 1
  fi
  echo -e "${GREEN}ISO: $ISO_FILE${NC}"
}

all_hosts() {
  nix eval .#nixosConfigurations --apply 'x: builtins.concatStringsSep "\n" (builtins.sort builtins.lessThan (builtins.attrNames x))' --raw 2>/dev/null
}

compute_vlan_args() {
  VLAN_ARGS=""
  if [ -n "${VLAN_PORT:-}" ]; then
    local mac_suffix
    mac_suffix=$(printf "%02x" "$((HOST_INDEX + 1))")
    VLAN_ARGS="-netdev socket,id=vlan0,mcast=230.0.0.1:${VLAN_PORT},localaddr=127.0.0.1 -device virtio-net-pci,netdev=vlan0,mac=52:54:00:12:34:${mac_suffix}"
  fi
}

compute_display_args() {
  DISPLAY_ARGS=""
  DAEMONIZE_ARGS="-daemonize"
  case "${DISPLAY_MODE:-none}" in
  spice)
    DISPLAY_ARGS="-display spice-app -device virtio-vga -device virtio-serial-pci -chardev spicevmc,id=vdagent,debug=0,name=vdagent -device virtserialport,chardev=vdagent,name=com.redhat.spice.0"
    DAEMONIZE_ARGS=""
    ;;
  gtk)
    DISPLAY_ARGS="-display gtk -device virtio-vga"
    DAEMONIZE_ARGS=""
    ;;
  none)
    DISPLAY_ARGS="-display none -serial null"
    ;;
  *)
    echo -e "${RED}Unknown display mode: $DISPLAY_MODE (use: none, spice, gtk)${NC}" >&2
    exit 1
    ;;
  esac
}
