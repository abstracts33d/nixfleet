# `nixfleet_proto::enroll_wire`

Bootstrap token + enrollment + renewal wire types (RFC-0003 §5).

Phase 3 PR-5. Token format is JSON: `{version, claims, signature}`
where `signature` is a detached ed25519 signature over the JCS
canonical bytes of `claims` (the `nixfleet-canonicalize` crate
produces the same bytes consumers verify against). The org root
pubkey lives in `trust.json` under `orgRootKey.current`.

All types are wire-only: no crypto primitives leak from this
module — the CP and `nixfleet-mint-token` consume them via the
issuance and signing helpers in their own crates.

## Items

### 🔓 `struct BootstrapToken`

_(no doc comment)_


### 🔓 `struct TokenClaims`

_(no doc comment)_


### 🔓 `struct EnrollRequest`

_(no doc comment)_


### 🔓 `struct EnrollResponse`

_(no doc comment)_


### 🔓 `struct RenewRequest`

_(no doc comment)_


### 🔓 `struct RenewResponse`

_(no doc comment)_


