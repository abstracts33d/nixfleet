---
name: batch
description: Decompose a task into N parallel subtasks in isolated worktrees. Use for multi-file refactors or independent changes.
user-invocable: true
---

# Batch Parallel Execution

## Process

1. **Analyze** the task description
2. **Decompose** into N independent subtasks (2-10 recommended)
3. **Present** the decomposition:
   ```
   Subtask 1: [description] → worktree-1
   Subtask 2: [description] → worktree-2
   ...
   ```
4. **Ask for confirmation** before spawning agents
5. **Spawn N subagents** each with `isolation: worktree`:
   - Each implements their subtask
   - Each runs `test-runner` to verify
   - Each runs `doc-writer` if docs affected
6. **Collect results**: List branches created, pass/fail per subtask
7. **Present**: Summary with links to review each branch
8. **User merges** chosen branches via `/ship`

## Rules
- Subtasks MUST be independent (no shared state)
- Each subtask produces exactly one worktree branch
- Failed subtasks don't block successful ones
- User decides which branches to merge
