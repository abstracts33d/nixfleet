#!/usr/bin/env bash
set -euo pipefail
export PATH="/usr/bin:/run/current-system/sw/bin:$PATH"

# PostToolUse: auto-format .nix files after Edit/Write
input=$(cat)
file_path=$(echo "$input" | jq -r '.tool_input.file_path // empty')

[[ -z $file_path ]] && exit 0
[[ $file_path != *.nix ]] && exit 0
[[ ! -f $file_path ]] && exit 0

before=$(md5sum "$file_path" | cut -d' ' -f1)
alejandra -q "$file_path" 2>/dev/null || exit 0
after=$(md5sum "$file_path" | cut -d' ' -f1)

if [[ $before != "$after" ]]; then
  echo '{"additionalContext": "File was auto-formatted by alejandra. Review the formatting changes."}'
fi

exit 0
