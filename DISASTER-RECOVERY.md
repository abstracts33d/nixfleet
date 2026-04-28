# Disaster recovery — destroying the control plane

**Status:** living document. **Owner:** open. **Last updated:** 2026-04-28.

This is the operator runbook for the load-bearing claim of `ARCHITECTURE.md` §1 / §8 done-criterion #1:

> Destroying the control plane is an outage, not a breach. Rebuilding the CP from the flake and the signed artifacts in storage gives you back the same fleet within one reconcile cycle.

Validation lives in `tests/harness/scenarios/teardown.nix` (`checks.x86_64-linux.fleet-harness-teardown`). The recovery time target is **under 5 minutes** for a 10-host fleet, **under 15 minutes** for 50 hosts.

## When to use this

- Storage corruption on the CP host (SQLite WAL split-brain, disk failure).
- Suspected CP compromise (you want to wipe and start clean from signed artifacts).
- Migrating CP between hosts (the procedure is identical to "rebuild from empty"; the destination CP boots with empty state and repopulates from agents + signed artifacts).
- Validating done-criterion #1 in a production-like environment.

## What gets recovered automatically

Every CP-resident table classifies as either soft state (recoverable from agent inputs on next checkin cycle) or hard state (recoverable from signed artifacts in git). See `ARCHITECTURE.md` §6 Phase 10 for the full classification.

| Table | Class | Recovery |
|---|---|---|
| `host_checkins` (in-memory) | soft | repopulates as agents check in (poll cadence determines completion time) |
| `pending_confirms` | soft | gap A's orphan-confirm path: agents whose `closure_hash` matches the verified target get a synthetic `confirmed` row + Healthy marker without a local rollback |
| `host_rollout_state` | soft | gap B's `last_confirmed_at` attestation: agents echo their last-confirm timestamp on every checkin; CP repopulates `last_healthy_since` clamped to `min(now, attested)` |
| `token_replay` | soft | bounded by 24h bootstrap-token TTL; loss extends the replay window by at most that duration |
| `cert_revocations` | **hard** | gap C's signed `revocations.json` sidecar: CP fetches + verifies + replays into the table on every reconcile tick |

Trust roots (`trust.json`) come from the flake itself — rebuilt as part of the closure.

## Procedure

### Step 0 — pre-flight checks

Confirm before destroying state:

1. **The flake's signed artifacts are reachable.** The CP needs `fleet.resolved.json` + `fleet.resolved.json.sig` + (optionally) `revocations.json` + `revocations.json.sig` available from the upstream URLs configured in `--channel-refs-artifact-url` / `--revocations-artifact-url`. If those are unreachable (Forgejo down, network partition), the CP will boot but stall on its first reconcile tick.
2. **The build-time artifact path is intact.** This is the fallback prime path (`--artifact` / `--signature` / `--trust-file`). If both the upstream and the build-time path are unreadable, the CP cannot recover at all — it has nothing to verify against.
3. **The fleet CA cert + key are available.** `--fleet-ca-cert` / `--fleet-ca-key` (typically under `/etc/nixfleet/cp/`). Without these, `/v1/enroll` and `/v1/agent/renew` return 500. Existing agents continue to work (their certs are valid until expiry); first-boot enrollment of new hosts blocks until the CA is restored.
4. **At least one agent is currently online.** Recovery completes "within one reconcile cycle" against agents that are checking in. Offline hosts repopulate when they come back online.

If any pre-flight check fails, **do not proceed with the wipe** — fix the prerequisite first.

### Step 1 — stop the CP service

```bash
systemctl stop nixfleet-control-plane.service
```

The service stops within ~5s. Active mTLS connections are dropped; agents see connection-refused on their next checkin and retry on the next poll tick.

### Step 2 — wipe the SQLite database

```bash
rm -rf /var/lib/nixfleet-cp/state.db /var/lib/nixfleet-cp/state.db-wal /var/lib/nixfleet-cp/state.db-shm
```

Deletes the main DB file plus the WAL + shared-memory files. Leave `audit.log` in place if present — it's an append-only audit record, not state-of-record, and the operator may want it for post-incident review.

> **Do not** delete `/etc/nixfleet/cp/trust.json`, `/etc/nixfleet/cp/fleet-ca-*.pem`, or any path under `/etc/nixfleet/cp/`. Those are flake-provided trust roots; deleting them turns recovery from "outage" into "breach" — the fleet's identity material is gone.

### Step 3 — restart the CP

```bash
systemctl start nixfleet-control-plane.service
```

The CP restarts and:

1. Opens a fresh SQLite database (refinery applies all migrations into the empty file in milliseconds).
2. Reads `trust.json` from `--trust-file`.
3. Tries the upstream poll first (when `--channel-refs-artifact-url` is set); falls back to the build-time `--artifact` if the upstream is unreachable. Either way, it primes `verified_fleet` from a verified signed artifact within the first reconcile tick (~30s default).
4. If `--revocations-artifact-url` is set, fetches + verifies + replays the signed revocations into `cert_revocations`. Same fallback — if the upstream is unreachable on first boot, the table stays empty until the next successful poll.
5. Starts the rollback timer + reconcile loop.
6. Begins accepting agent checkins.

### Step 4 — observe agent recovery

Watch the journal:

```bash
journalctl -u nixfleet-control-plane.service -f | grep checkin
```

Each agent's first post-restart checkin emits `checkin received hostname=<name>`. The recovery target is **every online agent checks in within one full poll interval** of CP restart. With the production default of 60s polling, expect full repopulation within ~70-120s.

The `host_checkins` projection (in-memory) repopulates as those checkins land. The reconciler tick consumes the projection on its next 30s cycle.

### Step 5 — verify recovery is complete

The completion criteria for done-criterion #1:

```bash
# 1. CP is healthy and the verified-fleet snapshot is primed.
curl -sk https://localhost:8443/healthz | jq '.last_tick_at != null'
# expected: true (within 30s of CP restart)

# 2. The verified-fleet snapshot's signed_at is fresh.
# `/v1/channels/{name}` requires mTLS — any valid fleet client
# cert + key pair works. Substitute the paths your operator
# tooling uses (e.g. an admin's mTLS pair, or one of the agents'
# certs for ad-hoc debugging — the framework does not yet ship a
# dedicated operator-cert workflow; that lives in #29's CLI scope).
curl -sk \
  --cacert /etc/nixfleet/cp/ca.pem \
  --cert <CLIENT_CERT_PEM> \
  --key <CLIENT_KEY_PEM> \
  https://localhost:8443/v1/channels/stable | jq '.signed_at'
# expected: rfc3339 timestamp matching the latest signed fleet.resolved

# 3. cert_revocations replays successfully (when the signed sidecar is configured).
journalctl -u nixfleet-control-plane.service --since='5 min ago' \
  | grep -E 'revocations poll|cert_revocations'
# expected: at least one "replayed signed list into cert_revocations" line

# 4. Every expected agent has checked in.
journalctl -u nixfleet-control-plane.service --since='5 min ago' \
  | grep 'checkin received' | awk '{print $NF}' | sort -u
# expected: every hostname declared in fleet.nix that's currently online
```

If all four checks pass, recovery is complete.

## What you've lost

Recoverable but **regressed by one reconcile cycle**:

- **In-flight rollouts.** Hosts that were mid-activation (in `pending_confirms.state = 'pending'`) get the gap A orphan-recovery treatment on their next confirm POST: the CP synthesises a confirmed row when the agent's reported closure matches the verified target. Agents whose closure does NOT match get the standard 410 + local rollback.
- **Soak windows.** Per-host `last_healthy_since` markers reset to "now of next checkin" via gap B's attestation. A wave that was mid-soak when the CP died restarts its soak window; if the wave's `soak_minutes` is e.g. 30 minutes and the host was 28 minutes in, you pay an extra 30 minutes before promotion.
- **Audit trail.** `audit.log` survives if you didn't delete it. The CP's structured journal output also survives (systemd journal is independent of the SQLite state). Together they reconstruct the pre-wipe state for incident review.

**Not lost** (recovered automatically from signed artifacts):

- Cert revocations (gap C — signed sidecar).
- Trust roots (flake-provided).

**Not lost** (recovered automatically from agent inputs):

- Host checkins.
- Agent identities (mTLS certs are agent-side; the CP only verifies them).

## Recovery time targets

These are validated by `fleet-harness-teardown` in CI (with the harness's 10s poll cadence):

| Fleet size | Target | Validated |
|---|---|---|
| 10 hosts | < 5 min p95 | yes (`fleet-harness-fleet-10` once #5's poll-cadence-aware variant lands) |
| 50 hosts | < 15 min p95 | not yet automated; estimate based on linear scaling |

Production poll cadence (60s) means the harness numbers don't translate 1:1. Expect ~2 minutes for a 10-host fleet, ~5-10 minutes for 50 hosts. If you observe substantially longer recovery, suspect upstream-fetch issues (Forgejo down, signature verify failing) rather than CP-internal latency.

## When this runbook fails

- **CP refuses to start.** Check `journalctl -u nixfleet-control-plane.service` for the verify-fleet error. Usually:
  - Trust file unreadable → check `--trust-file` permissions.
  - Build-time artifact signature invalid → flake build is producing a corrupted `fleet.resolved.json`. Roll back the flake commit.
  - Database migration failure → unexpected schema state in a non-empty DB; if you wiped per Step 2 this should not happen. If it does, file a bug.
- **Agents don't reconnect.** Check the agents' systemd unit on the agent host: `journalctl -u nixfleet-agent.service`. Usually a cert-expiry issue (the cert's notBefore predates a `cert_revocations` entry — the gap C sidecar replayed on restart and now invalidates the agent). Resolution: re-enroll the affected hosts via the bootstrap-token flow.
- **Recovery time exceeds the target by >10×.** Likely an upstream-fetch issue. Check the channel-refs poll loop: `journalctl -u nixfleet-control-plane.service | grep channel-refs`. Common causes: Forgejo authentication token expired, network partition, upstream URL misconfigured.

## Validation in CI

The harness scenario `fleet-harness-teardown` automates this procedure end-to-end:

```bash
nix build .#checks.x86_64-linux.fleet-harness-teardown
```

Asserts:
1. Both agents check in successfully against the freshly-booted CP (steady-state).
2. CP service stops, DB is wiped, service restarts.
3. Both agents check in successfully against the post-wipe CP within 30s (the harness's recovery window — production target is 5 min for 10 hosts).
4. The CP's journal shows the verified-fleet snapshot reprime line, proving the on-disk artifact path was consulted.

The scenario is the falsifiable validation of done-criterion #1: if it fails, the design's load-bearing claim is broken.
