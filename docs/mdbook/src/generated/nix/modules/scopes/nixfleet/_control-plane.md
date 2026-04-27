# `modules/scopes/nixfleet/_control-plane.nix`

NixOS service module for the NixFleet control plane.

Phase 3 PR-1: long-running TLS server. The binary's `serve`
subcommand runs forever, accepts mTLS-authenticated connections (PR-2
adds the verifier), and ticks an internal reconcile loop every 30s.
This replaces Phase 2's oneshot+timer pair — `Type=oneshot` →
`Type=simple`, `systemd.timers.nixfleet-control-plane` is dropped.
The `tick` subcommand on the binary preserves Phase 2's CLI contract
for tests and ad-hoc operator runs.

Auto-included by mkHost (disabled by default). Enable on the
coordinator host (typically lab) only.

## Bindings

### `trustConfig`

Shared trust.json payload — see ./_trust-json.nix for shape rationale
and the orgRootKey ed25519 promotion that matches proto::TrustConfig.

### `initialObservedJson`

First-deploy bootstrap for observed.json — laid down via
systemd-tmpfiles `C` (copy only if path does not exist) so the
reconciler's first tick has a parseable file even before the
operator has hand-written one. PR-4 swaps this for an in-memory
projection from agent check-ins; this stays as the offline
dev/test fallback.

### `listenPort`

Parse the listen address into HOST:PORT for the firewall rule.

### `fleetCaCert`

PR-5: cert issuance (enroll + renew). The CP holds the fleet
CA private key online — see nixfleet issue #41 for the deferred
TPM-bound replacement. fleet/modules/nixfleet/tls.nix wires
these to agenix-decrypted paths.

### `closureUpstream`

Phase 4 PR-C: closure proxy upstream. Attic instance the CP
forwards /v1/agent/closure/<hash> requests to. Typically the
local attic on lab. When null, the endpoint returns 501.

### `dbPath`

Phase 4 PR-1: SQLite path. When set, the CP opens + migrates
the DB on startup. Token replay + cert revocations + (Phase 4
PR-2+) pending confirms + rollouts persist across CP restarts.
When null, in-memory state only — fine for dev, not production.

### `forgejo`

PR-4: Forgejo channel-refs poll. When set, the CP polls
/api/v1/repos/{owner}/{repo}/contents/{artifactPath} every 60s
and refreshes the in-memory channel-refs cache. Phase 4 may
extend this with a sibling poll for the .sig file (verify-on-
load) — for now the CP trusts the authenticated TLS channel
to Forgejo + Forgejo's RBAC.

