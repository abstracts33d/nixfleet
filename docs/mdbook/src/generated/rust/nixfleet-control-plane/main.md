# `nixfleet_control_plane`

`nixfleet-control-plane` — CLI shell.

Two subcommands:

* `serve` (default) — long-running TLS server. axum + tokio +
  axum-server. Internal 30s reconcile loop. Phase 3 PR-1 ships
  `GET /healthz`; PR-2+ light up the agent endpoints.

* `tick` — Phase 2's oneshot behaviour: read inputs, verify,
  reconcile, print plan, exit. Preserved for tests + ad-hoc
  operator runs (handy for diffing what the loop is doing
  without tailing journald).

Exit codes for `tick` (preserved from Phase 2):
- 0 — verify ok, plan emitted (the plan may be empty — no drift).
- 1 — verify failed; one summary line emitted with the reason.
- 2 — input/IO/parse error before verify could run.

`serve` runs until interrupted; exit code 0 on graceful shutdown,
non-zero if startup (cert load, port bind) fails.

