# Agent Knowledge Proposals

Files here are proposed by agents during their work.
Review and integrate into the appropriate knowledge file.

## Flow

1. Agent discovers something worth recording during work
2. Agent writes to `_proposals/<agent-name>-<topic>.md` with frontmatter:
   ```yaml
   ---
   proposed_by: nix-expert
   date: 2026-03-25
   target: nix/gotchas.md
   action: append  # or: create, replace-section
   ---
   ```
3. The `/review` skill checks `_proposals/` for pending knowledge
4. Human approves and knowledge merges into the target file
5. The proposal file is deleted

## Why Not Auto-Merge?

Knowledge files are loaded into agent context. Bad knowledge pollutes all future sessions.
Human review is the quality gate.
