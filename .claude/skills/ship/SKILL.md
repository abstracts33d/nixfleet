---
name: ship
description: Ship feature from worktree to main with validation. Use when work is ready to merge.
user-invocable: true
---

# Ship Feature

Complete workflow for shipping a feature from worktree to main.

## Steps

### 1. Present Changes
Show ALL files modified vs main:
- List each file with a one-line description
- Group by category (code, docs, config)
- Check dependency chains (`.claude/rules/config-dependencies.md`)

**Ask for confirmation before proceeding.**

### 2. Create Feature Branch
`git checkout -b <type>/<description>` where type is `feat`, `fix`, `docs`, `refactor`, `chore`.

### 3. Atomic Commits
Split into logical commits. Each independently meaningful.
- Conventional messages: `feat:`, `fix:`, `docs:`, `refactor:`, `chore:`
- Include `Co-Authored-By: Claude <noreply@anthropic.com>`

### 3.5. Code Review
Invoke `superpowers:requesting-code-review` skill:
- Review changes against spec/plan
- Verify conventions and dependency chains
- Fix issues before proceeding
- If working on a tracked issue, transition to In Review:

### 4. Validate
Dispatch `test-runner` agent:
- Run `nix run .#validate`
- If fails, fix before proceeding

**Ask for confirmation before pushing.**

### 5. Docs Check (ALL doc trees — MANDATORY)
Dispatch `doc-writer` agent. **Every doc tree must be checked and updated:**

| Tree | Path | What to check |
|------|------|---------------|
| **AI context** | `CLAUDE.md` | Module tree, flags tables, skills/agents tables, architecture sections |
| **User-facing** | `README.md` | Hosts table, scopes, commands, architecture |
| **Technical docs** | `docs/src/` | Architecture, hosts, scopes, testing, apps pages. Check `SUMMARY.md` entries. |
| **User guide** | `docs/guide/` | Getting started, concepts, advanced (new-host), development pages |
| **NixFleet business** | `docs/nixfleet/` | Roadmap YAML (phase status), README, rendered source data |
| **GitHub Issues** | Issue tracker | Relevant issues updated/closed |

**HARD RULE:** A feature is NOT ready to ship if ANY doc tree references stale architecture, old file paths, outdated host counts, or missing new concepts. This check is blocking — fix before pushing.

Specific checks:
- New modules/files → `docs/src/` page + `SUMMARY.md` entry
- Changed architecture → `docs/src/architecture.md` + `docs/guide/` concept pages
- New hostSpec flags → CLAUDE.md flags table + README.md
- New hosts → host pages in `docs/src/hosts/`
- Changed commands → `docs/guide/getting-started/` + `docs/guide/development/`
- Phase completion → `docs/nixfleet/data/roadmap.yaml` status update

### 5.5. Docs Assessment
Dispatch `docs-assessor` agent:
- Verify cross-document coherence across ALL doc trees
- Flag any stale or contradictory content
- If critical issues found, fix before pushing

### 5.6. Config Health Check
Dispatch `config-manager` agent:
- Verify CLAUDE.md counts match reality (agents, skills, hosts)
- Verify agent definitions are valid
- Verify no stale references to deleted code
- Fix any issues before pushing

### 5.7. Omission Scan
Before pushing, verify nothing was forgotten:
- [ ] **docs/src/** — new pages for new modules/features, SUMMARY.md updated
- [ ] **docs/guide/** — user-facing workflows updated (new-host, concepts, testing)
- [ ] **docs/nixfleet/** — roadmap.yaml phase status, business docs if scope changed
- [ ] **CLAUDE.md** — module tree, flags tables, skills/agents tables accurate
- [ ] **README.md** — hosts, scopes, commands accurate
- [ ] Security findings → `.claude/security-reviews/YYYY-MM-DD.md` + `current.md`
- [ ] PR/commits reference relevant GitHub Issues (`Closes #XX`)
- [ ] Eval test assertions exist for new scopes/flags
- [ ] `.claude/rules/config-dependencies.md` chains checked

**If any item is missing, fix it before proceeding to push.**

### 6. Push
```
git push -u origin <branch>
```

### 6.5. Close Issues
If the branch addresses specific issues:
- Parse commit messages for `Closes #XX` or `Fixes #XX`
- If no explicit issue link, ask the user which issues this closes

### 7. Return to Main
```
git checkout main
```

### 7.5. Verification
Invoke `superpowers:verification-before-completion` skill:
- Run all relevant tests
- Confirm output shows passing
- Never claim success without evidence

### 8. Present for Manual Review
Present the branch summary:
- Branch name and remote URL
- Number of commits with messages
- Files changed (stat)
- Validate status

Say: "Branch `<branch>` pushed. Review and merge manually when ready:
```
git merge <branch> --no-edit && git push origin main
git branch -d <branch> && git push origin --delete <branch>
```"

**STOP HERE.** Do not merge. The user reviews and merges manually.

## Rules
- Never skip confirmations
- Never commit directly to main
- Never merge — push the branch, user merges manually
- Tests + docs MUST pass before push
