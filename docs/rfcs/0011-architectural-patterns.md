# RFC-0011: Architectural patterns for the v0.2.x cycle

**Status.** Draft (descriptive, not prescriptive of new code; documents the discipline used by RFC-0008, RFC-0009, RFC-0010 and anticipated for RFC-0012+).
**Depends on.** RFC-0008, RFC-0009, RFC-0010.
**Scope.** Documents the recurring architectural pattern the v0.2 fold has converged on — *lift to the general pattern that already exists* rather than build bespoke shapes for each new concern. Provides a checklist future feature work evaluates itself against. Enumerates known levers waiting to be pulled.
**Not normative.** This RFC does not introduce wire types, DB schemas, or module options. It is the meta-document the cycle's reviewers and implementers consult when triaging whether a new feature is reaching for a bespoke shape that would accumulate workaround LoC over time.

## 1. The observation

Five distinct decisions during the v0.2 cycle converged on the same shape:

| Before | After | What was reclaimed |
|---|---|---|
| Inference from successive checkin diffs (RFC-0003 §4.1) | Explicit events through `event_log` (RFC-0008) | ~6 scattered state-inference branches; ~1,100 LoC in legacy `reconcile.rs` |
| Scattered mutable state with RwLocks (`ProbeStateCache`, `host_reports` in-memory cache) | One MPSC + one mutator per side (RFC-0009 §7) | Race-handling branches, parallel-reader divergence |
| Manifest schema expansion per new feature | Closure-hash chain transitively signs declarations (RFC-0010 §4) | Wire-protocol bloat per feature; signing-surface growth |
| Per-channel special-cased compliance flags (`Channel.compliance.{mode, strict, frameworks}`) | Per-probe `mode` (uniform axis across all probe kinds) (RFC-0010 §3.3) | Bespoke compliance-gate code; special-case branches at every consumer |
| Independent table populated by applier writes (`host_reports`) | Derived view with `event_log_seq` FK-back (`probe_failures`) (RFC-0010 §7.2) | Shadow state; two-writer divergence; ad-hoc pruning |

Each decision saved 200-1,500 LoC of workaround code that would otherwise have lived in the codebase. The total v0.2 fold delivered ≈9,500 net LoC reduction in the Rust workspace despite adding two new pure crates and the verified-newtype safety layer.

## 2. The pattern, named

> **When config-or-state is expressed in a *narrower or more specific shape* than a *general pattern that already exists* in the framework, prefer the general pattern.**

This is not "always abstract to the maximum." The principle is: the framework already pays the cost of supporting general patterns (the event-log writer task, the closure-hash signing chain, the multi-scope `mkFleet` resolver). A new feature that reaches for a bespoke shape *also pays a cost*, but doesn't share infrastructure with anything else. The bespoke shape compounds.

Concretely, four levers exist where the general pattern is already cheap to apply:

### 2.1 State-mutating logic → pure reducer + applier effect

If a piece of code mutates state (per-host, per-rollout, per-channel), and the mutation has explicit transitions, model it as a pure `step(state, event, now) → (state, Vec<Effect>)` reducer in `nixfleet-state-machine`. The applier handles effects. The framework already pays for one MPSC + one mutator per side (RFC-0009 §7); the new state machine plugs in.

Counter-indication: the mutation is essentially "write this value, no transition semantics." Then it's a setter, not a state machine.

### 2.2 Per-(host|channel|rollout) config → fleet/tag/host multi-scope merge

If operators declare it and might change it more than once per quarter, declare options at `nixfleet.<thing>` (fleet) / `nixfleet.tags.<tag>.<thing>` (tag) / `nixfleet.hosts.<host>.<thing>` (host). `mkFleet` resolves with `host > tag > fleet` precedence. The framework already pays for this resolver (RFC-0010 §4).

Counter-indication: the config is set once at infrastructure-bootstrap (trust roots, signing keys). Then it's per-fleet only.

### 2.3 Per-host signed declaration → closure-hash chain, not signed manifest

If declarations are per-host and rendered into `/etc/nixfleet/agent/*.json` from the host's NixOS module, the closure hash transitively signs them. Adding a top-level signed manifest field for the same content denormalizes and grows the signing surface for no security gain (RFC-0010 §5).

Counter-indication: the content is fleet-wide or cross-host (e.g., the host_set or the channel ref). Then it belongs in the manifest payload, not in any single host's closure.

### 2.4 Applier-written DB table → derived view with `event_log_seq` FK-back

If a table is written exclusively by the applier in response to events, structure it as a derived view: write `event_log` row AND derived-view row in the same transaction; carry `event_log_seq` as a primary-key foreign-key back to the canonical store; ensure the table is provably re-derivable from `event_log` if lost (RFC-0010 §7.2).

Counter-indication: the table is short-lookup security-critical state (`token_replay`, `cert_revocations`) with a TTL lifecycle distinct from `event_log`'s append-only audit. Then it's a separate concern.

## 3. Evaluation checklist for new features

When writing a plan for a new feature, run these questions before drafting wire types or DB schemas:

1. **Does this mutate state with explicit transitions?** → reducer in `nixfleet-state-machine` (per §2.1)
2. **Is this per-host declarative config operators will change?** → multi-scope `nixfleet.{*,tags.*,hosts.*}` options (per §2.2)
3. **Does this need to be cryptographically signed?** → check whether the closure-hash chain already covers it (per §2.3) before adding a manifest field
4. **Is this a table the applier writes?** → derived view with `event_log_seq` FK-back (per §2.4)
5. **Does the wire need a new event variant?** → fit into existing event taxonomy first; only add a new variant if the semantics don't fold into an existing kind. (RFC-0010 §7 folded compliance into `kind = "evidence"` rather than adding `ComplianceFailure`.)

If the answer to any of 1-4 is yes and you find yourself reaching for the bespoke alternative, you're deferring the right shape. Stop and lift.

## 4. Known levers waiting

Surfaced during architectural review of the v0.2 cycle; not blocking but worth tracking:

| Lever | Severity | Status |
|---|---|---|
| Rollout-level state machine + uniform derived-view discipline | High | **Landed (Phase 10: `917b6188` + `1a0ddd3a` + `d0ae5ac0`).** RFC-0012 realized: `rollouts` lifecycle is a pure 8-state reducer in `nixfleet-state-machine::rollout`; `rollouts` + `quarantined_closures` joined `probe_failures` as derived views with `event_log_seq` FK-back (NULL-able pending v0.2.1-followups #1). Reducer's `current_wave_is_monotonic` proptest invariant caught a real bug during drafting — same lift-to-general payoff as the §1 entries: the discipline doesn't just prevent recurrence of past bugs, it finds new ones at design time. 10c collapsed the last implicit-side-effect anti-pattern (the inline supersession `UPDATE` in `record_rollout_opened`) into a reducer-driven `SuccessorOpened` path. |
| Multi-scope `disruption_budgets` declarations | Medium | Open. Worth lifting if operator workflow surfaces pain (currently `Vec<DisruptionBudget>` at fleet root with `Selector`). |
| `cert_revocations` + `token_replay` through `event_log` (uniform security audit trail) | Lower | v0.3 territory; current short-lookup-table shape is justified by lookup-rate semantics. |
| Metric emission unification (audit for direct `metric.inc()` outside `Effect::EmitMetric`) | Lower | Possibly already clean post-Phase 9; worth a one-pass grep. |
| Test fixture consolidation (`db/test_helpers.rs` shared helpers) | Lowest | Implementation polish, not architecture. |

Lever A+B moved from §4 to §1 territory: the destructive-cut shape it
delivered (`is_superseded`/`is_terminal`/`is_finished` boolean methods
deleted, `host_reports`-equivalent shadow state eliminated for two
more tables, `MarkChannelTerminal`/`ClearStaleQuarantine` compat
plumbing collapsed, inline-supersession side effect dissolved) is the
same caliber as the wins enumerated there. Lower-severity items are
flagged for operator-pain validation before pulling.

## 5. Anti-patterns this RFC actively discourages

- **Special-casing one feature with its own state, wire types, or DB table when the general pattern fits.** This is the disease the v0.2 cycle's destructive cuts (Phase 6g, 7g, 8c-8d, RFC-0010 deletions) all consumed.
- **Defending denormalisation as "operator UX optimisation" when a derived view at the CP-side gives the same query speed.** `host_reports` was the canonical example; the same shape was nearly reproduced in `RolloutManifest.compliance_frameworks` (RFC-0010 §6) until D2-a corrected.
- **Compatibility shims during v0.2.x cycle.** v0.2 is a full rewrite per RFC-0009 §12. Backward-compat code paths are the failure mode this entire cycle exists to prevent recurring.
- **"Just add a flag" thinking.** Each new flag adds operator-visible surface that must be documented, tested, and may be load-bearing for someone's deployment. RFC-0010 deleted three `_agent-args.nix` flags (`--poll-interval`, `--compliance-gate-mode`, `--health-checks-config`) in Phase 9a; each had been "just one flag" originally.

## 6. How this RFC evolves

This is a living document. When a new general pattern emerges that this RFC missed, future RFCs declare the pattern and amend this list. When a "known lever" lands (e.g., RFC-0012 ships), the entry moves from §4 to §1.

Treat this RFC as the cycle's institutional memory of "the lessons we keep learning the same way." When a reviewer or implementer cites it in a commit message ("per RFC-0011 §2.4 — applier-written table becomes derived view"), the discipline is operating.

## 7. Open questions / future patterns to canonicalize

- **Reducer composition** — Phase 9 introduces a probe-level concern that's neither purely host-state nor rollout-state. Does it warrant a third reducer? Or does it fold into one of the existing two? RFC-0012's per-rollout reducer may answer this; if a third pattern emerges, document.
- **Event-log retention vs replay-window** — `event_log` is append-only today. At fleet scale, retention becomes operational. The pattern for "old events expire" vs "old events are pruned but state is preserved" hasn't surfaced yet.
- **Cross-fleet patterns** — multi-fleet operators (operator running 3+ fleets from one machine) may surface patterns this RFC doesn't anticipate. Track if/when it surfaces.

The discipline of *recognizing and naming the pattern* is itself the deliverable. Code without this discipline accumulates scar tissue; code with it converges on shapes that compose.
