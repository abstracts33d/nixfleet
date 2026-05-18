# nixfleet-state-machine

**Role.** Pure per-host rollout state-machine reducer (RFC-0005 §3 + RFC-0006 §3). A single `step(state, event, now, policy) -> Result<(state, Vec<Effect>), TransitionError>` function. No I/O, no clock reads, deterministic. The same crate runs in the agent (drives the host's local state from worker output) and the CP (mirrors that state from inbound events) — both sides share the reducer by construction.

**Key types.** `HostRolloutState` (the 6-state machine: Pending, Activating, Soaking, Soaked / Failed / Reverted / Deferred / Converged variants per RFC-0005 §3), `Event` (the input vocabulary — `Local*` variants emitted by the agent, `Remote*` mirrors synthesized CP-side from wire `AgentEvent`s), `Effect` (side-effect descriptors the runtime applies — `LocalEmitEvent`, `RemoteAppendEventLog`, `RunActivation`, `RunRollback`, …), `ProbeSubResult` (per-control accounting carried on evidence-probe results — RFC-0007 §3.4), `RolloutId` newtype (canonical `{channel}@{channel_ref}` per RFC-0008 §6.3).

**Surface.** Library only. Public entry points: `step` (the canonical reducer), `wire_conversions` (bidirectional `AgentEvent ↔ Event` / `OutboundAgentEvent ↔ AgentEvent` maps that keep `nixfleet-proto` free of state-machine awareness per the d013 lift, RFC-0004 §2). `Cargo.toml`'s dependency list is part of the safety contract — tokio / reqwest / rusqlite are forbidden; CI verifies via `cargo tree`.

**Links.**

- Generated rustdoc: [`api/nixfleet_state_machine/`](../../api/nixfleet_state_machine/index.html)
- Relevant RFCs: [RFC-0005](../../rfcs/0005-event-driven-host-rollout-state.md), [RFC-0006](../../rfcs/0006-control-plane-architecture.md), [RFC-0007](../../rfcs/0007-multi-scope-health-probes.md), [RFC-0004](../../rfcs/0004-architectural-patterns.md), [RFC-0008](../../rfcs/0008-rollout-state-machine-and-derived-views.md)
- Architecture component: [§1.4 Control plane](../../design/architecture.md#14-control-plane-the-router), [§1.5 Agent](../../design/architecture.md#15-agent-the-actuator)
