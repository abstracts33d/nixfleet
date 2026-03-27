# Anti-Patterns

Common mistakes to avoid. These are the most frequent violations observed in practice.

## Workflow violations

- **Skip workflow checkpoints** — the #1 violation. Never compress research+spec+plan into one step. Each phase gets user confirmation.
- **Ship without asking** — ALWAYS stop and ask "review OK, can I ship?" before pushing.
- **Merge PRs automatically** — always present PR URL, let user merge manually.
- **Push directly to main** — always use feature branches + PRs.

## Agent dispatch violations

- **Batch independent tasks sequentially** — if 2+ tasks don't share files or depend on each other, dispatch parallel agents in a SINGLE message.
- **Dispatch low-level agents from user requests** — map to a skill first. Only dispatch agents directly when no skill matches (and explain why).

## Quality violations

- **Claim "build OK" without evidence** — run the build, show the output. Evidence before assertions.
- **Debug by guessing** — use systematic-debugging skill. Root cause before fix.
- **Write implementation before tests** — TDD: test assertions first, implementation second.
- **Start coding without design** — non-trivial features need brainstorming first.

## Documentation violations

- **Ship without updating docs** — a feature is not shippable if doc trees are stale.
- **Add scope/host/app without docs** — every new module needs a corresponding docs/src/ entry.
