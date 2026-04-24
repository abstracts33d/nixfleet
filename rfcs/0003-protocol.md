# RFC-0003: Agent ↔ control-plane protocol

**Status.** Draft.
**Depends on.** RFC-0001, RFC-0002, nixfleet #2 (magic rollback).
**Scope.** Wire protocol between agent and control plane. Identity, endpoints, polling, versioning, security properties. Does not cover control-plane-internal APIs.

## 1. Design goals

1. **Pull-only for control flow.** Agents initiate every connection. Control plane never needs to reach an agent — works behind CGNAT, hotel WiFi, intermittent links.
2. **Stateless on the wire.** Each request is self-describing. No sessions, no long-lived connections, no WebSockets in v1.
3. **Declarative intent, not commands.** The control plane answers "what should host X be running?", never "run this command". Scripted execution is outside the agent's vocabulary on purpose.
4. **Zero-knowledge for secrets.** Secrets do not transit the control plane in plaintext (see nixfleet #6). The protocol carries closure hashes and references, not secret material.
5. **Explicitly versioned.** Every request and response carries a protocol version. Mismatches fail loudly.

## 2. Identity model

- **Agent identity = mTLS client certificate.** CN carries `hostname`, SANs carry declared host attributes (channel, tags — redundant with fleet.resolved, used only for sanity checking).
- **Cert issuance.** On enrollment (nixfleet #9), agent generates ed25519 keypair, sends CSR with a one-shot bootstrap token. Control plane issues cert with 30-day validity.
- **Cert rotation.** Agent requests renewal at 50% of remaining validity. Old cert valid until expiry; overlap prevents downtime.
- **Cert revocation.** Control plane maintains a small revocation set (hostname → notBefore timestamp). Agents with certs issued before `notBefore` for their hostname are rejected. Simpler than CRLs; works because cert lifetime is short.
- **No shared credentials.** No API keys, no HMAC secrets, no bearer tokens. mTLS end to end.

## 3. Wire format

- **Transport.** HTTP/2 over TLS 1.3. mTLS mandatory.
- **Body.** JSON. Canonical field names, no nulls (absence means absence), timestamps RFC 3339 UTC.
- **Headers.**
  - `X-Nixfleet-Protocol: 1` — major version. Mismatched = 400.
  - `X-Nixfleet-Agent-Version: <semver>` — informational.
  - `Content-Type: application/json`.
- **Why not gRPC/protobuf?** Stability, debuggability, homelab introspection. Revisit if wire size becomes a problem (it won't at fleet sizes nixfleet targets).

## 4. Endpoints

All endpoints rooted at `https://<control-plane>/v1/`.

### 4.1 `POST /agent/checkin`

The core of the protocol. Agent polls this on its declared interval.

**Request body:**

```json
{
  "hostname": "m70q-attic",
  "agentVersion": "0.2.1",
  "currentGeneration": {
    "closureHash": "sha256-aabbcc...",
    "channelRef": "abc123def",
    "bootId": "f0e1d2c3-..."
  },
  "health": {
    "systemdFailedUnits": [],
    "uptime": 1234567,
    "loadAverage": [0.1, 0.2, 0.3]
  },
  "lastProbeResults": [
    { "control": "anssi-bp028-ssh-no-password", "status": "passed",
      "evidence": "...", "ts": "2026-04-24T10:15:02Z" }
  ]
}
```

**Response body:**

```json
{
  "target": {
    "closureHash": "sha256-ddeeff...",
    "channelRef": "def456abc",
    "rollout": "stable@def456",
    "wave": 2,
    "activate": {
      "confirmWindowSecs": 120,
      "confirmEndpoint": "/v1/agent/confirm",
      "runtimeProbes": [
        { "control": "anssi-bp028-ssh-no-password", "schema": "anssi-bp028/v1" }
      ]
    }
  },
  "nextCheckinSecs": 60
}
```

If the host is already at the desired generation, `target` is absent and `nextCheckinSecs` reflects idle polling.

**Idempotency.** Repeated check-ins from the same host with unchanged state are no-ops (but still update `lastSeen` for observability). The control plane must not create duplicate work.

### 4.2 `POST /agent/confirm`

Called exactly once by the agent, after a new generation has booted and the agent process has come up healthy. The magic-rollback window (nixfleet #2) closes on receipt.

**Request body:**

```json
{
  "hostname": "m70q-attic",
  "rollout": "stable@def456",
  "wave": 2,
  "generation": {
    "closureHash": "sha256-ddeeff...",
    "bootId": "new-boot-uuid-..."
  },
  "probeResults": [
    { "control": "anssi-bp028-ssh-no-password", "status": "passed", "evidence": "..." }
  ]
}
```

**Response:** `204 No Content` on acceptance, `410 Gone` if the rollout was cancelled or the wave already failed (agent then triggers local rollback on its own).

### 4.3 `POST /agent/report`

Out-of-band state reports: activation failure, probe failure, voluntary rollback. Distinct from `/checkin` so that failure reports don't interleave with normal polling cadence.

```json
{
  "hostname": "m70q-attic",
  "event": "activation-failed",
  "rollout": "stable@def456",
  "details": {
    "phase": "switch-to-configuration",
    "exitCode": 1,
    "stderrTail": "..."
  }
}
```

### 4.4 `GET /agent/closure/<hash>`

Optional. If the host cannot reach the binary cache directly (restricted network), the control plane can proxy closures. Preference remains: agents fetch from cache, not control plane — this endpoint exists as a fallback, not a default path.

### 4.5 Enrollment endpoints (nixfleet #9)

Out of scope for this RFC in detail. Summary:

- `POST /enroll` — accepts bootstrap token + CSR, returns signed cert. Token is burned on use.
- `POST /agent/renew` — accepts current cert (mTLS) + CSR, returns refreshed cert.

## 5. Polling cadence

- **Default interval.** 60s, controlled server-side via `nextCheckinSecs` in the checkin response.
- **Backoff on error.** Exponential with jitter, capped at the channel's `reconcileIntervalMinutes`. Network errors do not drain the confirm window — `/confirm` retries aggressively (up to 5×) within the window to survive transient failures.
- **Load shaping.** Control plane can vary `nextCheckinSecs` per-host to smooth thundering herds after a push (e.g. assigning each host a slot within the polling window based on a hash of its hostname).
- **Idle hosts.** A host with no pending target polls at the channel's idle cadence (can be much longer — weekly for `edge-slow`).

## 6. Versioning

- **Protocol major version** in header. v1 → v2 is a breaking change; running mixed versions is disallowed and fails at check-in with a clear message. Upgrade path: control plane supports N and N+1 simultaneously; operators upgrade agents, then retire control plane's N support.
- **Schema evolution within a major.** Fields may be added; agents and control plane MUST ignore unknown fields. Required fields never change meaning. Removing a field requires a major bump.
- **Agent version (informational).** Control plane refuses agents older than its declared minimum, emits events for newer agents (may indicate staged upgrade in progress).

## 7. Security model

**Defended against:**

- **Passive network observer.** TLS 1.3 — sees only traffic shape.
- **Active on-path attacker without a cert.** mTLS fails the handshake; no data exposed.
- **Compromised non-target agent.** Cert only authorizes its own hostname; cannot request targets for other hosts, cannot submit reports for other hosts. Control plane enforces `cert.CN == request.hostname` on every endpoint.
- **Compromised control plane.** Cannot learn secrets (zero-knowledge, nixfleet #6). Can serve wrong closure hash → but the hash is self-verifying against Nix store signatures, so a host fetching from an honest cache will fail verification.
- **Replay.** Confirm requests include `bootId`; the control plane rejects a confirm whose `bootId` doesn't match the expected new boot.

**Not defended against (explicit):**

- **Compromised host (root).** If the host's TLS key is stolen, the attacker can act as that host until the cert is revoked. Mitigated by short cert lifetime + TPM-backed keys (future issue).
- **Denial of service.** Out of scope for this RFC. Rate limiting, fail2ban-style protections, and similar are operational concerns.
- **Malicious control-plane operator.** Is explicitly a trusted role (can push any generation to any host). The security boundary is between the fleet and outsiders, not between operators and hosts.

## 8. Offline behavior

- **Agent caches the last check-in response** on disk. If the control plane is unreachable, the agent continues to operate at its current generation. It does not auto-revert, does not auto-upgrade.
- **Prolonged offline window.** If check-in fails for longer than `channel.offlineGraceSecs` (default: 7 days), the agent emits a local systemd journal warning but takes no action. Action is an operator decision.
- **Clock skew tolerance.** All deadlines (confirm window, cert validity) carry ≥ 60s slack to absorb typical host↔CP clock drift.

## 9. Open questions

1. **Per-host pinning for debugging.** Should operators be able to pin a host to a specific generation outside normal rollouts ("don't touch this, I'm debugging")? Leaning yes, via a `freeze` flag in fleet.nix or a control-plane-side override — but this is declarative-intent-breaking, so needs careful design.
2. **Closure signing.** Should the control plane sign its `target` responses to make them non-repudiable (attacker can't swap targets even by compromising TLS)? Probably overkill given TLS + store hash self-verification. Reject v1, revisit if threat model changes.
3. **Streaming vs polling.** SSE or long-polling for the checkin endpoint would reduce latency for event-driven rollouts (no need to wait for next poll). Deferred to v2; pure polling is simpler to reason about and adequate for nixfleet's target fleet sizes.
4. **Multi-control-plane.** Agents talking to a quorum of CPs for HA. Out of scope for v1; single control plane with standard HA (pacemaker, k8s statefulset) is the expected deployment.

---

## Appendix: Relationship between the three RFCs

```
  RFC-0001 (fleet.nix)          "what do I want?"
       │
       │  produces fleet.resolved
       ▼
  RFC-0002 (reconciler)          "what should happen next?"
       │
       │  emits per-host intents
       ▼
  RFC-0003 (agent protocol)      "how do intents reach hosts and
                                  how does observed state come back?"
       │
       │  updates observed state
       ▼
  RFC-0002 (reconciler, next tick)
```

The loop is:

1. RFC-0001 defines desired state.
2. RFC-0002 compares desired to observed and emits intent.
3. RFC-0003 ships intent to agents and returns observations.
4. Loop forever. Every tick is idempotent. Every decision has a written reason.

That's the whole system. Everything else in nixfleet — CLI, compliance, scopes, darwin support — is an accessory to this loop.
