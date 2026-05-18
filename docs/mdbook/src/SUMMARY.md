# Summary

[Introduction](introduction.md)

# Design

- [Architecture](design/architecture.md)
- [Contracts](design/contracts.md)
- [Source layout](design/source-layout.md)

# Reference

- [Test harness](reference/harness.md)
- [Crates](reference/crates/index.md)
  - [nixfleet-agent](reference/crates/nixfleet-agent.md)
  - [nixfleet-canonicalize](reference/crates/nixfleet-canonicalize.md)
  - [nixfleet-cli](reference/crates/nixfleet-cli.md)
  - [nixfleet-control-plane](reference/crates/nixfleet-control-plane.md)
  - [nixfleet-proto](reference/crates/nixfleet-proto.md)
  - [nixfleet-reconciler](reference/crates/nixfleet-reconciler.md)
  - [nixfleet-release](reference/crates/nixfleet-release.md)
  - [nixfleet-state-machine](reference/crates/nixfleet-state-machine.md)
  - [nixfleet-verify-artifact](reference/crates/nixfleet-verify-artifact.md)
- [Rust API (cargo doc)](api.md)

# Operations

- [Quickstart](operations/quickstart.md)
- [Operator cookbook](operations/operator-cookbook.md)
- [Bootstrap token lifecycle](operations/bootstrap-token-lifecycle.md)
- [VM lifecycle](operations/vm-lifecycle.md)
- [Testing](operations/testing.md)
- [Disaster recovery](operations/disaster-recovery.md)
- [Troubleshooting](operations/troubleshooting.md)

# RFCs

- [Index](rfcs/index.md)
- [RFC-0001 - Declarative fleet topology](rfcs/0001-fleet-nix.md)
- [RFC-0002 - Reconciler](rfcs/0002-reconciler.md)
- [RFC-0003 - Agent / CP protocol](rfcs/0003-protocol.md)
- [RFC-0004 - Architectural patterns](rfcs/0004-architectural-patterns.md)
- [RFC-0005 - Event-driven host-rollout state](rfcs/0005-event-driven-host-rollout-state.md)
- [RFC-0006 - Control-plane architecture](rfcs/0006-control-plane-architecture.md)
- [RFC-0007 - Multi-scope health probes](rfcs/0007-multi-scope-health-probes.md)
- [RFC-0008 - Rollout state machine + derived views](rfcs/0008-rollout-state-machine-and-derived-views.md)
- [RFC-0009 - Hardware-rooted trust](rfcs/0009-hardware-rooted-trust.md)
- [RFC-0010 - Trust lifecycle](rfcs/0010-trust-lifecycle.md)
- [RFC-0011 - Freshness-window policy](rfcs/0011-freshness-window-policy.md)
- [RFC-0012 - Air-gapped operation](rfcs/0012-air-gapped-operation.md)
