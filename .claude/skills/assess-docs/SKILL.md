---
name: assess-docs
description: Meta-level documentation review — questions coherence, design choices, staleness, and cross-document consistency. Use after major changes, before releases, or when documentation quality is uncertain.
user-invocable: true
---

# Assess Documentation

Meta-level review of all documentation for coherence and quality.

## Process

1. **Dispatch docs-assessor agent** with full repo scope
2. Agent reviews:
   - Cross-document coherence (CLAUDE.md <-> README <-> docs/src/ <-> docs/guide/ <-> TODO <-> TECHNICAL)
   - Design choice validity (documented architecture vs actual code)
   - Staleness (TODO items, outdated references, dead links)
   - Completeness (every module/host/scope/skill/agent documented everywhere)
3. **Present** assessment with score and findings
4. **If issues found**: Offer to dispatch `doc-writer` to fix them
5. **If design questions raised**: Present for user discussion

## When to use
- After `/docs-generate` — verify quality of generated docs
- Before major deploys — ensure docs match what's shipping
- In `/suggest` — as part of repo health scan
- In `/review` — verify docs match code changes
- Standalone — periodic documentation health check

## Integration
This skill is automatically dispatched by:
- `/ship` (Step 5.5, after doc-writer)
- `/suggest` (parallel scan)
- `/review` (alongside code + security review)
