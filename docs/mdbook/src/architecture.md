# Architecture

NixFleet is a two-layer system with a thin wire protocol between agents and the control plane. Curated; update when the design shifts, not on every code change.

## Layers

| Layer | Repo | Responsibility |
|---|---|---|
| **Framework** | `nixfleet` | mkHost / mkFleet API, contract schemas (`contracts/`), pluggable contract impls (`impls/` exposed at `flake.scopes.*`), wire protocol, control plane, agent, reconciler, CI signing primitives, JCS canonicalization |
| **Consumer** | `fleet` (your repo) | Everything fleet-shaped: service deployments, role bundles, hardware modules, profiles, secrets, host definitions. Composed via `mkHost` from the framework |

## Components

```
┌─────────────────────────────────────────────────────────────────┐
│                        Operator workstation                      │
│  - mints bootstrap tokens (org-root key)                         │
│  - pushes config changes to fleet repo                           │
│  - signs `fleet.resolved.json` via TPM-backed CI key (Phase 7)   │
└──────────────┬──────────────────────────────────────────────────┘
               │ git push
               ▼
┌─────────────────────────────────────────────────────────────────┐
│                       Forgejo (lab repo host)                    │
│  - hosts fleet repo                                              │
│  - stores releases/fleet.resolved.json + .sig                    │
│  - runs CI: build closures, sign, commit [skip ci] release       │
└──────────────┬──────────────────────────────────────────────────┘
               │ HTTPS poll (every 60s)
               ▼
┌─────────────────────────────────────────────────────────────────┐
│                       Control plane (lab)                        │
│  - verifies fleet.resolved against trust.json (ed25519 / ECDSA)  │
│  - keeps verified_fleet snapshot in memory                       │
│  - per-checkin dispatch: declared closure_hash → target          │
│  - SQLite: pending_confirms, token_replay, cert_revocations      │
│  - issues + renews agent certs from fleet CA (online; #41)       │
└──────────────┬──────────────────────────────────────────────────┘
               │ mTLS, /v1/agent/checkin every 60s
               ▼
┌─────────────────────────────────────────────────────────────────┐
│                          Agents (per host)                       │
│  - poll-only: never accept inbound connections                   │
│  - `nix-store --realise` + `nix-env --set` + `switch-to-config`  │
│  - post-switch verify: /run/current-system basename match        │
│  - magic rollback: agent on activation fail OR on /confirm 410   │
│  - emit /v1/agent/report on every failure path                   │
└─────────────────────────────────────────────────────────────────┘
```

## Sequence: a config commit propagates

```
operator     git push origin main
                │
                ▼
Forgejo      runs CI: build host closures, sign artifact, commit [skip ci]
                │
                ▼  (poll, ≤60s later)
CP (lab)     forgejo_poll fetches releases/{json,sig}; verify_artifact;
             writes verified_fleet snapshot under signed_at freshness gate
                │
                ▼  (next agent checkin)
agent → CP   POST /v1/agent/checkin { current_generation: <basename> }
CP → agent   200 { target: { closure_hash: <newer-basename>, … } }
                │
                ▼
agent        nix-store --realise /nix/store/<basename>
             nix-env --profile … --set /nix/store/<basename>
             /nix/store/<basename>/bin/switch-to-configuration switch
             readlink /run/current-system → basename matches → ✓
                │
                ▼
agent → CP   POST /v1/agent/confirm { rollout, generation }
CP           pending_confirms.state = 'confirmed'
                │
                ▼
agent        next checkin reports new current_generation
CP           dispatch::Decision::Converged → target: null
```

## Key contracts

- **`fleet.resolved.json`**: signed JCS-canonical artifact. Contains channels, hosts (with `closureHash`), rollout policies, waves, edges, disruption budgets. Authoritative source for what every host should run.
- **mTLS**: every `/v1/*` endpoint requires a fleet-CA-issued client cert. CN is the hostname.
- **Bootstrap token**: ed25519-signed claim, single-use (replay-protected), used once for first-boot enrollment.
- **closure_hash**: the basename of the `/nix/store` path. Same shape on the wire (CP → agent), in the artifact (CI → CP), and in agent's `CheckinRequest.current_generation` (agent → CP).

## Sovereignty posture (current vs target)

| Property | Today | Target | Tracking |
|---|---|---|---|
| CP holds no secrets | 🟡 fleet CA private key online | TPM-bound CA, offline root | [#41](https://github.com/abstracts33d/nixfleet/issues/41) |
| Trust roots disconnected from CP | ✅ org-root key never reaches CP | — | — |
| Host identity = host key | 🟡 fresh keypair per enroll | host SSH key derives CSR | [#43](https://github.com/abstracts33d/nixfleet/issues/43) |
| Reproducible artifact verify | ✅ JCS + ed25519/ECDSA | — | — |
| Agent-side closure verify | ✅ realise + post-switch basename | + signed-narinfo verify (Phase 7) | — |
| Compliance gates | ❌ static-only | probe execution + signed evidence | [#4](https://github.com/abstracts33d/nixfleet/issues/4), [#13](https://github.com/abstracts33d/nixfleet/issues/13) |
