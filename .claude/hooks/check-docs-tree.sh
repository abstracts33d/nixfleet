#!/usr/bin/env bash
set -euo pipefail
export PATH="/usr/bin:/run/current-system/sw/bin:$PATH"

input=$(cat)
file=$(echo "$input" | jq -r '.tool_input.file_path // empty')

[[ -z $file ]] && exit 0

# Strip project dir prefix
rel="${file#${CLAUDE_PROJECT_DIR:-.}/}"

msg=""
case "$rel" in
modules/hosts/* | modules/hosts/vm/*)
  msg="Host file changed. Update docs/src/hosts/ and docs/guide/ if architecture affected."
  ;;
modules/scopes/*)
  msg="Scope changed. Update docs/src/scopes/ and docs/guide/concepts/scopes.md."
  ;;
modules/core/*)
  msg="Core module changed. Update docs/src/core/ if behavior affected."
  ;;
modules/fleet.nix)
  msg="Fleet definition changed. Update README.md hosts table and docs/src/hosts/."
  ;;
modules/_shared/host-spec-module.nix)
  msg="hostSpec changed. Update CLAUDE.md flags table, README.md, docs/nixfleet/specs/mk-fleet-api.md."
  ;;
modules/_shared/lib/*)
  msg="Framework lib changed. Update docs/nixfleet/specs/mk-fleet-api.md."
  ;;
modules/_shared/mk-host.nix)
  msg="Host constructor changed. Update docs/src/architecture.md."
  ;;
modules/tests/*)
  msg="Tests changed. Update docs/src/testing/ and docs/guide/development/testing.md."
  ;;
modules/_shared/*)
  msg="Shared module changed. Update docs/src/architecture.md if structure affected."
  ;;
.claude/agents/* | .claude/skills/* | .claude/hooks/* | .claude/rules/*)
  msg="Claude integration changed. Update docs/src/claude/."
  ;;
esac

if [[ -n $msg ]]; then
  jq -n --arg msg "$msg" '{"additionalContext": ("DOCS TREE ALERT: " + $msg)}'
fi

exit 0
