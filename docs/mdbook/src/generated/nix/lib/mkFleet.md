# `lib/mkFleet.nix`

lib/mkFleet.nix

Produces `fleet.resolved` per RFC-0001 §4.1 + docs/CONTRACTS.md §I #1.
Output is canonicalized to JCS (RFC 8785) by `bin/nixfleet-canonicalize`
(owned by Stream C) before signing — DO NOT introduce floats, opaque
derivations, or attrsets whose iteration order is significant here.

## Bindings

### `hostType`

--- Host ---

### `hasCycle`

Tarjan-free cycle detection using iterative DFS marking.
Edges: { after = "a"; before = "b"; } means a must finish before b starts.
So we walk "after → before" edges.

### `resolveSelector`

--- Selector resolution: selector × hosts → [host-name] ---
Variant precedence (RFC-0001 §3): `not` > `and` > base OR composition.
Base OR = host matches iff any of tags/tagsAny/hosts/channel/all matches.

### `checkInvariants`

--- Invariant checks (RFC-0001 §4.2) ---

### `resolveFleet`

--- Resolved projection (RFC-0001 §4.1) ---

### `withSignature`

Stamp CI-provided signing metadata onto a resolved fleet value.
`signatureAlgorithm` is optional — omit it when signing with ed25519
(the default per CONTRACTS §I #1 for backward-compatible consumers).
Set it to `"ecdsa-p256"` (or any future value the contract accepts)
when Stream A's CI signs with a non-default algorithm, e.g. when the
TPM keyslot emits ECDSA P-256.

### `mergeFleets`

--- Composition (RFC-0001 §5) ---
Merge a list of mkFleet-input attrsets into a single fleet value.
Precedence rules:
  - hosts / tags / channels: strict merge — same name across inputs throws.
  - rolloutPolicies: later wins (associative, not commutative per RFC §5).
  - edges / disruptionBudgets: concatenated (no dedup; order preserved).
  - complianceFrameworks: union of whatever each input specified; if no
    input declared any, the mkFleet default list applies.

