# `modules/scopes/nixfleet/_operator.nix`

Operator workstation scope.

Wires the operator-side tooling into a NixOS host:
- `nixfleet-mint-token` — signs bootstrap tokens for /v1/enroll.
- `nixfleet-derive-pubkey` — derives base64 ed25519 pubkey from a
  raw private key file (one-shot, used when initialising the org
  root key).

The org root **private** key is intentionally NOT a fleet-wide
secret. It lives in fleet-secrets agenix-encrypted to the operator
user + the operator workstation's host key only — lab CP and other
fleet hosts never decrypt it. The CP only verifies token signatures
with the public half (declared in `config.nixfleet.trust.orgRootKey`).

Per the design property in `docs/CONTRACTS.md §II #3` and
nixfleet#10's "control plane holds no secrets, forges no trust",
the org root key compromise scenario is a multi-host operator-
workstation event — not a CP-side breach. Sovereignty preserved.

Auto-included by mkHost (disabled by default). Enable on the
operator's workstation only.

## Bindings

### `environment.variables`

Surface the configured key path via shell env so the operator
can run `nixfleet-mint-token` without remembering the agenix
path (or muscle-memorise an alias). When `orgRootKeyFile` is
null this stays unset.

