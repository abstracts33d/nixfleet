#!/usr/bin/env bash
set -euo pipefail

# Stop: check if docs need updating when code changed
# Prevent infinite loop
[[ ${stop_hook_active:-} == "1" ]] && exit 0
export stop_hook_active=1

cd "${CLAUDE_PROJECT_DIR:-.}"

# Check staged + unstaged changes
CODE_CHANGED=$(git diff --name-only HEAD 2>/dev/null | grep -cE '\.nix$' || true)
DOCS_CHANGED=$(git diff --name-only HEAD 2>/dev/null | grep -cE '(CLAUDE|README|TODO)\.md$' || true)
MODULES_CHANGED=$(git diff --name-only HEAD 2>/dev/null | grep -cE '^modules/' || true)
DOCS_TREE_CHANGED=$(git diff --name-only HEAD 2>/dev/null | grep -cE '^docs/(src|guide)/' || true)

# Block if nix files changed but no top-level docs updated
if [[ $CODE_CHANGED -gt 0 && $DOCS_CHANGED -eq 0 ]]; then
  echo "Nix files changed but CLAUDE.md/README.md/TODO.md not updated." >&2
  exit 2
fi

# Block if modules changed but docs trees not updated
if [[ $MODULES_CHANGED -gt 0 && $DOCS_TREE_CHANGED -eq 0 ]]; then
  echo "Module files changed but docs/src/ and docs/guide/ not updated. Run /docs-generate or update manually." >&2
  exit 2
fi

exit 0
