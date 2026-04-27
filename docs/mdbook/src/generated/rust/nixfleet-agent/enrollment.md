# `nixfleet_agent::enrollment`

Bootstrap enrollment + cert renewal client (Phase 3 PR-5).

- On first boot, when the agent's `--client-cert` / `--client-key`
  files don't exist, it reads `--bootstrap-token-file`, generates
  a fresh keypair + CSR, POSTs `/v1/enroll`, writes the issued
  cert + private key atomically.
- During the regular poll loop, when the existing cert has < 50%
  remaining validity, the agent generates a fresh keypair + CSR,
  POSTs `/v1/agent/renew` over the current valid mTLS, writes
  the new cert + key atomically.

## Items

### 🔓 `fn generate_csr`

Generate a fresh keypair + a CSR with `CN=hostname`. Returns the
(PEM CSR, PEM key, raw pubkey bytes for fingerprinting).


### 🔓 `fn fingerprint_pubkey_der`

SHA-256 fingerprint (base64) of pubkey DER bytes — matches the CP's
`expected_pubkey_fingerprint` shape in the bootstrap token.


### 🔓 `fn enroll`

First-boot enrollment. Reads token file, generates CSR, POSTs
`/v1/enroll`, writes the cert + key atomically to the configured
paths.


### 🔓 `fn renew`

Renew the existing cert. Generates a fresh keypair + CSR, POSTs
`/v1/agent/renew` over the current authenticated mTLS connection
(caller wires the existing client identity into `client`), writes
the new cert + key atomically.


### 🔒 `fn write_atomic`

Atomic write: write to a sibling tempfile then rename, so a crash
mid-write doesn't leave a half-written cert at the canonical path.


### 🔓 `fn cert_remaining_fraction`

Read an existing cert PEM and decide whether it needs renewal.
Returns `(remaining_fraction, not_after)` where
`remaining_fraction < 0.5` means time to renew.


### 🔒 `mod pem`

Lightweight PEM parser fallback so we don't pull a full pem crate.


