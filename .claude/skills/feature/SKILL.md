---
name: feature
description: End-to-end feature flow — from client need to shipped code. Chains product analysis, architecture, spec, plan, implementation, review.
user-invocable: true
argument-hint: "<feature description>"
---

# End-to-End Feature Flow

## Input

The user provides a feature description as the argument. If not provided, ask: "What feature do you want to build?"

## Process

### Stage 1 — Product Validation (product-analyst)

Dispatch `product-analyst` with the feature description:
- Is this feature aligned with client needs?
- Which NixFleet tier does it belong to (starter / team / enterprise)?
- Does a GitHub Issue already track this? `gh issue list --search "<feature>" --state open`
- Is there prior art in the codebase (existing scope, module, or skill)?
- Output: validation summary + tier classification + issue number (if found)

If no issue exists, create one:
```
gh issue create --title "<feature>" --label "type:feature,impact:medium" --body "<product-analyst summary>"
```

Transition issue to In Progress:
```
```

### Stage 2 — Architecture Fit (architect)

Dispatch `architect` with the Stage 1 output:
- Where does this feature live in the module tree?
- Does it need a new hostSpec flag? A new scope? A new agent? A new skill?
- What are the dependency chains? (ref `.claude/rules/config-dependencies.md`)
- What platforms does it affect? (NixOS / Darwin / portable)
- Output: architecture proposal with file list and dependency map

### Stage 3 — Design (brainstorming)

Invoke `superpowers:brainstorming` with the Stage 1+2 context:
- Present the product validation and architecture proposal to the user
- Explore design alternatives
- Agree on approach before writing code
- This step requires user interaction — do not skip

### Stage 4 — Spec (spec-writer)

Dispatch `spec-writer` with the brainstorming output:
- Write a design spec in `docs/nixfleet/specs/YYYY-MM-DD-<feature-slug>.md`
- Include: motivation, approach, interface design, open questions
- Output: spec file path

### Stage 5 — Plan (writing-plans)

Invoke `superpowers:writing-plans` with the spec:
- Decompose into tasks
- Identify dependencies between tasks
- Mark tasks that can run in parallel
- Output: implementation plan with task list

### Stage 6 — Implementation (subagent-driven-development)

Invoke `superpowers:subagent-driven-development` with the plan:
- Independent tasks → dispatch in parallel (HARD REQUIREMENT)
- Sequential tasks → dispatch after their dependencies complete
- Each subagent receives: spec, plan, relevant context, task description
- Ref: `superpowers:dispatching-parallel-agents` for parallelism rules

### Stage 7 — Review (code-reviewer + security-reviewer, parallel)

Dispatch `code-reviewer` AND `security-reviewer` simultaneously:
- Review all changes made during implementation
- `git diff main --stat` for scope
- Wait for both results
- If critical findings: fix before continuing

Invoke `superpowers:requesting-code-review` to ensure review quality gate is met.

### Stage 8 — Tests (test-runner)

Dispatch `test-runner`:
- Run eval tests: `nix flake check --no-build`
- If runtime behavior: run VM tests `nix run .#validate -- --vm`
- Fix any failures before continuing
- Output: test results with pass/fail evidence

### Stage 9 — Docs (doc-writer)

Dispatch `doc-writer`:
- Update CLAUDE.md if new flags, agents, or skills were added
- Update README.md hosts/scopes tables if applicable
- Add/update `docs/src/` page for the feature
- Add/update `docs/guide/` entry if user-facing
- Ensure both doc trees are in sync (per `superpowers-enforcement.md`)

### Stage 10 — Ship

Invoke `/ship`:
- Pre-push validation, format check, commit, push branch
- Link PR to issue with `Closes #<number>`

## Verification

Before claiming the feature is done, invoke `superpowers:verification-before-completion`:
- Show test output proving tests pass
- Show that docs were updated (list changed files)
- Show that the PR was created with the correct issue link
- Never claim "works" or "done" without evidence
