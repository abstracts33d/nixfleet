---
name: security
description: Full security audit with timestamped report. Use periodically or before major deploys.
user-invocable: true
---

# Security Audit

## Process

1. **Dispatch security-reviewer agent** with full repo scope
2. **Read latest report** from `.claude/security-reviews/` (most recent by date)
3. **Compare**: Identify new findings, resolved findings, unchanged findings
4. **MANDATORY: Write new report** to `.claude/security-reviews/YYYY-MM-DD.md` with:
   - Date, reviewer, scope
   - Findings table (severity, file, description, status)
   - Comparison with previous review (new/resolved/unchanged with counts)
   - Action items with priority
5. **Update current.md**: Copy the new report to `.claude/security-reviews/current.md` (always overwrite). This file is the live snapshot — anyone can read `current.md` without guessing the latest date.
6. **Commit both files**: `git add .claude/security-reviews/ && git commit`
7. **Present summary**: New/resolved/unchanged counts, top action items

**HARD RULE:** A security audit is NOT complete until the timestamped report AND `current.md` are written and committed. The agent must verify both files exist before reporting done.

## Report Template
```markdown
# Security Review — YYYY-MM-DD

**Reviewer:** [agent name]
**Scope:** Full repository
**Previous review:** YYYY-MM-DD

## Findings
| # | Severity | File | Finding | Status |

## Comparison with Previous
- New: N findings
- Resolved: N findings
- Unchanged: N findings

## Action Items
1. ...
```
