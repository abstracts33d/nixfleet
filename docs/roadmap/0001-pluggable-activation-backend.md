# Roadmap 0001 — Pluggable activation backend

**Status:** deferred. **Last updated:** 2026-04-27. **Owner:** open.

## What this is

The agent's activation pipeline (`crates/nixfleet-agent/src/activation.rs`)
is currently NixOS-coupled: it shells out to `nixos-rebuild
switch --system <path>`, then post-verifies by reading
`realpath /run/current-system` and comparing the basename. That
contract is fine for any active **NixOS** fleet host, but it
forecloses three categories of host that should plausibly
participate in a NixFleet fleet:

1. **Darwin hosts.** nix-darwin's switch model is
   `darwin-rebuild switch`; there is no `/run/current-system` —
   the active generation lives at `/run/current-system` symlink
   shape *similar but not identical*, and the current-generation
   resolution path differs. The Darwin agent today is a paper
   option tree — declared, never activates anything.
2. **Non-NixOS Linux** with `system-manager`-style declarative
   layers. Fits the same protocol (signed closure → realise →
   activate → verify) but with a different switch primitive.
3. **microVM hosts.** A microVM "switch" is a guest-side reload
   (or full restart) coordinated by the host. Different again.

This roadmap captures the design decisions to make whenever
this work becomes a priority.

## Why deferred

This is a **project**, not a refactor. Three reasons:

1. The agent's safety properties are tied to NixOS' generation
   model. Each new backend has to re-prove them. "Pre-realise +
   switch + post-verify" is a contract; expressing the same
   contract for darwin-rebuild requires understanding what
   "current system" means there and writing test fixtures that
   exercise the rollback path. That's RFC-level work, not just
   adding a `match` arm.
2. The agent is the most safety-critical component in the
   system. A bug in activation can brick a host. Speculative
   refactors of safety-critical code, with no second backend in
   active use to drive the abstraction's shape, tend to design
   the wrong interface.
3. The current decoupling work (phases 1–13) covers every
   tech-specific concern *outside* the agent's activation step.
   This is the last hard NixOS coupling and it stays explicit
   under CONTRACTS §VI ("Irreducible technology assumptions").

When a Darwin-active fleet (or a non-NixOS host) is the goal,
revisit this roadmap.

## Sketch

### Rust trait

```rust
// crates/nixfleet-agent/src/activation/backend.rs

#[async_trait::async_trait]
pub trait ActivationBackend: Send + Sync {
    /// Force the substituter to fetch + signature-check the closure
    /// before activation. Fails closed: if the closure isn't
    /// available or trust is misconfigured, we never proceed.
    async fn pre_realise(&self, closure: &Path) -> Result<(), ActivationError>;

    /// Activate the closure as the current generation.
    async fn switch(&self, closure: &Path) -> Result<(), ActivationError>;

    /// Resolve the current active generation's closure path.
    /// Used by post-verify to confirm that switch landed on the
    /// expected closure.
    async fn current_system(&self) -> Result<PathBuf, ActivationError>;

    /// Roll back to the previous generation. Triggered when
    /// post-verify or confirm-timeout fail.
    async fn rollback(&self) -> Result<(), ActivationError>;
}
```

### Implementations

```rust
struct NixosBackend;            // current — nixos-rebuild + /run/current-system
struct DarwinBackend;           // darwin-rebuild + nix-darwin's current-system
struct SystemManagerBackend;    // system-manager (Linux non-NixOS)
struct MicrovmBackend;          // microvm guest reload over a host channel
```

Each variant captures its own `current_system` resolution
algorithm and `switch`/`rollback` invocations. The agent's
existing `activation::activate()` becomes a generic over
`ActivationBackend`.

### Backend selection

Two reasonable selection strategies; pick one when the work is
scheduled:

- **Compile-time:** `cfg(target_os = "macos")` selects Darwin,
  `cfg(target_os = "linux")` selects NixOS or SystemManager
  based on a runtime probe (presence of
  `/run/current-system/sw/bin/nixos-rebuild` vs
  `system-manager`). Fewer moving parts, larger binary.
- **Runtime:** `--activation-backend nixos|darwin|system-manager`
  CLI flag (defaulted by host platform). More flexible for
  testing, slightly harder to reason about.

The compile-time route fits the framework's "explicit dependency"
posture better — a Darwin agent binary and a Linux agent binary
are distinct artifacts produced by the same crate with different
Cargo features (`features = ["activation-darwin"]`,
`features = ["activation-nixos"]`).

## Implementation contract

When the work begins, these are the boundaries to preserve:

1. **Existing `activation::activate()` becomes generic.** No
   change to caller-side semantics — the reconciler dispatches
   a `Decision`, the agent calls `activate()`, and three hooks
   fire (`pre_realise`, `switch`, `current_system` for verify).
2. **Post-verify invariant retained.** Whatever `current_system()`
   returns, the agent compares its basename against the expected
   closure hash. If the basename comparison can't be done (e.g.
   Darwin's path layout differs), the backend supplies an
   `assert_active_closure(&self, expected: &str)` method
   instead of `current_system()` — the post-verify becomes
   "ask the backend to assert."
3. **Rollback semantics.** Currently NixOS-specific (boot back
   to the previous generation). Each backend defines what
   "rollback" means in its own world. The reconciler's confirm-
   timeout path calls `rollback()` and trusts the backend to
   leave the host in a known-good state.
4. **No protocol changes.** The agent ↔ CP wire (CONTRACTS §I #2)
   stays the same. The CP doesn't know which backend the agent
   is running.
5. **CONTRACTS amendment.** Adding a Darwin backend is a §VIII
   amendment because it's a new wire-protocol participant
   (Darwin's `closureHash` shape is different from NixOS' —
   may need a `system: aarch64-darwin` discriminator field).
   Don't bypass.

## Files to touch

- `crates/nixfleet-agent/src/activation.rs` → split into
  `activation/{mod.rs, backend.rs, nixos.rs, darwin.rs, ...}`.
- `crates/nixfleet-agent/Cargo.toml` → cargo features per
  backend.
- `crates/nixfleet-agent/src/main.rs` → backend selection
  (compile-time or runtime flag).
- `tests/harness/` → add Darwin VM test if Darwin backend is
  the first new entry.
- `docs/CONTRACTS.md §VI` → move the "NixOS" assumption from
  irreducible to "irreducible *for the default backend*";
  document the backend trait as the contract for additional
  participants.
- `docs/ARCHITECTURE.md` → activation diagram update.
- `RFC-0001` (or new RFC) → multi-platform `closureHash` shape
  if Darwin support requires schema discriminator.

## Cost estimate

Rough sizing assuming Darwin is the first new backend:

- Backend trait + NixOS backend extraction: 1 day. Pure
  refactor of existing code into the trait shape.
- Darwin backend impl: 3–5 days. Reading nix-darwin's
  generation model, finding the right primitives, writing the
  rollback path, fixture testing.
- Tests + harness updates: 2–3 days. Darwin VM test fixtures,
  cross-platform CI matrix.
- Docs + CONTRACTS amendment: 1 day.

**Total:** ~1–2 weeks of focused work. Not a side project.

## Pointers for whoever picks this up

- Read `crates/nixfleet-agent/src/activation.rs` end-to-end
  first. The current implementation is short (~150 lines of
  meaningful code) and well-commented; the safety-critical
  decisions are explicit.
- Read RFC-0003 (agent ↔ CP protocol) and CONTRACTS §I #1, §I #2
  before touching any wire shape.
- The reconciler's confirm-timeout path is the place where
  `rollback()` is invoked. Walk that path before deciding how
  the backend trait expresses rollback.
- Don't try to abstract the substituter / cache-fetch step.
  That's already implementation-agnostic (see CONTRACTS §VI).
  The activation backend trait is *only* about the per-platform
  generation-switching primitive.
