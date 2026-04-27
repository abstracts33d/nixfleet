# `modules/scopes/nixfleet/_agent.nix`

NixOS service module for the NixFleet fleet agent (v0.2 contract).

Linux-only. Poll-only agent that reads a trust-root declaration from
/etc/nixfleet/agent/trust.json and talks to the control plane over
mTLS. Reload model is restart-only (docs/trust-root-flow.md §7.1) —
nixos-rebuild switch changes the etc entry content, systemd restarts,
the binary re-reads on startup.

v0.1 surface (tags, healthChecks, metricsPort, dryRun, allowInsecure,
cacheUrl, healthInterval) was removed in #29 as part of the v0.2
migration. The v0.2 agent is intentionally minimal; health, metrics,
and cache concerns move out of the agent binary in this contract.

Auto-included by mkHost (disabled by default).

## Bindings

### `trustConfig`

Materialise config.nixfleet.trust into the v0.2 proto::TrustConfig
JSON shape (crates/nixfleet-proto/src/trust.rs). schemaVersion = 1
is required per docs/trust-root-flow.md §7.4 — binaries refuse to
start on unknown versions.

Shared trust.json payload — see ./_trust-json.nix for shape rationale
and the orgRootKey ed25519 promotion that matches proto::TrustConfig.

### `bootstrapTokenFile`

PR-5: bootstrap token for first-boot enrollment. When set, and
`tls.clientCert` doesn't exist yet on disk, the agent reads
this token, generates a CSR, POSTs /v1/enroll, and writes the
issued cert + key to the configured paths before entering its
poll loop. fleet/modules/secrets/nixos.nix wires this to an
agenix-decrypted `bootstrap-token-${hostname}` path.

