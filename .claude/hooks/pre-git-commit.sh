#!/usr/bin/env bash
set -euo pipefail
export PATH="/usr/bin:/run/current-system/sw/bin:$PATH"

# PreToolUse: gate git commit on formatting
input=$(cat)
command=$(echo "$input" | jq -r '.tool_input.command // empty')

[[ ! $command =~ git[[:space:]]+commit ]] && exit 0

cd "${CLAUDE_PROJECT_DIR:-.}"
if ! nix fmt -- --check 2>/dev/null; then
  echo '{"permissionDecision": "deny", "reason": "Nix files are not formatted. Run `nix fmt` first."}'
fi

exit 0
