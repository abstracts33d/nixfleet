# `nixfleet_control_plane::server`

Long-running TLS server (Phase 3 PR-1 onwards).

axum router + axum-server TLS listener + internal reconcile loop
+ Forgejo poll. The slim entry point — `serve()` and
`build_router()` — is what `main.rs` calls; everything else lives
in the submodules:

- [`state`] — shared `AppState`, `ServeArgs`, helper types,
  constants
- [`middleware`] — `require_cn` (mTLS gate) + protocol-version
  middleware
- [`handlers`] — `/healthz` + `/v1/*` route handlers
- [`reconcile`] — background reconcile loop (verifies the
  build-time artifact every 30s, projects checkins → reconciler
  actions, writes the fleet snapshot under a freshness gate)

Originally this was one 1450-LOC file; split here for readability
and to keep each piece focused.

## Items

### 🔒 `fn build_router`

Build the axum router. `/healthz` lives outside the `/v1` namespace
so it doesn't go through the protocol-version middleware
(operator status probe should always reply, regardless of header
version drift). `/v1/*` is the agent-facing surface and gates on
the protocol version header.


### 🔒 `fn version_layer`

Thin adapter so the router only sees a free function. Forwards to
the protocol-version middleware in [`middleware`].


### 🔓 `fn serve`

Serve until interrupted. Builds the TLS config, opens the DB,
primes the verified-fleet snapshot from Forgejo (when configured),
starts the reconcile loop + the Forgejo poll task, binds the
listener, runs forever.


