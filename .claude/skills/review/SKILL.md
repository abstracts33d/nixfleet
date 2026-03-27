---
name: review
description: Parallel code + security review of current changes. Use when code is ready for review or before merging.
user-invocable: true
---

# Code Review

## Process

1. **Identify changes**: Run `git diff main --stat` to see what changed
2. **Dispatch parallel reviews**:
   - Dispatch `code-reviewer` agent with the diff context
   - Dispatch `security-reviewer` agent with the diff context
   - Dispatch `docs-assessor` agent for documentation coherence check
3. **Wait for all results**
4. **Synthesize**: Combine findings, deduplicate, sort by severity
5. **Present**: Findings table with file paths and recommendations
5.5. **Post to issue**: If working on a branch linked to an issue:
   - Detect issue from branch name or recent commit messages (`git log --oneline -5 | grep -oP '#\d+'`)
5.6. **Save security findings**: If the security-reviewer found any High or Critical findings:
   - Write a timestamped report to `.claude/security-reviews/YYYY-MM-DD.md`
   - Follow the template in `/security` skill
   - Commit the report
6. **Omission Check**: Run an automated omission scan over the reviewed changes:
   1. **Doc sync**: Verify CLAUDE.md, README.md, and docs/ reflect the changes being reviewed
      - Check that new scopes/features have docs/src/ pages
      - Check that modified flags are reflected in flag tables
      - Check that skills/agents tables match .claude/skills/ and .claude/agents/
   2. **TODO.md redirect**: Confirm TODO.md still points to GitHub Issues (not accumulating new items)
   3. **Security review**: If security-reviewer found findings, verify a timestamped report exists in `.claude/security-reviews/`
   4. **Issue links**: Verify the branch/PR references relevant GitHub Issues via `Closes #XX`
   5. **Test coverage**: If new scopes or flags were added, verify eval test assertions exist
   6. **Config dependencies**: Run through `.claude/rules/config-dependencies.md` for any chain that applies to the changed files

   Flag any omissions as findings in the review output.
7. **If critical findings**: Recommend blocking merge until resolved
8. **If clean**: Confirm ready to ship

## Verification
Before presenting results, invoke `superpowers:verification-before-completion`:
- Show actual command output proving the review ran
- Never summarize without evidence

## Output Format
```
## Review Results

### Code Review (code-reviewer)
| Severity | File | Finding |
...

### Security Review (security-reviewer)
| Severity | File | Finding |
...

### Documentation Assessment (docs-assessor)
| Severity | File | Finding |
...

### Summary
- Critical: N, High: N, Medium: N, Low: N
- Recommendation: [PASS/BLOCK]
```
