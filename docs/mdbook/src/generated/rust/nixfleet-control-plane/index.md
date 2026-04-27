# `nixfleet_control_plane`

NixFleet control plane.

Phase 2 shipped this as a oneshot reconciler runner: read
`fleet.resolved.json` + a hand-written `observed.json`, verify,
reconcile, emit the plan, exit. Phase 3 PR-1 turns the same binary
into a long-running TLS server: the existing [`tick`] function
becomes the body of a 30s `tokio::time::interval` loop inside a
new [`server`] module, and `GET /healthz` lights up as the first
axum endpoint. The `tick` subcommand is preserved for tests +
ad-hoc operator runs (see `src/main.rs`).

[`tick`] remains a pure function so the long-running serve loop
and the oneshot CLI share one verify-and-reconcile path. The
file-backed `--observed` flag stays as a dev/test fallback until
PR-4 introduces the live projection from agent check-ins.

## Items

### 🔓 `mod auth_cn`

_(no doc comment)_


### 🔓 `mod db`

_(no doc comment)_


### 🔓 `mod dispatch`

_(no doc comment)_


### 🔓 `mod forgejo_poll`

_(no doc comment)_


### 🔓 `mod issuance`

_(no doc comment)_


### 🔓 `mod observed_projection`

_(no doc comment)_


### 🔓 `mod rollback_timer`

_(no doc comment)_


### 🔓 `mod server`

_(no doc comment)_


### 🔓 `mod tls`

_(no doc comment)_


### 🔓 `struct TickInputs`

_(no doc comment)_


### 🔓 `struct TickOutput`

_(no doc comment)_


### 🔓 `enum VerifyOutcome`

_(no doc comment)_


### 🔓 `fn tick`

_(no doc comment)_


### 🔓 `fn render_plan`

Render a tick result as one summary JSON line plus one JSON line per
action. Each line is intended for the systemd journal — `journalctl
-o cat` produces the raw JSON; `jq` filters trivially.


