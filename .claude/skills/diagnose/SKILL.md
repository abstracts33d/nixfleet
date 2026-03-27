---
name: diagnose
description: Analyze build/test failures and propose fixes. Use when nix build fails, tests fail, or runtime errors occur.
user-invocable: true
---

# Diagnose

## Process

0. Invoke `superpowers:systematic-debugging` — structured root cause analysis before proposing any fix
1. **Detect error type** from the output:
   - Nix evaluation error → dispatch `nix-expert`
   - Build failure → dispatch `nix-expert`
   - Test failure (eval/VM) → dispatch `test-runner`
   - Runtime error → dispatch `nix-expert` with logs
2. **Agent analyzes** the error:
   - Reads the full trace
   - Checks known patterns from its memory
   - Identifies the root cause
   - Proposes a specific fix (file, line, code)
3. **Present** the diagnosis and proposed fix
4. **Ask confirmation** before applying
5. **If applied**: Re-run the failing test/build to verify
6. **If still fails**: Iterate (max 3 attempts, then escalate to user)

## Error patterns
The nix-expert and test-runner agents accumulate error patterns in their memory. Over time, common issues are diagnosed faster.
