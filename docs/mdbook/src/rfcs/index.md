# RFCs

Authoritative design documents for the v0.2+ contract. Each RFC owns one boundary; together they define what is load-bearing across releases.

| RFC | Topic | Status |
|-----|-------|--------|
| [RFC-0001](0001-fleet-nix.md) | Declarative fleet topology (`mkFleet`, selectors, rollouts) | Accepted |
| [RFC-0002](0002-reconciler.md) | Reconciler decision procedure | Accepted |
| [RFC-0003](0003-protocol.md) | Agent / control-plane wire protocol | Accepted |
| [RFC-0004](0004-architectural-patterns.md) | Architectural-pattern checklist (lift discipline) | Descriptive |
| [RFC-0005](0005-event-driven-host-rollout-state.md) | Event-driven host-rollout state machine | Accepted |
| [RFC-0006](0006-control-plane-architecture.md) | Control-plane functional core / imperative shell | Accepted |
| [RFC-0007](0007-multi-scope-health-probes.md) | Multi-scope health probes + compliance shorthand | Accepted |
| [RFC-0008](0008-rollout-state-machine-and-derived-views.md) | Rollout-level state machine + derived-view discipline | Accepted |
| [RFC-0009](0009-hardware-rooted-trust.md) | Hardware-rooted trust (TPM, attestation) | v0.3 target |
| [RFC-0010](0010-trust-lifecycle.md) | Trust lifecycle (operator roles, rotation) | v0.3 target |
| [RFC-0011](0011-freshness-window-policy.md) | Freshness-window policy | v0.3 target |
| [RFC-0012](0012-air-gapped-operation.md) | Air-gapped operation (signed bundles) | v0.3 target |

The RFC pages above are mdbook wrappers that include the canonical sources from the repo's `docs/rfcs/` tree.
