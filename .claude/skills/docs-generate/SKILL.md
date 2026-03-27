---
name: docs-generate
description: Regenerate both documentation trees (technical + guide) from current codebase state. Use after major changes or when docs drift from code.
user-invocable: true
---

# Generate Documentation

Regenerate both documentation sites from the current codebase.

## Process

1. **Technical docs** (`docs/src/`):
   - Dispatch `doc-writer` agent to scan all modules, hosts, scopes, apps
   - Update each .md file with current state (packages, options, flags)
   - Verify SUMMARY.md matches the file tree

2. **Guide docs** (`docs/guide/`):
   - Dispatch `doc-writer` agent to update conceptual content
   - Verify guides reflect current architecture
   - Update examples and commands

3. **Verify**: Run `mdbook build` on both trees to ensure no broken links

4. **Report**: List files updated, new files created, stale files removed

## Rules
- Technical docs describe WHAT and WHERE (reference)
- Guide docs describe WHY and HOW (conceptual)
- Never duplicate content — guide links to technical docs for details
- Both trees must build without errors
