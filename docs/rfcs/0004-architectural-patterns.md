# RFC-0004: Architectural patterns

**Status.** Descriptive (not prescriptive of new code; documents the discipline RFC-0005, RFC-0006, RFC-0007, RFC-0008 converged on).
**Depends on.** RFC-0005, RFC-0006, RFC-0007.
**Scope.** Names the recurring pattern feature work in nixfleet evaluates itself against: *lift to the general pattern that already exists* rather than build bespoke shapes for each new concern. Provides the checklist new feature plans run before drafting wire types or DB schemas.
**Not normative.** Introduces no wire types, DB schemas, or module options.

## 1. The observation

Several distinct decisions converged on the same shape:

| Before | After |
|---|---|
| Inference from successive checkin diffs | Explicit events through `event_log` (RFC-0005) |
| Scattered mutable state with RwLocks | One MPSC + one mutator per side (RFC-0006 §7) |
| Manifest schema expansion per new feature | Closure-hash chain transitively signs declarations (RFC-0007 §4) |
| Per-channel special-cased compliance flags | Per-probe `mode` uniform across all probe kinds (RFC-0007 §3.3) |
| Independent table populated by applier writes | Derived view with `event_log_seq` FK-back to canonical state (RFC-0007 §7.2, RFC-0008 §6) |

In each case the framework already paid the cost of supporting the general pattern; the bespoke alternative would have compounded the maintenance surface for no shared infrastructure benefit.

## 2. The pattern, named

> **When config-or-state is expressed in a *narrower or more specific shape* than a *general pattern that already exists* in the framework, prefer the general pattern.**

This is not "always abstract to the maximum." The principle is: the framework already pays the cost of supporting general patterns (the event-log writer task, the closure-hash signing chain, the multi-scope `mkFleet` resolver). A new feature that reaches for a bespoke shape *also pays a cost*, but doesn't share infrastructure with anything else. The bespoke shape compounds.

Concretely, four levers exist where the general pattern is already cheap to apply:

### 2.1 State-mutating logic → pure reducer + applier effect

If a piece of code mutates state (per-host, per-rollout, per-channel), and the mutation has explicit transitions, model it as a pure `step(state, event, now) → (state, Vec<Effect>)` reducer in `nixfleet-state-machine`. The applier handles effects. The framework already pays for one MPSC + one mutator per side (RFC-0006 §7); the new state machine plugs in.

Counter-indication: the mutation is essentially "write this value, no transition semantics." Then it's a setter, not a state machine.

### 2.2 Per-(host|channel|rollout) config → fleet/tag/host multi-scope merge

If operators declare it and might change it more than once per quarter, declare options at `nixfleet.<thing>` (fleet) / `nixfleet.tags.<tag>.<thing>` (tag) / `nixfleet.hosts.<host>.<thing>` (host). `mkFleet` resolves with `host > tag > fleet` precedence. The framework already pays for this resolver (RFC-0007 §4).

Counter-indication: the config is set once at infrastructure-bootstrap (trust roots, signing keys). Then it's per-fleet only.

### 2.3 Per-host signed declaration → closure-hash chain, not signed manifest

If declarations are per-host and rendered into `/etc/nixfleet/agent/*.json` from the host's NixOS module, the closure hash transitively signs them. Adding a top-level signed manifest field for the same content denormalizes and grows the signing surface for no security gain (RFC-0007 §5).

Counter-indication: the content is fleet-wide or cross-host (e.g., the host_set or the channel ref). Then it belongs in the manifest payload, not in any single host's closure.

### 2.4 Applier-written DB table → derived view with `event_log_seq` FK-back

If a table is written exclusively by the applier in response to events, structure it as a derived view: write `event_log` row AND derived-view row in the same transaction; carry `event_log_seq` as a primary-key foreign-key back to the canonical store; ensure the table is provably re-derivable from `event_log` if lost (RFC-0007 §7.2, RFC-0008 §6).

Counter-indication: the table is short-lookup security-critical state (`token_replay`, `cert_revocations`) with a TTL lifecycle distinct from `event_log`'s append-only audit. Then it's a separate concern.

## 3. Evaluation checklist for new features

When writing a plan for a new feature, run these questions before drafting wire types or DB schemas:

1. **Does this mutate state with explicit transitions?** → reducer in `nixfleet-state-machine` (per §2.1)
2. **Is this per-host declarative config operators will change?** → multi-scope `nixfleet.{*,tags.*,hosts.*}` options (per §2.2)
3. **Does this need to be cryptographically signed?** → check whether the closure-hash chain already covers it (per §2.3) before adding a manifest field
4. **Is this a table the applier writes?** → derived view with `event_log_seq` FK-back (per §2.4)
5. **Does the wire need a new event variant?** → fit into existing event taxonomy first; only add a new variant if the semantics don't fold into an existing kind.

If the answer to any of 1-4 is yes and you find yourself reaching for the bespoke alternative, you're deferring the right shape.
