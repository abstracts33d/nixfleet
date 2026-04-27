# `nixfleet_control_plane::server::state`

Shared state + configuration types for the long-running server.

Pulled out of the monolithic `server.rs` so the handler /
middleware / reconcile-loop modules can each take a thin
dependency on `AppState` without dragging the whole serve()
surface along. Public re-export from `server::mod` keeps the
crate's external API unchanged.

## Items

### 🔐 `const REPORT_RING_CAP`

Per-host event ring buffer cap. Phase 3's `/v1/agent/report` is
in-memory only — Phase 4 polish wires the agent to actually emit
reports, but persistence (SQLite-backed `host_reports` table) is
still pending. 32 entries is enough to spot a flapping host
without unbounded memory growth.


### 🔐 `const NEXT_CHECKIN_SECS`

Returned to the agent in `CheckinResponse.next_checkin_secs`.
Default 60s. The dispatch loop doesn't currently shape this
per-host; future load-shaping (RFC §5) hashes hostname into a
poll slot.


### 🔐 `const RECONCILE_INTERVAL`

Reconcile loop cadence — D2 default. Operator-visible drift (host
failed to check in) shows up in the journal within one cycle;
slow enough not to spam.


### 🔐 `const CONFIRM_DEADLINE_SECS`

Time the dispatch loop gives an agent to fetch + activate +
confirm a target before the magic-rollback timer marks the
pending row as `rolled-back`. 120s is the spec-D1 default —
enough headroom for a closure download + activation, short enough
that a stuck agent surfaces in the journal within one rollback-
timer tick.


### 🔓 `struct ServeArgs`

Inputs the `serve` subcommand receives from clap.


### 🔓 `struct HostCheckinRecord`

Most-recent checkin per host. PR-4's projection feeds this into
the reconciler.


### 🔓 `struct ReportRecord`

In-memory record of an event report. Bounded ring buffer per
host (cap = [`REPORT_RING_CAP`]). DB-backed persistence is
deferred to Phase 5.


### 🔓 `struct ClosureUpstream`

Closure-proxy upstream client + URL. Captured at serve() time
so each request avoids re-parsing the URL or rebuilding the
reqwest client.


### 🔓 `struct IssuancePaths`

Issuance paths. Stored on `AppState` so handlers can read them
at request time.


### 🔓 `struct AppState`

Server-wide shared state.

`db` is `Option<Arc<Db>>` so file-backed deploy + tests run
without standing up SQLite. Production deploys wire it via
`--db-path`.

`verified_fleet` and `channel_refs_cache` are both `Arc<RwLock<>>`
so the Forgejo poll task can write through them directly without
a mirror task. The reconcile loop's per-tick build-time verify
uses a `signed_at` freshness gate before overwriting, so the
Forgejo-fresh snapshot is preserved.


