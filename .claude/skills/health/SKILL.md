---
name: health
description: Quick health check — config, tests, CI, format, issues. Fast parallel version of /audit.
user-invocable: true
---

# Quick Health Check

## Process

### Stage 1 — Parallel Scan (3 agents simultaneously)

Dispatch all three agents in a SINGLE message (HARD REQUIREMENT — do not sequence these):

**Agent A — config-manager**:
- Check CLAUDE.md, README.md, TODO.md for obvious staleness
- Verify `.claude/agents/` and `.claude/skills/` tables match actual files on disk
- Check for open GitHub Issues labeled `urgency:now`:
  `gh issue list --label "urgency:now" --state open --json number,title`
- Check for draft PRs or stale branches:
  `gh pr list --state open --json number,title,isDraft,updatedAt`
- Output: config health summary (Green / Yellow / Red per area)

**Agent B — test-runner**:
- Run eval tests: `nix flake check --no-build`
- Run cargo tests if a Rust project exists: `cargo test --workspace 2>&1 | tail -20`
- Check format: `nix fmt --fail-on-change 2>&1 | head -20` (report only, do not fix)
- Output: test results with pass/fail counts and any error output

**Agent C — devops**:
- Check CI status: `gh run list --limit 5 --json status,conclusion,name,updatedAt`
- Check git hook health: verify `.githooks/pre-commit` and `.githooks/pre-push` exist and are executable
- Check branch protection: `gh api repos/{owner}/{repo}/branches/main --jq '.protection'` (derive owner/repo from `gh repo view --json nameWithOwner -q .nameWithOwner`)
- Check last security review date: `ls -t .claude/security-reviews/ | head -1`
- Output: CI/CD health summary

### Stage 2 — Wait and Consolidate

Wait for all three agents to complete. Merge results into a single health matrix:

```
## Health Check — YYYY-MM-DD HH:MM

### Config        [GREEN / YELLOW / RED]
<1-2 line summary>

### Tests         [GREEN / YELLOW / RED]
<1-2 line summary with counts>

### CI/CD         [GREEN / YELLOW / RED]
<1-2 line summary>

### Overall: X green, Y warnings, Z errors
```

### Stage 3 — Triage

If any area is RED:
- Recommend the appropriate follow-up skill:
  - Config RED → dispatch `doc-writer` or run `/audit`
  - Tests RED → dispatch `test-runner` for detailed diagnosis or run `/diagnose`
  - CI RED → dispatch `devops` for fix or run `/incident`

If all GREEN:
- Confirm: "Fleet is healthy. No immediate action needed."
- Suggest: "Run `/audit` for a deeper analysis or `/suggest` for improvement ideas."

If YELLOW (warnings only):
- List the warnings with recommended actions
- Ask: "Want to address any of these now?"

## Verification

Before presenting, invoke `superpowers:verification-before-completion`:
- Show actual command output from each agent (test counts, CI run IDs, file lists)
- The three agents MUST have run in parallel — if they ran sequentially, the health check is invalid
- Never report GREEN without evidence
