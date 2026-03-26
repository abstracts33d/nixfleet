---
name: plan-and-execute
description: End-to-end feature implementation — research, brainstorm, spec, plan, execute. Semi-autonomous by default (3 checkpoints), --auto skips all checkpoints except ship.
user-invocable: true
argument-hint: "[--auto] <feature description>"
---

# Plan and Execute

Full pipeline: research → brainstorm → spec → plan → execute → ship.

## Modes

### Semi-autonomous (default) — 3 checkpoints only
1. After Phase 1a (research) — user picks direction
2. After Phase 2 (spec) — user validates design
3. After Phase 3 (plan) — user approves execution

**NO other interruptions.** Technical choices (stack, libs, patterns) are made internally using the recommended approach. Never ask "which framework?" or "merge?" during execution.

### Autonomous (`--auto`) — 0 checkpoints
Runs everything, stops only at ship (branch pushed, user merges manually).

```
/plan-and-execute add bluetooth scope
/plan-and-execute --auto fix the xdg-portal warning
```

## Phases

### Phase 1a: Research (fast, ~2 min)
1. Parse `$ARGUMENTS` — detect `--auto` flag, extract feature description
2. Quick scan: read CLAUDE.md, relevant `.claude/rules/`, existing modules in the area
3. Research alternatives, available options, trade-offs
4. Check alignment with core doctrine (rules, conventions, platform design)
5. Present a concise summary:
   - 2-3 approaches with trade-offs
   - Recommended approach with rationale
   - Alignment notes (which rules apply)

**CHECKPOINT 1 (semi-auto):** "Here are the options. Which direction?" or "I recommend X, proceed?"
- In `--auto`: pick recommended, log the choice, continue

### Phase 1b: Deep brainstorm
1. Based on user's chosen direction (or auto-recommended)
2. Explore the codebase deeply: read affected files, understand current patterns
3. Identify dependencies, impacts, edge cases
4. Produce a clear feature description with scope boundaries

### Phase 2: Design (spec)
1. Dispatch `spec-writer` agent → `docs/superpowers/specs/YYYY-MM-DD-<feature>-design.md`
2. Spec includes: context, architecture, detailed design, testing strategy, files summary

**CHECKPOINT 2 (semi-auto):** Present spec summary, wait for "looks good"
- In `--auto`: proceed immediately

### Phase 3: Plan
1. Dispatch `plan-writer` agent → `docs/superpowers/plans/YYYY-MM-DD-<feature>.md`
2. Task-by-task with exact files, code, commands, commits
3. Identify which tasks are independent (for parallel execution)

4. **Create tracking issue**:
   - Run: `source scripts/gh-issue-helper.sh && gh_create_issue "<feature title>" "<spec link + plan summary>" "<labels>" "<milestone>"`
   - Transition to Ready: `source scripts/gh-issue-helper.sh && gh_transition_issue <number> planned`
   - Add `tracking: <issue URL>` frontmatter to the spec file (URL from `gh issue view <number> --json url -q .url`)

**CHECKPOINT 3 (semi-auto):** Present plan summary with task count and parallelism plan, wait for "go"
- In `--auto`: proceed immediately

### Phase 4: Execute
1. Transition to In Progress: `source scripts/gh-issue-helper.sh && gh_transition_issue <number> started`
2. Create git worktree (`superpowers:using-git-worktrees`)
3. Analyze task dependencies → identify parallel groups
4. Execute via `superpowers:subagent-driven-development`:
   - Independent tasks → `superpowers:dispatching-parallel-agents` (MANDATORY)
   - Dependent tasks → sequential
5. Each subagent uses `superpowers:test-driven-development`
6. After all tasks: dispatch `test-runner` → validate
7. After all tasks: dispatch `doc-writer` → update docs trees

**NO CHECKPOINTS during execution.** Fix errors internally, retry up to 3 times.

### Phase 5: Ship
1. Invoke `superpowers:verification-before-completion` — evidence before claiming done
2. Invoke `superpowers:requesting-code-review` — quality gate
3. Invoke `/ship` skill — push branch for manual review
4. Dispatch `/suggest` for post-push analysis

**STOP.** User merges manually.

## Behavior Matrix

| Step | Semi-auto | Auto |
|------|-----------|------|
| Phase 1a research | Present options → CHECKPOINT | Auto-pick recommended |
| Phase 1b brainstorm | Automatic | Automatic |
| Phase 2 spec | CHECKPOINT | Automatic |
| Phase 3 plan | CHECKPOINT | Automatic |
| Phase 4 execute | Automatic (parallel where possible) | Automatic |
| Phase 5 ship | Push branch, user merges | Push branch, user merges |

## Strict Rules
- In semi-auto: EXACTLY 3 checkpoints. Never more.
- Never ask technical questions mid-flow ("which lib?", "which pattern?") — make the decision, document it in the spec
- Never ask "merge?" — /ship handles that
- Parallelism is MANDATORY for independent tasks
- Tests + docs are ALWAYS baked in
- Spec and plan are always committed (audit trail)
- `--auto` does NOT skip validation, only human checkpoints
