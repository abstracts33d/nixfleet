#!/usr/bin/env bash
set -euo pipefail
export PATH="/usr/bin:/run/current-system/sw/bin:$PATH"

# Fleet repo doc-tree check.
# Framework modules (core/, scopes/, tests/) are in nixfleet, not here.
# This hook covers fleet-specific files only.

input=$(cat)
file=$(echo "$input" | jq -r '.tool_input.file_path // empty')

[[ -z $file ]] && exit 0

rel="${file#${CLAUDE_PROJECT_DIR:-.}/}"

msg=""
case "$rel" in
modules/fleet.nix)
  msg="Fleet definition changed. Update README.md hosts table if hosts/orgs changed."
  ;;
modules/_hardware/*)
  msg="Hardware config changed. Verify host still builds."
  ;;
modules/_config/*)
  msg="Org dotfile changed. Corresponding HM module is in nixfleet — verify compatibility."
  ;;
modules/_shared/disk-templates/*)
  msg="Disk template changed. Verify all hosts using this template still build."
  ;;
.claude/agents/* | .claude/skills/* | .claude/hooks/* | .claude/rules/*)
  msg="Claude integration changed. Update CLAUDE.md if structure affected."
  ;;
demo/*)
  msg="Demo script changed. Verify demo/README.md is up to date."
  ;;
esac

if [[ -n $msg ]]; then
  jq -n --arg msg "$msg" '{"additionalContext": ("DOCS TREE ALERT: " + $msg)}'
fi

exit 0
