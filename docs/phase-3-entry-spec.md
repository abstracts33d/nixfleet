# Phase 3 entry spec

Sequences RFC-0003 (agent ↔ control-plane wire) into five reviewable PRs. End deliverable: each NixOS fleet host runs a real `nixfleet-agent` that polls the CP over mTLS and reports its current generation. The CP records check-ins, derives `Observed` state from them (replacing Phase 2's hand-written `observed.json`), and the reconciler plan reflects actual fleet state. **No activation runs** — that's Phase 4.

Cross-references: `docs/KICKOFF.md` §1 Phase 3, `rfcs/0003-protocol.md` (the wire spec), `rfcs/0002-reconciler.md` §4 (the reconciler this feeds), `docs/trust-root-flow.md` §3 (the trust-file pipeline).

Status: **proposed** — adopt as the implementation plan for the next ~5 PRs.

## 1. Goal

Phase 2 made the reconciler real on lab as a oneshot timer reading a hand-written `observed.json`. Phase 3 turns the same binary into a long-running TLS server with one new internal loop and a richer HTTP surface, and adds a real `nixfleet-agent` body that talks to it. By the end of Phase 3:

- Lab's CP listens on a TLS port, accepting mTLS-authenticated agent connections.
- Each NixOS fleet host runs `nixfleet-agent` as a systemd service. It POSTs `/v1/agent/checkin` every 60s with its current closure hash, bootId, pending generation, and last fetch outcome.
- The CP's reconcile loop ticks every 30s on an in-memory `Observed` derived from check-ins (the file-backed `--observed` becomes a dev/test fallback).
- The CP polls Forgejo every 60s for `releases/fleet.resolved.json`, refreshing channel-refs without operator intervention.
- Agents fetch new client certs via `/v1/agent/renew` automatically at 50% of validity — no operator-driven re-enrolment for normal cert rotation.
- Adding a new fleet host = declare in `fleet.nix` + agenix-encrypted bootstrap token; first boot self-enrols and immediately begins checking in.
- Operator can correlate fetch/verify failures fleet-wide via `/v1/agent/report` (CP records to in-memory state; surfaced in journal).

**Not in Phase 3** (deferred to later phases, even though some live in RFC-0003):

- Activation (`nixos-rebuild switch` from agent) — Phase 4.
- Magic rollback semantics — Phase 4 (only meaningful once activation can fail). `/v1/agent/confirm` is wire-shape-locked as a stub in Phase 3 so no client refactor when Phase 4 lights it up.
- Probe execution + signed evidence — Phase 7.
- Compliance gates as rollout blockers — Phase 6/7.
- Closure proxy (`/v1/agent/closure/<hash>`) — Phase 4 (only relevant when agents fetch closures).
- Darwin (`aether`) agent support — non-goal; stays manually managed until Phase 5+.

**Trust-hierarchy hardening is deferred** (see issue #41 — TPM-bound issuance CA, offline fleet root, name constraints). Phase 3 + Phase 4 ship under an online Fleet CA on the CP. The wire protocol is independent of the CA model, so migrating later is a `trust.json` rotation plus `/renew` cycle — not a re-architecture. See §9.

## 2. The architectural shift

Phase 2 was:

```
systemd timer (5min) ──▶ nixfleet-control-plane (oneshot)
                            ├── verify_artifact(fleet.resolved)
                            ├── reconcile(observed.json)
                            └── emit JSON-line plan to journal, exit
```

Phase 3 becomes:

```
systemd service (long-running) ──▶ nixfleet-control-plane (server)
                                     ├── mTLS listener (PR-1, PR-2)
                                     ├── GET  /healthz                  (PR-1)
                                     ├── GET  /v1/whoami                (PR-2)
                                     ├── POST /v1/agent/checkin         (PR-3)
                                     ├── POST /v1/enroll                (PR-5)
                                     ├── tokio::time::interval(30s) ──▶ reconcile(in-memory Observed)
                                     └── emit JSON-line plan to journal each tick

                          systemd service (per host) ──▶ nixfleet-agent
                                                          ├── tokio + reqwest mTLS
                                                          ├── poll /v1/agent/checkin every 60s
                                                          └── log target on stdout
```

The shift is real: the reconciler stops being a side-effect-free CLI and becomes a co-located function inside the server's tick loop. PR #36 deliberately deferred this — PR-1 below is where it lands.

## 3. PR breakdown

### PR-1 — CP becomes a long-running TLS server with `/healthz`

**Scope.** Restructure `nixfleet-control-plane` from oneshot to long-running. One real endpoint (`GET /healthz`) for proof-of-life. TLS-only listener (server cert + key from operator-supplied paths). mTLS not required yet — PR-2 adds it.

**Concrete.**

- Re-add to `crates/nixfleet-control-plane/Cargo.toml`: `tokio` (full), `axum 0.8`, `axum-server 0.7` (tls-rustls), `rustls 0.23`. (Removed in PR #36 as Phase 2 didn't need them.)
- Subcommand split: `nixfleet-control-plane serve` (long-running) and `nixfleet-control-plane tick` (oneshot, kept for tests + ad-hoc operator runs). Default subcommand is `serve`.
- New `src/server.rs` with axum router + axum-server TLS listener.
- Internal reconcile loop: `tokio::time::interval(Duration::from_secs(30))` calls the existing `tick()` function and emits the plan via tracing.
- NixOS module switches from oneshot+timer to a `simple` always-running service. Re-add `--listen` and `--tls-cert/--tls-key` options. Keep `--observed` as fallback for the file-backed input until PR-4.
- `/healthz` returns `{"ok": true, "version": "<crate version>", "lastTickAt": "<rfc3339>"}`.
- Tests: rcgen-generated server cert, hit `/healthz` with reqwest, assert 200 + valid JSON.

**Deliverable.** From the operator's workstation:

```
curl --cacert /etc/nixfleet/fleet-ca.pem https://lab:8080/healthz
# {"ok":true,"version":"0.2.0","lastTickAt":"2026-04-25T12:34:56Z"}
```

`journalctl -u nixfleet-control-plane.service` on lab shows reconcile-tick JSON lines every 30s.

**Open decisions.** §8 D1, D2, D3.

### PR-2 — mTLS + `/v1/whoami`

**Scope.** Server requires a verified client cert; verifies against an operator-supplied CA. Adds `GET /v1/whoami` returning the verified CN of the client — useful for confirming the cert pipeline before the agent body is real.

**Concrete.**

- axum-server TLS config: `ClientCertVerifier` against the configured CA.
- `/healthz` remains unauthenticated (operational debuggability — see §8 D7); `/v1/*` requires verified mTLS.
- Extract client CN via `x509-parser` on the verified cert chain.
- `/v1/whoami` returns `{"cn": "<client-CN>", "issuedAt": "<rfc3339>"}`.
- NixOS module re-adds `--client-ca` flag (the v0.2 skeleton had it).
- Tests: rcgen generates server cert, valid client cert, invalid client cert. Verify whoami returns CN for valid; rejected (TLS handshake failure) for invalid.

**Deliverable.** From any fleet host:

```
curl --cert /run/agenix/agent-krach-cert \
     --key  /run/agenix/agent-krach-key \
     --cacert /etc/nixfleet/fleet-ca.pem \
     https://lab:8080/v1/whoami
# {"cn":"krach","issuedAt":"..."}
```

### PR-3 — Agent body: first `/v1/agent/checkin` + `/report` + `/confirm` stub

**Scope.** Replace the `tracing::info!` skeleton in `nixfleet-agent` with a real poll loop. Send `/v1/agent/checkin` every `pollInterval` seconds with a richer body than RFC-0003 §4.1's minimum (pending generation, last-fetch outcome, agent self-version). CP records check-ins into in-memory state and responds with `target: null` (no rollouts dispatched in Phase 3 — that's Phase 4). Add `/v1/agent/report` (real, in-memory) for fetch/verify failure events and `/v1/agent/confirm` as an accept-and-discard stub to lock the wire shape ahead of Phase 4.

**Concrete.**

- `nixfleet-agent`: real main loop. Reads cert paths from CLI args (already present in module). Builds a `reqwest::Client` with mTLS. Polls `/v1/agent/checkin` every 60s.
- Checkin request body — extends RFC-0003 §4.1 with operator-observability fields:
  ```json
  {
    "hostname": "krach",
    "agentVersion": "0.2.0",
    "currentGeneration": {
      "closureHash": "<hash from /run/current-system>",
      "channelRef": null,
      "bootId": "<from /proc/sys/kernel/random/boot_id>"
    },
    "pendingGeneration": {
      "closureHash": "<hash if /run/booted-system != /run/current-system>",
      "scheduledFor": null
    },
    "lastEvaluatedTarget": {
      "closureHash": "<last target the agent saw from CP>",
      "channelRef": "<channel-ref of that target>",
      "evaluatedAt": "<rfc3339>"
    },
    "lastFetchOutcome": {
      "result": "ok" | "verify-failed" | "fetch-failed" | "none",
      "error": "<short string>"  // null when result == ok or none
    },
    "uptime": "<seconds since agent process start>"
  }
  ```
  All new fields are nullable. `pendingGeneration`, `lastEvaluatedTarget`, and `lastFetchOutcome` may be null on first check-in or when no relevant event has occurred.
- CP-side `/v1/agent/checkin` handler. Validates the verified mTLS CN matches the body's `hostname` (sanity check, not a security boundary — mTLS already authenticated). Records into `Arc<RwLock<HashMap<String, HostState>>>`. Returns `{"target": null, "nextCheckinSecs": 60}`.
- CP-side `POST /v1/agent/report` handler:
  - Body shape per RFC-0003 §4.5 (event reports). Records into the same in-memory state with bounded ring buffer per host (default 32 entries).
  - Surfaced in journal as `report received hostname=<cn> kind=<kind> error=<short>`.
  - No persistence to disk — survives only as long as the CP process. Phase 4 adds SQLite persistence.
- CP-side `POST /v1/agent/confirm` stub — see §8 D10. Accept-and-discard: parse + validate the body, log it, return `200 OK` with `{"acknowledged": true}`. Body interpretation is intentionally a no-op until Phase 4 wires up activation deadlines.
- Tests:
  - Cargo integration test: spin up CP in-process (axum), agent in-process, run one check-in with each non-null field combination, assert state captured.
  - `/report` integration test: agent sends a synthetic verify-failure event, CP records, journal shows it.
  - `/confirm` smoke test: agent posts, CP returns 200, no state mutation observable.
  - Optional: extend the PR #34 harness scenario to make two agent microVMs check in to a host CP — `journalctl -u nixfleet-control-plane | grep checkin` shows both hostnames within 60s. (May land as PR-3.5 if it grows.)

**Deliverable.** `journalctl -u nixfleet-control-plane.service` on lab shows entries like `checkin received hostname=krach closureHash=861d2y2zmssij… pending=null lastFetch=ok`. Each fleet host's `journalctl -u nixfleet-agent` shows successful periodic checkins. A synthetic `nixfleet-cli send-report` (added under §3.5) round-trips a fake fetch-failed event into CP's journal.

**Open decisions.** §8 D10.

### PR-4 — Live `Observed` projection from check-ins + Forgejo channel-ref poll

**Scope.** CP derives `Observed` (the existing `nixfleet_reconciler::Observed` input type) from in-memory check-in state. Reconcile loop reads from this projection instead of `observed.json`. The hand-written file becomes opt-in via `--observed` flag for tests/dev only. CP also polls Forgejo every 60s for `releases/fleet.resolved.json`, refreshing channel-refs without operator intervention (replaces the hand-edited `/etc/nixfleet/cp/channel-refs.json` default).

**Concrete.**

- New module `src/observed_projection.rs`: takes the in-memory `HashMap<String, HostState>` plus a configured `channel_refs` source and produces an `Observed`.
- New module `src/forgejo_poll.rs`:
  - `tokio::time::interval(60s)` task fetching `https://git.lab.internal/api/v1/repos/<owner>/fleet/contents/releases/fleet.resolved.json` (URL configured via `--forgejo-base-url` + `--fleet-repo`).
  - Auth via `Authorization: token $(cat $FORGEJO_TOKEN_FILE)`. Token mounted from agenix at `/run/agenix/cp-forgejo-token`.
  - Decodes the API response (base64-encoded `content` field), runs the existing `verify_artifact` against it, updates the in-memory channel-refs cache.
  - Failure semantics: log warning, keep last-known channel-refs, do not crash. See §8 D9.
- Server's reconcile loop calls `project()` each tick using the latest cached channel-refs, then `reconcile()`.
- The `--observed` flag stays — useful for offline-replay debugging (operator dumps in-memory state to a file, reproduces a tick). The `--channel-refs-file` flag also stays as an offline alternative for tests + dev.
- Plan JSON-line format unchanged from Phase 2.
- Tests:
  - Simulated check-ins → projection → reconcile → assert plan reflects reported state.
  - Forgejo-poll integration test: stub Forgejo HTTP, return a signed `fleet.resolved.json`, assert poll loop refreshes cache, reconcile picks it up on next tick.
  - Forgejo-down test: stub returns 503, assert poll logs warning + retains previous cache value.

**Deliverable.** Operator commits a no-op release commit + push to lab Forgejo. CI signs. Workstations auto-upgrade (per fleet PR #47) to that commit. Each host checks in with its new closure hash. Lab's CP polls Forgejo within 60s, picks up the new `fleet.resolved.json`, and the reconcile loop sees the converged state and emits zero actions. *Diverged* state would emit `OpenRollout` (the Phase 4 dispatch loop is what would then act on it). No operator-driven `channel-refs.json` edit needed.

**Open decisions.** §8 D9.

### PR-5 — Bootstrap enrollment + cert renewal

**Scope.** `POST /v1/enroll` accepts a CSR + bootstrap token; verifies the token against the org root key; issues a 30-day client cert signed by the fleet CA. `POST /v1/agent/renew` accepts a CSR over an existing valid mTLS connection; issues a fresh 30-day cert. Agent has a one-shot enrollment mode for first boot when no cert exists, and a self-paced renewal at 50% of cert validity. `/enroll` and `/renew` share the CSR-validation + cert-issuance code path.

**Concrete.**

- Org root key bootstrap, parallel to the `ciReleaseKey` TPM bootstrap from fleet PR #45:
  - Generate ed25519 keypair offline (operator workstation per §8 D5; later PRs may move to Yubikey).
  - Declare pubkey under `nixfleet.trust.orgRootKey.current` in `fleet/modules/nixfleet/trust.nix`.
  - Private key kept on operator workstation (or a Yubikey when §8 D5 is upgraded).
- New tiny binary `nixfleet-mint-token` in `crates/nixfleet-cli` (or a new crate): operator runs `nixfleet-mint-token --hostname krach --csr-pubkey-fingerprint <sha256>` once per host before first deploy; emits a one-shot token signed with the org root private key.
- CP-side issuance module `src/issuance.rs` — shared between `/enroll` and `/renew`:
  - `issue_cert(csr, validity, audit_context) -> Result<Certificate>`.
  - Validates CSR (subject CN format, pubkey algorithm, no extension extras).
  - Builds TBS cert with `clientAuth` EKU + SAN dNSName + standard X.509 hygiene.
  - Signs with the fleet CA private key (path from `--fleet-ca-key`). **See §9: this is the online-CA tech-debt; replaced by TPM-bound issuance per issue #41.**
  - Audit-logs every issuance: requesting CN, issued subject, validity window, source IP, request type (enroll | renew), to journal AND to `/var/lib/nixfleet/cp/issuance.log`.
- CP-side `/v1/enroll` handler:
  - Verify token signature against `orgRootKey.current` from trust.json.
  - Verify token's `expectedHostname` matches the CSR's CN.
  - Verify token's `expectedPubkeyFingerprint` matches the CSR's public key.
  - Verify token hasn't been used (in-memory replay set; persistence is Phase 4).
  - Call `issuance::issue_cert(csr, 30d, AuditContext::Enroll { token_id })`.
- CP-side `/v1/agent/renew` handler:
  - Auth: existing mTLS (verified client cert chain, not yet expired).
  - Validate: CSR's CN matches verified mTLS CN; CSR's pubkey ≠ existing cert's pubkey (key rotation enforced); SAN list matches existing cert's SAN list.
  - Soft floor: warn-but-accept if requesting agent's existing cert has >50% remaining validity (agent should self-pace at 50%; CP doesn't enforce strictly to avoid blocking emergency rotations).
  - Call `issuance::issue_cert(csr, 30d, AuditContext::Renew { previous_cert_serial })`.
- Agent-side first-boot mode:
  - On startup, if `--cert/--key` files don't exist (or cert is expired), enter enrollment.
  - Read `--bootstrap-token` path. Generate a CSR (`rcgen`). POST `/v1/enroll`. Write returned cert to disk.
  - Resume normal checkin loop.
- Agent-side renewal loop:
  - On every check-in tick, evaluate: cert remaining validity < 50% of total validity?
  - If yes, generate fresh keypair + CSR (rcgen). POST `/v1/agent/renew` over current valid mTLS. Write new cert + key atomically (`O_TMPFILE` + rename).
  - Restart only mTLS client (not the whole process); next check-in uses the new cert.
- Module updates:
  - Agent module gains `bootstrapTokenFile` option.
  - Fleet-secrets gains `bootstrap-token-${hostname}` agenix entries (operator generates + commits per host).
  - CP module gains `--fleet-ca-key` flag; key path from agenix.
- Tests:
  - End-to-end enroll → checkin happy path (cargo integration test).
  - Renew round-trip: agent enrols, fakes time forward to 50% validity, renews, checks-in with new cert. Both happen over the same axum CP instance.
  - Renew rejects: same pubkey (key not rotated), wrong CN, expired requesting cert.
  - Token replay rejected (enroll).
  - Tampered token rejected (enroll).
  - Audit log: assert each issuance writes a JSON line with the expected fields.

**Deliverable.** Adding a new fleet host:
1. Declare in `fleet.nix`.
2. Operator runs `nixfleet-mint-token --hostname <new-host> ...`, agenix-encrypts the result.
3. First boot: agent enrols, immediately begins checking in.
4. Day 15 (50% of 30d validity): agent silently rotates its cert; operator sees the renew event in the CP audit log and journal. No operator action required.

No manual SSH-to-lab-and-copy-cert step. No periodic operator-driven re-enrolment.

**Open decisions.** §8 D5, D6, D11.

## 4. Test substrate

The PR #34 signed-roundtrip harness scenario is the substrate. Phase 3 PRs extend it:

- **PR-1**: cargo integration test only (binary smoke); harness untouched.
- **PR-2**: cargo integration test for mTLS handshake (rcgen-based cert generation in-test).
- **PR-3**: extend the harness scenario to make agent microVMs check in to the host CP. Replaces the curl+verify-artifact wrapper with the real agent binary. Adds `/report` event-roundtrip assertion. `/confirm` covered by cargo-only smoke. (May land as PR-3.5 if it grows.)
- **PR-4**: extend the harness assertion to grep for `checkin received` in CP journal across multiple agents. New harness scenario `fleet-harness-forgejo-channel-roll`: stub Forgejo, agent + CP run, push a signed `fleet.resolved.json` update, assert CP picks it up within one poll cycle.
- **PR-5**: new harness scenario `fleet-harness-enroll-checkin`: agent boots without a cert, has a bootstrap token, enrols, then checks in. Renewal scenario `fleet-harness-renew`: agent boots with a cert that's >50% expired, renews, continues checking in with the new cert.

## 5. Cargo dep changes per PR

| PR | Adds to `nixfleet-control-plane` | Adds to `nixfleet-agent` |
|---|---|---|
| PR-1 | tokio (full), axum, axum-server (tls-rustls), rustls — re-add | — |
| PR-2 | x509-parser, rustls-pki-types | — |
| PR-3 | (no new server-side beyond what `/report` + `/confirm` need — same axum/serde stack) | tokio, reqwest (rustls-tls-native-roots), serde_json |
| PR-4 | reqwest (rustls-tls-native-roots), base64 — for Forgejo poll | — |
| PR-5 | rcgen, sha2, hex (token signing + cert issuance primitives) | rcgen (CSR generation, both enroll + renew) |

## 6. Hard prerequisites before PR-1

These need to be true on lab before PR-1 can ship:

1. **CP server cert + key in agenix.** Dropped from `fleet/modules/nixfleet/tls.nix` in PR #46 to unblock the Phase 2 deploy; need to come back. Specifically: declare `cp-cert` and `cp-key` secrets in `fleet-secrets`, encrypt to lab's pubkey, re-add the wiring.
2. **Fleet CA exists at `_config/fleet-ca.pem`.** The agent TLS block already references it (see `fleet/modules/nixfleet/tls.nix`); verify the file is committed and the corresponding private key is offline somewhere (used to sign agent + CP server certs).
3. **Per-host agent certs in agenix.** `agent-${hostName}-{cert,key}` already declared per `fleet/modules/secrets/nixos.nix`; verify they're populated for `krach`, `ohm`, `lab`, `pixel` (aether deferred).

If these don't exist, PR-1 is blocked on a fleet-side prep PR that creates them. Estimate: ~1h (key generation + agenix encryption + wiring re-add).

## 7. Order

Strictly sequential: PR-1 → PR-2 → PR-3 → PR-4 → PR-5. Each PR is shippable on its own and unblocks the next.

Rough size estimates (after the scope expansion in §1):

| PR | Rust LOC | NixOS LOC | Effort (focused) |
|---|---|---|---|
| Prep | — | ~50 | ~1h |
| PR-1 | ~400 | ~50 | half-day |
| PR-2 | ~150 | ~20 | few hours |
| PR-3 | ~700 | ~50 | ~1.5 days (added: richer check-in + `/report` + `/confirm` stub) |
| PR-4 | ~350 | ~30 | ~1 day (added: Forgejo poll) |
| PR-5 | ~900 | ~100 | ~2 days (added: `/renew` shares issuance code with `/enroll`) |

Total Phase 3: ~5-6 days focused, ~2-3 weeks part-time.

Phase 4 (activation + magic rollback + closure proxy) layers on top — that's where the agent gains `nixos-rebuild switch`, the CP gains dispatch + soak + rollback semantics, `/v1/agent/confirm` gains real semantics (activation deadline), `/v1/agent/closure/<hash>` proxies attic, and `system.autoUpgrade` on workstations (fleet PR #47) gets disabled per-host as the agent supersedes it. Phase 4 is now leaner because `/renew`, `/report`, `/confirm` (wire), and Forgejo polling already shipped in Phase 3.

## 8. Decisions to lock in before PR-1

Confirm before implementation starts. **Defaults stand if you don't override.**

### D1 — CP server cert source (Phase 3 prep)

**Default.** Re-add `cp-cert/cp-key` to `fleet-secrets` as agenix-encrypted secrets, mirroring how `agent-${hostName}-cert/key` already work. Same fleet CA signs both. Operator generates the keypair offline once, encrypts to lab's pubkey, commits.

**Alternative.** Self-signed cert generated at first boot. Simpler bootstrap, harder rotation, fights the architecture's "everything is signed by something offline" principle.

### D2 — Reconcile cadence

**Default.** 30s. Fast enough that operator-visible drift (host failed to check in) shows up in the journal within one cycle; slow enough not to spam the journal.

**Alternative.** 60s (matches RFC-0003 default polling); 10s (tighter operator feedback at the cost of journal noise).

### D3 — Server port

**Default.** 8080 (HTTPS). Matches the v0.2 skeleton; `ports < 1024` would require CAP_NET_BIND_SERVICE; 443 collides with operator-facing services on lab.

**Alternative.** A non-standard port (8443? 9443?) for less collision-prone discoverability. No strong reason.

### D4 — Channel-ref source for the in-memory projection (PR-4)

**Default (revised).** CP polls Forgejo's `/api/v1/repos/<owner>/fleet/contents/releases/fleet.resolved.json` every 60s. Auth via agenix-mounted token. Failure semantics: log warning, retain previous cache, do not crash. See §8 D9 for poll-loop details.

**Alternative.** Hand-edited `/etc/nixfleet/cp/channel-refs.json`, declared by the CP NixOS module. Retained as `--channel-refs-file` flag for tests/dev only — not the operator-facing default. (Was the original default in this spec; revised because the operator-toil cost of hand-editing every release outweighs the +1h implementation cost of polling.)

### D5 — Org root private key (PR-5)

**Default.** File on operator workstation, consumed by `nixfleet-mint-token`. Simplest bootstrap; rotation is a documented procedure.

**Alternative.** Yubikey-resident from day one. Right end-state per the architecture doc; adds hardware setup steps before PR-5 can land. Fine to defer to Phase 9 polish.

### D6 — Cert validity (PR-5)

**Default (revised).** 30d, matching RFC-0003 §2 ("agent requests renewal at 50% of remaining validity"). `/v1/agent/renew` lands in Phase 3 (see PR-5 expansion in §3) — no operator-driven re-enrolment for normal cert rotation.

**Alternative.** Longer (1y) for Phase 3 only; switch to 30d when `/v1/agent/renew` lands. (Was the previous default; revised because `/renew` is now in Phase 3 scope, eliminating the toil this alt was meant to mitigate.)

### D7 — `/healthz` authentication

**Default.** Unauthenticated. Operational debuggability (curl from anywhere with network reachability + CA trust) outweighs the marginal sovereignty gain of mTLS-gating a status endpoint.

**Alternative.** mTLS-required like `/v1/*`. Strict default; reachable only from agent-equipped hosts.

### D8 — Phase 3 scope: `/v1/agent/{confirm,report}` (revised)

**Default (revised).** Both endpoints land in PR-3:

- **`/v1/agent/report`** — real, in-memory recording (bounded ring buffer per host). Surfaced in journal. Phase 4 adds SQLite persistence + correlation with rollouts.
- **`/v1/agent/confirm`** — accept-and-discard stub (200 OK, body parsed + logged + thrown away). Wire shape locked; semantics gain meaning in Phase 4 when activation deadlines exist. See D10.

**Alternative.** Defer both, or stub both with `410 Gone`. (Was the previous default; revised because `/report` has standalone value during Phase 3 — fetch/verify failures are observable now, not only post-activation. `/confirm` as stub is essentially free and avoids a Phase 4 client refactor.)

### D9 — Forgejo poll: auth, cadence, failure mode (PR-4)

**Default.** Pull-based, every 60s, agenix-mounted read-only token scoped to the `fleet` repo (`/run/agenix/cp-forgejo-token`). Endpoint: `/api/v1/repos/<owner>/fleet/contents/releases/fleet.resolved.json`. On failure (5xx, network blip, signature verify fail): log warning + retain previous cached `channel-refs`. CP does not crash on Forgejo unavailability.

**Alternative.** Push-based webhook from Forgejo to CP. Faster reaction (sub-second) but requires inbound network reachability + webhook auth. Defer to a future polish PR if poll latency becomes a problem.

### D10 — `/v1/agent/confirm` stub semantics (PR-3)

**Default.** Accept-and-discard. Endpoint returns `200 OK` with `{"acknowledged": true}`. Body is parsed against the RFC-0003 §4.4 shape (so the wire is real), validated for well-formedness, logged at `info` level, then discarded. No state mutation observable to the agent or other endpoints.

**Alternatives.**
- (a) `410 Gone` — explicit "not implemented yet". Cleaner semantics but means Phase 4 needs an agent-side branch (`if status == 410 then proceed else error`). Default avoids this.
- (b) Accept-and-record — body stored in same in-memory state as `/report`. Slight risk of baking semantics that Phase 4 needs to change.

### D11 — `/v1/agent/renew` authentication and key-rotation policy (PR-5)

**Default.** Auth: existing client cert (mTLS), still-valid (not expired). CSR validation:
- CN matches existing mTLS-verified CN
- New CSR pubkey ≠ existing cert pubkey (key rotation enforced — point of renewal)
- SAN list matches existing cert's SAN list
- Soft floor: warn-but-accept if requesting agent's existing cert has >50% remaining validity (don't block emergency rotations)

Issuance: signed by the same fleet CA used for `/enroll`. New cert validity: 30d. **The fleet CA is online on the CP — see §9 for the deferred TPM-bound replacement (issue #41).**

**Alternative.** Require the agent to also include a renewal token (signed by org root key). Adds replay defense beyond mTLS. Marginal benefit when mTLS already authenticates the requesting host; defer unless threat-modelling argues otherwise.

---

When you've confirmed (or pushed back on) the decisions above, PR-1 can start. The prep PR for the CP server cert (§6 #1) goes first.

## 9. Deferred: trust-hierarchy hardening

Phase 3 + Phase 4 ship under an **online Fleet CA on the CP** — `/enroll` and `/renew` both rely on the fleet CA private key being readable to the CP process. This violates issue #10's "control plane holds no secrets, forges no trust" property: a runtime root compromise on lab CP can mint arbitrary agent certs while the CP is up.

The wire protocol (RFC-0003) is **independent of the CA model**. Agents see the same cert chain, same mTLS handshake, same endpoint surface whether the cert was minted by an online CA or a hardware-bound one. So this is a tech-debt issue, not a re-architecture.

The proper fix is tracked in issue #41 — TPM-bound issuance CA, offline fleet root, X.509 name constraints. Substrate exists in `nixfleet-scopes/modules/scopes/tpm-keyslot/` (ECDSA P-256 key in TPM persistent handle, idempotent provisioning, sign wrapper). Gaps to close when the work lands: PCR sealing, `tss-esapi` Rust integration, X.509 cert template builder with name constraints, operator bootstrap script.

**Migration when issue #41 lands**:

1. Operator runs the new TPM-bound bootstrap on lab CP (one-shot).
2. Publishes `trust.json` with both old (online) and new (TPM-bound) issuance CAs trusted.
3. Agents rotate to the new CA on next `/renew` cycle (≤30d).
4. After grace period, drop old CA from `trust.json`.

~1 day operator time, ~1 week wall clock. Recommended placement: Phase 7-9 polish window.

**Risk during the tech-debt window** (Phase 3 launch → issue #41 close):

- CP holds fleet CA private key online; root-on-CP = trust forgery.
- Mitigated by Tailscale-only access, single operator, ~5-host fleet, lab not public-facing.
- Not exposed to attacker classes outside operator/insider.

This is a conscious deferral. Phase 3's value (working wire protocol, observable agents, declarative enrolment, automatic cert rotation) materialises immediately under the online-CA model. Hardware-binding the CA can land independently without the wire spec moving.
