# `EvidenceCollector` trait

The architectural commitment that lets adapters for different evidence
sources plug into the canonical per-host record without schema rework.
Lives in `crates/nixfleet-cli/src/commands/evidence/collector.rs`.

## The trait

```rust
pub trait EvidenceCollector {
    fn id(&self) -> &'static str;
    fn collect(&self, ctx: &CollectorContext<'_>) -> Result<CollectorEntry>;
}
```

Operator-side, synchronous, failure-isolated. One adapter failing
returns `Err`; the aggregator logs the error and continues with
sibling adapters. No adapter can abort the run.

## `CollectorContext`

```rust
pub struct CollectorContext<'a> {
    pub hostname: &'a str,
    pub host: &'a Host,                            // declarative
    pub fetched: Option<&'a FetchedHost>,          // operator-fetched bytes
}
```

| Field | Source | Use |
|---|---|---|
| `hostname` | `fleet.resolved.json` | Used by every adapter for logging + record keying. |
| `host` | `fleet.resolved.json` declarative slot | Adapters reading the operator-declared state (`closureHash`, `channel`, `system`, `tags`, `pin`). |
| `fetched` | SSH-fetched bytes (or `None` when fetch failed) | Adapters consuming host-emitted blobs (`facter.json`, future `osquery` output, future `tpm2-quote`). |

Adapters that read only declarative state ignore `fetched`. Adapters
that consume bytes from the host check `ctx.fetched.and_then(...)`
and record `available: false` (or equivalent) when the data isn't
present.

## `CollectorEntry`

```rust
pub struct CollectorEntry {
    pub collector_id: String,        // stable per adapter
    pub data: serde_json::Value,     // adapter-specific blob
}
```

The wrapper schema stores a `Vec<CollectorEntry>` per host. Renderers
key off `collector_id` to know how to interpret `data`. Adapter
authors choose the `data` shape; it should be additive (adding
fields is fine; renaming requires coordination with renderers).

## Existing adapters as worked examples

### `nix-derivation` (declarative shape)

`collectors/nix_derivation.rs`. Pure data extraction. Reads
`ctx.host.{closure_hash, channel, system}` and surfaces them as a
`CollectorEntry`. No subprocess. No fetched data needed.

```rust
let data = NixDerivationData {
    closure_hash: ctx.host.closure_hash.clone(),
    channel: ctx.host.channel.clone(),
    system: ctx.host.system.clone(),
};
```

This is the intent side of the eventual intent-vs-reality reconciliation
surface: "what we declared the host should be running."

### `facter` (fetched-data shape)

`collectors/facter.rs`. Consumes a best-effort host-emitted
`facter.json` (added to the SSH fetch loop with missing-is-ok
semantics). Surfaces parsed JSON verbatim as `data`. Subprocess
hygiene: the `nixos-facter` binary runs on the host as a standard
probe; the operator-side adapter only parses its JSON output.
Never link or vendor facter into nixfleet code (it is GPL v3).

```rust
let bytes = ctx.fetched.and_then(|f| f.facter_json.as_ref());
match bytes {
    None => FacterData { available: false, raw: None, parse_error: None },
    Some(b) => match serde_json::from_slice::<serde_json::Value>(b) {
        Ok(v)  => FacterData { available: true,  raw: Some(v), parse_error: None },
        Err(e) => FacterData { available: false, raw: None, parse_error: Some(e.to_string()) },
    },
}
```

This is the hardware side: TPM presence, Secure Boot keys, kernel
version, package set. Auditor uses it to verify the running host
matches the declared closure.

## Adding a new adapter

1. Create `crates/nixfleet-cli/src/commands/evidence/collectors/<name>.rs`.
2. Define an `<Name>Data` struct (serde) — the JSON payload your adapter emits.
3. Define a unit struct `<Name>Collector` and `impl EvidenceCollector for <Name>Collector`.
4. Register in `collectors/mod.rs` with `pub mod <name>;`.
5. Add the adapter to the `collectors` Vec in `collect::run` (the operator-side configured list).
6. Tests at the module bottom: at minimum, one happy-path and one absent-input case.

## When to subprocess

Adapters running operator-side. Adapters consuming host-emitted bytes
do not subprocess to the host; the SSH fetch loop already did that.

If a future adapter needs to subprocess on the operator's machine
(e.g., `cosign verify`, `nix store verify`, `tpm2_quote_verify`), use
`tokio::process::Command`. Document the binary dependency in the
adapter's module doc comment so operators know what their environment
must provide.

## Best-effort fetches

The SSH fetch loop currently fetches three required files
(`evidence.json`, `evidence.json.sig`, `evidence.host.pub`) and one
best-effort file (`facter.json`). To add another best-effort file:

1. Add a constant in `fetch_ssh.rs` (`pub const <NAME>_FILENAME: &str`).
2. Add a field to `FetchedHost` (`pub <name>_json: Option<Vec<u8>>`).
3. In `fetch_one_host`, after the required files succeed, attempt the
   extra fetch and silently set `None` on failure (do not flip
   `ok = false`).

## Trait survivability discipline

Two adapters validate the trait. One adapter is underspec. M1.2's
`nix-derivation` reads only declarative state; M1.3's `facter` reads
fetched bytes — the trait survives both shapes by carrying the union
as `CollectorContext.fetched: Option<...>`.

When adding a third adapter, audit the trait against the new shape
before committing:

| Adapter shape | Trait survives? |
|---|---|
| Reads only `ctx.host` | Yes (existing). |
| Reads only `ctx.fetched.<field>` | Yes (existing). |
| Reads operator-side env (e.g., `~/.config/...`) | Yes; the trait does not constrain side input. |
| Reads CP state (HTTPS to control plane) | Trait survives mechanically, but breaks the operator-side / synchronous design. Consider whether the data belongs in the per-host `evidence.json` instead. |
| Needs async I/O | Trait is sync; wrap your `.await`s in a `tokio::runtime::Builder` block, or refactor the trait to `async fn collect`. Discuss before doing the latter. |

The trait gets revised when the second adapter of a new shape breaks
it cleanly. Until then, treat it as stable.
