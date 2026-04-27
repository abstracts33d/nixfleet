# RFCs

Authoritative design documents for the v0.2 contract. Each RFC owns one boundary; together they define what's load-bearing across releases.

| RFC | Title | Owns |
|---|---|---|
| [0001](0001-fleet-nix.md) | Declarative fleet topology | `fleet.nix` schema → `fleet.resolved.json` shape, channels, hosts, rollout policies |
| [0002](0002-reconciler.md) | Rollout execution engine | Pure reconcile() decision procedure, state machine, action stream |
| [0003](0003-protocol.md) | Agent ↔ control plane protocol | Wire types, mTLS posture, confirm + magic rollback semantics |

The RFC files in this directory are copied verbatim from the repo's `rfcs/` tree by `nix run .#docs`.
