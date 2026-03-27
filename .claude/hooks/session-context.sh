#!/usr/bin/env bash
set -euo pipefail

# SessionStart: inject git context
cd "${CLAUDE_PROJECT_DIR:-.}"

branch=$(git branch --show-current 2>/dev/null || echo "detached")
dirty=$(git status --porcelain 2>/dev/null | wc -l | tr -d ' ')
ahead=$(git rev-list --count @{upstream}..HEAD 2>/dev/null || echo "?")
last=$(git log -1 --format="%h %s" 2>/dev/null || echo "no commits")

echo "Branch: $branch | Dirty files: $dirty | Ahead: $ahead | Last: $last"
