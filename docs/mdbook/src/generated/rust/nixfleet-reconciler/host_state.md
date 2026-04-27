# `nixfleet_reconciler::host_state`

Per-host state machine handling (RFC-0002 §3.2).

Given a wave's host list, the reconciler's per-rollout state, and
supporting context, emit the set of actions for each host and track
whether the wave as a whole is soaked (all hosts in terminal ok states).

