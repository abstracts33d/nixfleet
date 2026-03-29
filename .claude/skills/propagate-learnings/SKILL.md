# Propagate Learnings

Extract generic knowledge from project memories and agent learnings, then propose updates to the shared claude-defaults plugin.

## When to use

After a significant session with new learnings, or periodically to keep the shared knowledge fresh.

## Process

1. **Scan local memories**

   Read all memory files:
   ```
   ~/.claude/projects/*/memory/*.md
   .claude/agent-memory/*/
   ```

   For each file, classify content as:
   - **GENERIC** — applicable to any project (workflow patterns, anti-patterns, tool gotchas)
   - **PROJECT-SPECIFIC** — only relevant to this repo (architecture decisions, module details)

2. **Scan global knowledge for gaps**

   Read current global knowledge:
   ```
   ~/.claude/knowledge/claude-code/*.md
   ~/.claude/knowledge/languages/*.md
   ```

   Identify learnings from step 1 that are GENERIC but NOT yet in global knowledge.

3. **Prepare update**

   For each gap found:
   - Draft the addition to the appropriate knowledge file
   - If no file fits, draft a new knowledge file

4. **Clone and update claude-defaults**

   ```bash
   TMPDIR=$(mktemp -d)
   git clone git@github.com:abstracts33d/claude-defaults.git "$TMPDIR/claude-defaults"
   cd "$TMPDIR/claude-defaults"
   git checkout -b docs/propagate-learnings-$(date +%Y%m%d)
   ```

   Apply the drafted changes to the knowledge files in the clone.

5. **Present to user**

   Show:
   - Summary of learnings extracted (generic vs project-specific)
   - Diff of proposed changes to claude-defaults
   - Ask: "Review OK, can I push and create PR?"

6. **Push and create PR** (only after user approval)

   ```bash
   git add -A
   git commit -m "docs: propagate learnings from <project>"
   git push -u origin <branch>
   gh pr create --title "docs: propagate learnings" --body "..."
   ```

## What counts as GENERIC

- Tool gotchas (e.g., "alejandra strips unused args", "reqwest defaults to openssl in Nix")
- Workflow patterns (e.g., "parallel agents for independent tasks")
- Anti-patterns (e.g., "never merge PRs automatically")
- Claude Code configuration tips
- Bash/shell patterns

## What stays PROJECT-SPECIFIC

- Architecture decisions (e.g., "framework = mechanism, overlay = policy")
- Module/API details (e.g., "mkFleet signature")
- Audit findings about specific code
- Phase/roadmap status
- Team/org conventions
