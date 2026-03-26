#!/usr/bin/env bash
# scripts/backlog-existing-issues.sh
# One-time script to move all open issues without a status to Backlog.
# Usage: bash scripts/backlog-existing-issues.sh

set -euo pipefail
source "$(dirname "$0")/gh-issue-helper.sh"

echo "Moving all open issues to Backlog..."
for num in $(gh issue list -R "$REPO" --state open --json number --jq '.[].number'); do
  echo "Moving #$num to Backlog"
  gh_move_issue "$num" "Backlog" 2>/dev/null || echo "  (skipped — may not be in project)"
done
echo "Done."
