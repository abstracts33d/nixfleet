#!/usr/bin/env bash
set -euo pipefail
export PATH="/usr/bin:/run/current-system/sw/bin:$PATH"

# PreToolUse: guard against destructive nix/git commands
input=$(cat)
command=$(echo "$input" | jq -r '.tool_input.command // empty')

[[ -z $command ]] && exit 0

# Block: destructive nix store operations (managed by auto GC)
if [[ $command =~ nix-store[[:space:]]+--delete ]] ||
  [[ $command =~ nix[[:space:]]+store[[:space:]]+delete ]] ||
  [[ $command =~ nix-collect-garbage ]]; then
  echo '{"permissionDecision": "deny", "reason": "Garbage collection is managed by auto-GC. Do not run manually."}'
  exit 0
fi

# Block: force push to main
if [[ $command =~ git[[:space:]]+push[[:space:]]+.*--force ]] &&
  [[ $command =~ main || $command =~ master ]]; then
  echo '{"permissionDecision": "deny", "reason": "Force push to main/master is forbidden."}'
  exit 0
fi

# Ask: prefer nix run .#build-switch over raw rebuild commands
if [[ $command =~ nixos-rebuild ]] || [[ $command =~ darwin-rebuild ]]; then
  echo '{"additionalContext": "Prefer `nix run .#build-switch` over raw rebuild commands. It handles platform detection and flags."}'
  exit 0
fi

exit 0
