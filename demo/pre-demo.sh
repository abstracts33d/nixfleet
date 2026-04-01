#!/usr/bin/env bash
set -euo pipefail

GREEN='\033[1;32m'
YELLOW='\033[1;33m'
RED='\033[1;31m'
BLUE='\033[1;34m'
NC='\033[0m'

echo -e "${YELLOW}=== NixFleet Demo Preparation ===${NC}"
echo ""
echo "This script builds and caches everything needed for the demo."
echo "Expected time: ~10-15 min (first run), ~1 min (cached)."
echo ""

step=0
total=8
fail=0

run_step() {
  step=$((step + 1))
  local desc="$1"
  shift
  echo -e "${YELLOW}[$step/$total] $desc${NC}"
  if "$@"; then
    echo -e "${GREEN}  Done${NC}"
  else
    echo -e "${RED}  FAILED${NC}"
    fail=$((fail + 1))
  fi
  echo ""
}

# -- Dev shell warmup (first — so cargo is cached for step 6) -----------------
run_step "Warming up dev shell (provides cargo, rustc, clippy)..." \
  bash -c 'nix develop --command true 2>&1'

# -- Build Rust binaries -------------------------------------------------------
run_step "Building nixfleet-agent..." \
  nix build .#nixfleet-agent --no-link

run_step "Building nixfleet-control-plane..." \
  nix build .#nixfleet-control-plane --no-link

run_step "Building nixfleet-cli..." \
  nix build .#nixfleet-cli --no-link

# -- Cache fleet eval -----------------------------------------------------------
run_step "Caching fleet eval (5 hosts)..." \
  bash -c 'nix eval .#nixosConfigurations --apply "x: builtins.attrNames x" > /dev/null 2>&1'

# -- Eval tests -----------------------------------------------------------------
run_step "Caching eval tests (6 checks + treefmt)..." \
  bash -c '
    for check in eval-hostspec-defaults eval-ssh-hardening eval-username-override eval-locale-timezone eval-ssh-authorized eval-password-files treefmt; do
      nix build ".#checks.x86_64-linux.$check" --no-link 2>&1 || exit 1
    done
  '

# -- Rust tests (run inside dev shell for cargo) --------------------------------
run_step "Running Rust workspace tests..." \
  bash -c 'nix develop --command cargo test --workspace --quiet 2>&1 | tail -5'

# -- Full validation ------------------------------------------------------------
run_step "Running full validation (eval + host builds)..." \
  bash -c 'nix run .#validate 2>&1 | tee /tmp/last-validate-output.txt'

# -- Summary --------------------------------------------------------------------
echo "================================================================"
if [ "$fail" -eq 0 ]; then
  echo -e "${GREEN}=== Demo preparation complete (all $total steps passed) ===${NC}"
else
  echo -e "${RED}=== Demo preparation finished with $fail failure(s) ===${NC}"
fi
echo "================================================================"
echo ""
echo -e "${BLUE}Checklist before going live:${NC}"
echo ""
echo "  1. Set terminal font to 18pt+ (dark theme)"
echo "  2. Open 3 terminal tabs:"
echo "     Tab 1: demo commands"
echo "     Tab 2: control plane (starts during Act 5)"
echo "     Tab 3: agent (starts during Act 5)"
echo "  3. Test quick commands:"
echo "     nix run .#nixfleet -- --help"
echo "     nix run .#control-plane -- --help"
echo "     nix run .#nixfleet-agent -- --help"
echo "  4. Test fleet spawn:"
echo "     bash demo/spawn-fleet.sh start"
echo "     curl -s -H 'Authorization: Bearer demo-key' http://127.0.0.1:8080/api/v1/machines | jq ."
echo "     bash demo/spawn-fleet.sh stop"
echo "  5. Open fallback: docs/business/rendered/ in browser"
echo "  6. Phone silent, notifications off, screensaver disabled"
echo "  7. Review Act 0 talking points (no terminal — pure narrative)"
echo ""
echo "  Cached validate output: /tmp/last-validate-output.txt"
echo ""
echo -e "${GREEN}Ready to demo.${NC}"
