# `nixfleet_reconciler::verify`

RFC-0002 §4 step 0 — fetch + verify + freshness-gate.

## Items

### 🔒 `const ACCEPTED_SCHEMA_VERSION`

Accepted `schemaVersion` for this consumer.


### 🔓 `enum VerifyError`

_(no doc comment)_


### 🔓 `fn verify_artifact`

Verify a signed `fleet.resolved` artifact per RFC-0002 §4 step 0.

# Trust root list

`trusted_keys` is a list to support [`CONTRACTS.md §II`]'s rotation
grace window — during a key rotation, the previous and current keys
are BOTH valid trust roots for up to 30 days. The verifier tries
each in declaration order; the first key whose algorithm is
supported AND whose `verify_strict` accepts the signature wins.

Entries with unsupported algorithms are skipped (with no error),
enabling forward compatibility: when `#18` amends §II to add e.g.
`p256`, an older verifier binary can still operate against a
mixed-algorithm `trust.ciReleaseKeys` list; it just only matches
the subset of keys whose algorithms it knows.

# Signature width

`signature` is a byte slice, not a fixed-size array. Per-algorithm
length validation happens inside the dispatcher. ed25519 expects
exactly 64 bytes (32-byte R || 32-byte s). A future `p256` branch
will decide whether to accept raw r||s (64 bytes) or DER-encoded
(variable) — for now, non-ed25519 algorithms bail with
`UnsupportedAlgorithm`.

# Compromise switch

`reject_before` is the slot-wide compromise kill-switch per
[`docs/trust-root-flow.md §7.2`][flow] / `CONTRACTS.md §II #1`.
When `Some(ts)`, any artifact whose `meta.signedAt < ts` is rejected
with [`VerifyError::RejectedBeforeTimestamp`] regardless of which
trust root matched the signature. `None` disables the gate. The
check fires before the `freshness_window` check so alerts can
distinguish an operator-declared incident response from routine
staleness. The comparison is strict `<` — an artifact signed
exactly at `reject_before` is accepted.

[flow]: ../../../docs/trust-root-flow.md


### 🔒 `fn verify_ed25519`

Dispatched verification for ed25519. `verify_strict` rejects malleable
signatures (non-canonical R or `s >= L`) — required for root-of-trust
keys per CONTRACTS.md §II #1.


### 🔒 `fn verify_ecdsa_p256`

Dispatched verification for ECDSA P-256 (NIST curve) per #18 §II.

Public key encoding per CONTRACTS.md §II #1: 64 bytes `X ‖ Y`
(uncompressed SEC1 point with the `0x04` tag stripped), base64-encoded.
Signature encoding: 64 bytes `R ‖ S`, raw (no DER wrapping).

Low-s malleability rejection: ECDSA signatures on Weierstrass curves
are malleable — if `(r, s)` is valid, so is `(r, n − s)`. Canonical
p256 signatures have `s <= n / 2`. The `p256` crate's
`Signature::normalize_s()` returns `Some(normalized)` iff the input
was high-s; we reject any such signature outright. Required for
root-of-trust keys per the same hardening pattern as `verify_strict`
on ed25519.


### 🔒 `fn finish_verification`

Steps 4-6 after signature verification: type-parse, schema-gate,
reject_before compromise switch, freshness.

Ordering rationale: `reject_before` is an operator-declared incident
response (slot-wide compromise switch per trust-root §7.2), a
stronger signal than routine staleness. We surface the more specific
error first so logs and alerts can distinguish "key compromised,
rotate" from "CI is behind, re-run".


