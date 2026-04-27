# `modules/_trust.nix`

modules/_trust.nix

nixfleet.trust.* — the four trust roots from docs/CONTRACTS.md §II.
Public keys are declared here; private keys live elsewhere (HSM, host
SSH key, offline Yubikey) and never enter this module.

Each root supports a `.previous` slot for the 30-day rotation grace
window and a shared `rejectBefore` timestamp for compromise response.

`ciReleaseKey` carries an explicit `algorithm` per the §II #1 amendment
in abstracts33d/nixfleet#18 (ECDSA P-256 alongside ed25519 because
commodity TPM2 hardware does not expose the ed25519 curve). The other
trust roots remain bare strings for now; their algorithms are pinned
by CONTRACTS.md §II #2 (attic-native) and §II #3 (ed25519).

## Bindings

### `publicKeyType`

Typed public-key declaration (CONTRACTS §II #1 amendment).
Consumers read `.algorithm` to pick the verifier; `.public` is the
base64-encoded raw public key bytes per the algorithm's encoding rule.

### `ciReleaseKeySlotType`

CI-release-key slot. Per CONTRACTS §II #1, the public half is typed
(algorithm + public) so consumers learn the algorithm without
out-of-band knowledge. `rejectBefore` remains a flat timestamp.

### `keySlotType`

Legacy slot type — bare-string current/previous, used by the other
two trust roots that still pin a single algorithm per CONTRACTS §II.

