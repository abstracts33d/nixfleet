# `nixfleet_control_plane::rollback_timer`

Magic rollback deadline tracker (Phase 4 PR-B).

Periodic background task: every 30s, scan `pending_confirms` for
rows whose `confirm_deadline` has passed but `state` is still
`'pending'`. Transition each to `'rolled-back'` and emit a
journal line. The agent learns the rollout was rolled back via
its next `/v1/agent/checkin` (the CP would normally include
`target = null` and a separate signal — Phase 4 dispatch loop
adds that signal).

This task is the CP-side half of magic rollback (issue #2). The
agent-side half is in `nixfleet-agent`'s activation loop (parallel
PR): on a missed confirm window, the agent locally runs
`nixos-rebuild --rollback` to revert to the previous boot
generation. Both halves work independently — the CP marks state
regardless of whether the agent successfully rolled back, so the
operator's view via the CP's audit trail is always correct.

## Items

### 🔓 `const ROLLBACK_TIMER_INTERVAL`

How often the timer wakes up. 30s matches the reconcile-loop
cadence (D2). Faster means quicker detection of missed confirms;
slower reduces journal noise. 30s is a fine default for the
homelab fleet.


### 🔓 `fn spawn`

Spawn the periodic rollback-timer task. Runs forever; logs at
info on each rollback transition, debug otherwise.


