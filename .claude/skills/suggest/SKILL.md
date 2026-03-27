---
name: suggest
description: Analyze repo state and suggest prioritized improvements or next tasks. Use at the start of a session or when unsure what to work on.
user-invocable: true
---

# Suggest Next Actions

## Process

1. **Parallel scan** — dispatch 4 agents simultaneously:
   - `code-reviewer`: scan for code quality issues, tech debt, improvable patterns
   - `security-reviewer`: check drift since last audit in `.claude/security-reviews/`
   - `nix-expert`: check for outdated inputs, unused flags, simplifiable modules
   - `docs-assessor`: documentation coherence, staleness, design drift

2. **Read context**:
   - `gh issue list --state open --limit 20 --json number,title,labels` → issue overview
   - `gh issue list --label "urgency:now" --state open` → urgent items
   - `gh issue list --label "impact:critical" --state open` → critical items
   - `git log --oneline -10` → recent work momentum
   - `git status` → uncommitted changes

3. **Synthesize** suggestions list, each with:
   - **Description**: What to do
   - **Impact**: High/Medium/Low
   - **Effort**: Quick-win / Afternoon / Multi-day
   - **Type**: Security / Quality / DX / Feature / Maintenance

4. **Present top 3** with rationale for the ranking

5. **Ask**: "Which one do you want to tackle?"

6. **Chain** to the appropriate skill:
   - Security finding → `/security`
   - Code quality → dispatch `code-reviewer` for detailed fix plan
   - New feature → `/scope`
   - Multiple independent fixes → `/batch`
   - Build issue → `/diagnose`

   When chaining to a skill for a tracked issue, transition to In Progress:
