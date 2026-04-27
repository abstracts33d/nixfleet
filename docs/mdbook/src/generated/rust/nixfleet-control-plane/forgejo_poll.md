# `nixfleet_control_plane::forgejo_poll`

Forgejo poll loop for channel-refs (Phase 3 PR-4).

Replaces the hand-edited `/etc/nixfleet/cp/channel-refs.json`
default from PR-4's earlier design. Polls Forgejo's contents API
every 60s for `releases/fleet.resolved.json`, decodes the base64
body, runs the existing `verify_artifact` against it, and
refreshes an in-memory `channel_refs` cache.

Failure semantics: log warning + retain previous cache. CP does
not crash on Forgejo unavailability — operator can curl /healthz
and see when the last successful tick was even if Forgejo is
down.

## Items

### 🔓 `const POLL_INTERVAL`

Poll cadence — D9 default. Faster doesn't help (CI sign + push
latency dominates); slower delays the operator's "I pushed a
release commit, when does CP see it" feedback loop unhelpfully.


### 🔓 `struct ForgejoConfig`

Configuration for the poll task. All fields populated by the
CLI flags in main.rs.


### 🔒 `struct ContentsResponse`

Forgejo `/api/v1/repos/{o}/{r}/contents/{path}` response.
`content` is base64-encoded with `\n` chunked every 60 chars
(RFC 2045 / "MIME" encoding).


### 🔓 `struct ChannelRefsCache`

In-memory cache the reconcile loop reads from. Wrapped in
`Arc<RwLock<...>>` so concurrent reads are cheap; writes only
happen at poll cadence.


### 🔓 `fn spawn`

Spawn the poll task. Runs forever; logs warnings on failure;
updates the channel-refs cache + the verified-fleet snapshot on
success.

On each successful poll the task:
1. Fetches `releases/fleet.resolved.json` + its `.sig` from
   Forgejo (over HTTPS with the deployed cp-forgejo-token).
2. Reads `trust.json` fresh — operator key rotation propagates
   on the next poll, no CP restart required.
3. Runs `verify_artifact` (canonicalize + signature verify +
   schemaVersion gate + freshness check). Same path the
   reconcile loop's file-backed verifier uses.
4. Updates `verified_fleet` so the dispatch path's per-checkin
   decisions read fresh closureHashes.
5. Refreshes the channel_refs cache (kept for telemetry +
   `Observed.channel_refs` projection in the reconciler).

Failure semantics match the prior shape: log warn, retain
previous state. A transient Forgejo outage or a bad signature
must not blank out a previously-good snapshot — the operator
fixes the artifact, the next poll repopulates.


### 🔓 `fn prime_once`

One-shot synchronous fetch + verify, called once from `serve()`
**before** starting the reconcile loop or accepting connections.

Without this, the CP's first reconcile-loop prime falls back to
the compile-time `--artifact` path — which is always an older
release than what's on Forgejo (CI commits the [skip ci] release
AFTER building the closure, so each closure's bundled artifact
is the previous release). Agents check in immediately on CP
boot, before the periodic poll's first tick, and dispatch
returns a stale target — lab observed stair-stepping backwards
through deploy history during the GitOps validation pass.

Behaviour: this function tries the Forgejo path. On success the
caller stores the verified `FleetResolved` in `verified_fleet`.
On failure (network, verify, anything) the caller falls back to
the build-time artifact prime — same posture as before, just
with Forgejo as the preferred source when configured.


### 🔒 `fn fetch_repo_file`

Fetch a single file from a Forgejo repo via the Contents API.
Returns the raw decoded bytes (Forgejo serves base64 in its
`content` field; we strip the wrapping newlines + decode). One
helper shared between artifact + signature reads so the URL +
auth + decoding logic lives in one place.


