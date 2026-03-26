# Superpowers Skill Enforcement

Use the right superpowers skill for the right situation. These are NOT optional.

## Mandatory Skill Triggers

| Situation | Required Skill | Why |
|-----------|---------------|-----|
| Start of any session | `using-superpowers` | Establishes skill awareness |
| Before any creative/feature work | `brainstorming` | Design before code |
| After brainstorming produces a spec | `writing-plans` | Structured task decomposition |
| Executing a plan with independent tasks | `subagent-driven-development` | Fresh context per task |
| 2+ independent tasks identified | `dispatching-parallel-agents` | Parallel execution |
| Before claiming "done", "works", "passes" | `verification-before-completion` | Evidence before assertions |
| Any bug, test failure, unexpected behavior | `systematic-debugging` | Root cause before fix |
| Before any code implementation | `test-driven-development` | Tests before code |
| Before merge/ship | `requesting-code-review` | Quality gate |
| Feature branch ready | `finishing-a-development-branch` | Structured completion |
| Starting feature work | `using-git-worktrees` | Isolated workspace |
| Creating/editing skills | `writing-skills` | Verify before deploy |
| Receiving review feedback | `receiving-code-review` | Technical rigor |
| After any code change that affects structure | `/docs-generate` (via doc-writer) | Both doc trees must stay in sync |

## Parallelism (HARD REQUIREMENT)

Before dispatching subagents for any plan:
1. Analyze task dependencies — which tasks share files? Which depend on each other's output?
2. If 2+ tasks are independent → dispatch them in parallel using multiple Agent tool calls in a SINGLE message
3. NEVER batch independent tasks into one sequential agent
4. Use `superpowers:dispatching-parallel-agents` skill

**Example decomposition:**
- Task 1 (scaffold) → sequential (others depend on it)
- Tasks 2+3 (independent files) → PARALLEL
- Tasks 4+5 (independent features) → PARALLEL
- Task 6 (touches shared files from 2-5) → sequential after 2-5

## Agent Levels (HARD RULE)

**User interactions → high-level skills ONLY.** Never dispatch low-level agents directly from user requests.

### High-level (user-facing skills — these orchestrate agents)
| Skill | User says | Dispatches |
|-------|-----------|------------|
| `/suggest` | "what should I do?" | code-reviewer + security + nix-expert + docs-assessor |
| `/audit` | "audit the codebase" | config-manager → security → code → architect → product |
| `/feature` | "add feature X" | analyst → architect → spec → plan → implement → review |
| `/review` | "review the code" | code-reviewer + security + docs-assessor |
| `/ship` | "ship this" | test-runner → doc-writer → config-manager → docs-assessor |
| `/health` | "check health" | config-manager + test-runner + devops |
| `/onboard` | "add org X" | analyst → architect → nix → fleet-ops → docs |
| `/incident` | "X is broken" | fleet-ops → nix → security → architect |
| `/scope` | "add scope X" | scaffolds + doc-writer + test-runner |
| `/security` | "security audit" | security-reviewer |
| `/plan-and-execute` | "implement X" | research → spec → plan → execute → ship |

### Low-level (specialist agents — dispatched BY skills, not BY users)
`nix-expert`, `rust-expert`, `security-reviewer`, `code-reviewer`, `test-runner`, `doc-writer`, `docs-assessor`, `config-manager`, `architect`, `product-analyst`, `fleet-ops`, `devops`, `spec-writer`, `plan-writer`, `integration-tester`

**When a user asks for something, map it to a skill first.** If no skill matches, THEN dispatch an agent directly — but explain why no skill covers this case.

## Agent ↔ Skill Enforcement

### Skills MUST dispatch the right agents
| Skill | Required Agent(s) |
|-------|-------------------|
| `/suggest` | code-reviewer + security-reviewer + nix-expert + docs-assessor (parallel) |
| `/review` | code-reviewer + security-reviewer + docs-assessor (parallel) |
| `/ship` | test-runner + doc-writer + docs-assessor |
| `/security` | security-reviewer |
| `/scope` | doc-writer + test-runner |
| `/diagnose` | nix-expert or test-runner |
| `/plan-and-execute` | spec-writer + plan-writer + test-runner + doc-writer |

### Agents MUST use available skills
| Agent | Must use skill when... |
|-------|----------------------|
| Any implementation agent | `test-driven-development` — write test before code |
| Any agent making code changes | `verification-before-completion` — prove it works |
| doc-writer | Check ALL 7 doc trees (see agent definition) |
| security-reviewer | Write timestamped report + `current.md` |
| code-reviewer | Use `requesting-code-review` template |

### Board transitions MUST happen
| Event | Agent/Skill responsible | Transition |
|-------|------------------------|------------|
| Issue created | `gh_create_issue` | → Backlog |
| Plan approved | `/plan-and-execute` Phase 3 | → Ready |
| Work starts | `/plan-and-execute` Phase 4 | → In Progress |
| PR created | `/ship` Step 3.5 | → In Review |
| PR merged | `/ship` Step 6.5 | → Done |

## Workflow Convention (HARD RULE)

**ALWAYS follow the complete plan-and-execute flow. NEVER skip phases.**

1. Research → CHECKPOINT (present options, wait for user)
2. Spec → CHECKPOINT (present summary, wait for user)
3. Plan → CHECKPOINT (present task list, wait for user)
4. Execute → no checkpoints (fix internally)
5. **STOP before shipping** → present branch summary, ask "review OK, can I ship?"
6. Only push/create PR after explicit user confirmation
7. **NEVER merge** — present PR URL, user merges manually

This applies to EVERY task, not just big features. Even a 5-line fix follows: present → confirm → push.

## Anti-patterns (never do these)

- **Skip workflow checkpoints** — this is the #1 violation. Never compress research+spec+plan into one step
- **Ship without asking** — ALWAYS stop and ask "review OK, can I ship?" before pushing
- **Merge PRs automatically** — always present PR URL, let user merge
- **Batch independent tasks into one sequential agent** — parallelize them
- Claim "build OK" without running the build and showing output
- Debug by guessing instead of using systematic-debugging
- Write implementation before tests
- Start coding without brainstorming for non-trivial features
- Execute plans without subagent-driven-development
- Ship code without updating ALL doc trees
- Add a new scope/host/app without a corresponding docs/src/ entry
- Move issues on the board manually — use `gh_transition_issue`
- Push directly to main — always use feature branches + PRs
