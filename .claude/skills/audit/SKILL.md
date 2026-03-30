---
name: audit
description: Full codebase audit — config health, security, code quality, architecture, product gaps. Dispatches 5 agents sequentially, each builds on the previous.
user-invocable: true
---

# Full Codebase Audit

## Process

Each stage receives the findings of all previous stages as context. The pipeline is sequential so that later agents can reason over earlier results.

### Stage 1 — Config Health (config-manager)

Dispatch `config-manager` with:
- Check CLAUDE.md, README.md, TODO.md for staleness and drift
- Verify `.claude/rules/`, `.claude/agents/`, `.claude/skills/` tables match actual files
- Check `config-dependencies.md` chain completeness
- Assess rules coverage and open design questions
- Output: list of config health findings (severity, file, description)

### Stage 2 — Security Posture (security-reviewer)

Dispatch `security-reviewer` with the Stage 1 findings as context:
- Full security review per `.claude/rules/security-review.md` process
- Review secrets management, permission model, network, VM, supply chain, impermanence
- Write a timestamped report to `.claude/security-reviews/YYYY-MM-DD.md`
- Commit the report
- Output: security findings table (severity, file, description, status)

### Stage 3 — Code Quality (code-reviewer + rust-expert, parallel)

Dispatch `code-reviewer` AND `rust-expert` simultaneously with Stage 1+2 findings as context:
- `code-reviewer`: code quality, conventions, dependency chains, module patterns, scope hygiene
- `rust-expert`: Rust build quality, error handling, unsafe usage, dependency graph, idiomatic patterns
- Wait for both to complete
- Output: combined code quality findings

### Stage 4 — Architecture (architect)

Dispatch `architect` with all Stage 1–3 findings as context:
- Evaluate module organization, deferred module patterns, hostSpec flag design
- Identify structural debt, over-engineering, missing abstractions
- Review wrapper boundary compliance (`.claude/rules/wrapper-boundary.md`)
- Assess NixFleet scalability: does the current architecture support fleet management at scale?
- Output: architecture findings + structural recommendations

### Stage 5 — Product Gaps (product-analyst)

Dispatch `product-analyst` with all Stage 1–4 findings as context:
- Map existing features to client needs and NixFleet tier requirements
- Identify feature gaps per tier (starter / team / enterprise)
- Surface opportunities based on code and architecture findings
- Cross-reference open GitHub Issues: `gh issue list --state open --json number,title,labels`
- Output: product gap analysis with tier mapping

### Stage 6 — Consolidation

Merge all findings across 5 domains. Deduplicate overlapping items. Sort by priority:
1. Critical / High security findings
2. High-impact architecture changes
3. Code quality blockers
4. Product gaps for current tier commitments
5. Config / doc drift

### Stage 7 — Present

```
## Audit Report — YYYY-MM-DD

### Summary
- Config: N findings
- Security: N findings (M written to .claude/security-reviews/)
- Code Quality: N findings
- Architecture: N findings
- Product: N gaps

### Top 3 Actions
1. [Priority 1] ...
2. [Priority 2] ...
3. [Priority 3] ...

### Full Findings
| Domain | Severity | File | Description |
...
```

## Chaining

After presenting, ask: "Which finding do you want to address first?"

Chain to:
- Security finding → `/security`
- Code/architecture issue → `/feature` or dispatch `code-reviewer` for fix plan
- Product gap → `/feature`
- Config drift → dispatch `doc-writer`
- Multiple independent fixes → `/batch`

## Verification

Before presenting results, invoke `superpowers:verification-before-completion`:
- Show that each agent ran and produced output
- Never summarize without evidence from each stage
- Security report must exist on disk before claiming it was written
