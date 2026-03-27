#!/usr/bin/env bash
set -euo pipefail
export PATH="/usr/bin:/run/current-system/sw/bin:$PATH"

# PostToolUse: remind about config dependency chains
# Path pairs are loaded from config-deps.json (org-customizable)
input=$(cat)
file_path=$(echo "$input" | jq -r '.tool_input.file_path // empty')

[[ -z $file_path ]] && exit 0

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DEPS_FILE="$SCRIPT_DIR/config-deps.json"

[[ ! -f $DEPS_FILE ]] && exit 0

rel="${file_path#${CLAUDE_PROJECT_DIR:-.}/}"
msg=""

while IFS= read -r entry; do
  pattern=$(echo "$entry" | jq -r '.match')
  message=$(echo "$entry" | jq -r '.message')
  # shellcheck disable=SC2254
  case "$rel" in
  $pattern)
    msg="$message"
    break
    ;;
  esac
done < <(jq -c '.[]' "$DEPS_FILE")

if [[ -n $msg ]]; then
  jq -n --arg msg "$msg" '{"additionalContext": $msg}'
fi

exit 0
