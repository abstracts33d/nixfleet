#!/usr/bin/env bash
set -euo pipefail
export PATH="/usr/bin:/run/current-system/sw/bin:$PATH"

# PreToolUse: run eval tests + cargo test before git push
# Full validate (host builds) is too slow for a hook — run manually or in CI
input=$(cat)
command=$(echo "$input" | jq -r '.tool_input.command // empty')

[[ ! $command =~ git[[:space:]]+push ]] && exit 0

cd "${CLAUDE_PROJECT_DIR:-.}"

# Format check (fast)
if ! nix fmt -- --fail-on-change 2>/dev/null; then
  echo '{"permissionDecision": "deny", "reason": "Nix files are not formatted. Run `nix fmt` first."}'
  exit 0
fi

# Eval tests (fast, ~10s cached)
failed=0
for check in eval-hostspec-defaults eval-scope-activation eval-org-defaults; do
  if ! nix build ".#checks.x86_64-linux.$check" --no-link 2>/dev/null; then
    failed=1
  fi
done
if [[ $failed -ne 0 ]]; then
  echo '{"permissionDecision": "deny", "reason": "Eval tests failed. Fix before pushing."}'
  exit 0
fi

# Cargo test (fast, ~5s)
if [[ -f Cargo.toml ]]; then
  if ! cargo test --workspace --bins --tests --lib --quiet 2>/dev/null; then
    echo '{"permissionDecision": "deny", "reason": "Rust tests failed. Fix before pushing."}'
    exit 0
  fi
fi

exit 0
