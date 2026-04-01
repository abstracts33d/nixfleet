#!/usr/bin/env bash
# spawn-fleet.sh — Start a NixFleet demo fleet with control plane and agents.
#
# Usage:
#   bash demo/spawn-fleet.sh start       # Start CP + 2 local mock agents (fast, no VMs)
#   bash demo/spawn-fleet.sh graphical   # Start CP + instructions to boot real QEMU VMs
#   bash demo/spawn-fleet.sh stop        # Stop everything
#   bash demo/spawn-fleet.sh status      # Show fleet status
#   bash demo/spawn-fleet.sh clean       # Stop + remove all demo state
set -euo pipefail

GREEN='\033[1;32m'
YELLOW='\033[1;33m'
BLUE='\033[1;34m'
RED='\033[1;31m'
NC='\033[0m'

CP_PORT="${CP_PORT:-8080}"
DEMO_DIR="/tmp/nixfleet-demo"
DEMO_API_KEY="demo-key"
DEMO_API_KEY_HASH="c48a01f49fd0f2cc404bc3cbbc80e91457a3d41bb429a695243de4c61794155c"

log() { echo -e "${YELLOW}[fleet]${NC} $*"; }
ok() { echo -e "${GREEN}[fleet]${NC} $*"; }
info() { echo -e "${BLUE}[fleet]${NC} $*"; }
err() { echo -e "${RED}[fleet]${NC} $*"; }

wait_for_cp() {
  log "Waiting for control plane on :$CP_PORT..."
  local elapsed=0
  while ! curl -sf "http://127.0.0.1:$CP_PORT/health" >/dev/null 2>&1; do
    sleep 1
    elapsed=$((elapsed + 1))
    if [ "$elapsed" -ge 120 ]; then
      err "Control plane did not start within 15s"
      err "Check $DEMO_DIR/cp.log for errors"
      exit 1
    fi
  done
  ok "Control plane ready (http://127.0.0.1:$CP_PORT)"
}

seed_api_key() {
  # Insert demo API key (idempotent — ignore if already exists)
  # Use nix-provided sqlite3 to avoid system dependency
  nix run nixpkgs#sqlite -- "$DEMO_DIR/cp.db" \
    "INSERT OR IGNORE INTO api_keys (key_hash, name, role) VALUES ('$DEMO_API_KEY_HASH', 'demo', 'admin');" 2>/dev/null

  ok "API key seeded (use: Authorization: Bearer $DEMO_API_KEY)"
}

start_cp() {
  mkdir -p "$DEMO_DIR"

  if [ -f "$DEMO_DIR/cp.pid" ] && kill -0 "$(cat "$DEMO_DIR/cp.pid")" 2>/dev/null; then
    log "Control plane already running (PID $(cat "$DEMO_DIR/cp.pid"))"
    return
  fi

  log "Starting control plane on :$CP_PORT"
  nix run .#control-plane -- \
    --listen "127.0.0.1:$CP_PORT" \
    --db-path "$DEMO_DIR/cp.db" \
    >"$DEMO_DIR/cp.log" 2>&1 &
  echo $! >"$DEMO_DIR/cp.pid"
  wait_for_cp
  seed_api_key
}

start_mock_agents() {
  log "Starting mock agents (local processes, dry-run)..."

  for id in demo-host-01 demo-host-02; do
    if [ -f "$DEMO_DIR/$id.pid" ] && kill -0 "$(cat "$DEMO_DIR/$id.pid")" 2>/dev/null; then
      log "Agent $id already running"
      continue
    fi

    nix run .#nixfleet-agent -- \
      --control-plane-url "http://127.0.0.1:$CP_PORT" \
      --machine-id "$id" \
      --poll-interval 5 \
      --db-path "$DEMO_DIR/$id.db" \
      --dry-run \
      --allow-insecure \
      >"$DEMO_DIR/$id.log" 2>&1 &
    echo $! >"$DEMO_DIR/$id.pid"
    log "  Started $id (PID $!)"
  done

  ok "Demo fleet running: 1 control plane + 2 agents (dry-run)"
  echo ""
  info "=== Demo Commands (API key: $DEMO_API_KEY) ==="
  echo ""
  info "Fleet status:"
  echo "  curl -s -H 'Authorization: Bearer $DEMO_API_KEY' http://127.0.0.1:$CP_PORT/api/v1/machines | jq ."
  echo ""
  info "Set desired generation:"
  echo "  curl -s -X POST http://127.0.0.1:$CP_PORT/api/v1/machines/demo-host-01/set-generation \\"
  echo "    -H 'Authorization: Bearer $DEMO_API_KEY' \\"
  echo "    -H 'Content-Type: application/json' \\"
  echo "    -d '{\"hash\": \"/nix/store/abc123-nixos-system-demo-host-01\"}'"
  echo ""
  info "CLI status:"
  echo "  nix run .#nixfleet -- status"
  echo ""
  info "Agent logs:"
  echo "  tail -f $DEMO_DIR/demo-host-01.log"
  echo ""
  info "Stop:"
  echo "  bash demo/spawn-fleet.sh stop"
}

start_graphical() {
  log "Graphical VM Demo"
  echo ""
  info "This mode boots real NixOS VMs with the NixFleet agent pre-configured."
  info "The agent in each VM connects to the control plane running on the host."
  info "QEMU NAT maps 10.0.2.2 (inside VM) -> localhost (host)."
  echo ""

  start_cp

  echo ""
  log "=== Next steps (run in separate terminals) ==="
  echo ""

  info "Step 1: Boot VMs with spawn-qemu (first run builds the image):"
  echo ""
  echo "  # VM 1 (SSH :2220)"
  echo "  nix run .#spawn-qemu"
  echo ""

  info "Step 2: Install NixOS on the VM with nixos-anywhere (standard tooling):"
  echo ""
  echo "  nixos-anywhere --flake .#web-02 root@localhost -p 2220"
  echo ""

  info "Step 3: After reboot, the agent auto-starts and polls the CP:"
  echo ""
  echo "  # Check fleet status"
  echo "  curl -s -H 'Authorization: Bearer $DEMO_API_KEY' http://127.0.0.1:$CP_PORT/api/v1/machines | jq ."
  echo ""
  echo "  # Set a desired generation"
  echo "  curl -s -X POST http://127.0.0.1:$CP_PORT/api/v1/machines/web-02/set-generation \\"
  echo "    -H 'Authorization: Bearer $DEMO_API_KEY' \\"
  echo "    -H 'Content-Type: application/json' \\"
  echo "    -d '{\"hash\": \"/nix/store/demo-generation-hash\"}'"
  echo ""

  info "Step 4: Watch agent logs in the VM:"
  echo ""
  echo "  ssh -p 2220 root@localhost journalctl -fu nixfleet-agent"
  echo ""

  ok "Control plane is running. Follow the steps above to boot VMs."
  info "Stop CP: bash demo/spawn-fleet.sh stop"
}

stop_fleet() {
  log "Stopping fleet..."

  for pidfile in "$DEMO_DIR"/*.pid; do
    [ -f "$pidfile" ] || continue
    local name
    name=$(basename "$pidfile" .pid)
    local pid
    pid=$(cat "$pidfile")
    if kill -0 "$pid" 2>/dev/null; then
      kill "$pid" 2>/dev/null || true
      log "  Stopped $name (PID $pid)"
    else
      log "  $name already stopped"
    fi
    rm -f "$pidfile"
  done

  ok "Fleet stopped"
}

clean_fleet() {
  stop_fleet
  if [ -d "$DEMO_DIR" ]; then
    rm -rf "$DEMO_DIR"
    ok "Removed $DEMO_DIR"
  fi
}

show_status() {
  if ! curl -sf "http://127.0.0.1:$CP_PORT/health" >/dev/null 2>&1; then
    err "Control plane not running on :$CP_PORT"
    exit 1
  fi

  ok "Control plane running on :$CP_PORT"
  echo ""
  curl -s -H "Authorization: Bearer $DEMO_API_KEY" "http://127.0.0.1:$CP_PORT/api/v1/machines" | jq . 2>/dev/null ||
    curl -s -H "Authorization: Bearer $DEMO_API_KEY" "http://127.0.0.1:$CP_PORT/api/v1/machines"
}

usage() {
  echo "Usage: bash demo/spawn-fleet.sh <command>"
  echo ""
  echo "Commands:"
  echo "  start       Start CP + 2 mock agents (fast, no VMs)"
  echo "  graphical   Start CP + instructions for real QEMU VMs"
  echo "  stop        Stop all fleet processes"
  echo "  status      Show fleet status from control plane"
  echo "  clean       Stop + remove all demo state"
  exit 1
}

case "${1:-}" in
start)
  start_cp
  start_mock_agents
  ;;
graphical) start_graphical ;;
stop) stop_fleet ;;
status) show_status ;;
clean) clean_fleet ;;
*) usage ;;
esac
