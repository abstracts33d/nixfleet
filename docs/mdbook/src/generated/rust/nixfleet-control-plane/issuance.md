# `nixfleet_control_plane::issuance`

Cert issuance for `/v1/enroll` and `/v1/agent/renew`.

Validates the CSR + token, builds a TBS certificate with the
standard agent-cert profile (clientAuth EKU, SAN dNSName), and
signs with the fleet CA's private key. **The fleet CA private
key is read at issuance time from a path on disk — issue #41
tracks moving it to TPM-bound signing.**

Audit log: every issuance writes one JSON line to journal AND
appends to a configured audit-log file. The file is plaintext
JSON-lines (one record per line) so an operator can `tail -f`
it during incidents.

## Items

### 🔓 `const AGENT_CERT_VALIDITY`

30 days — D6 default. Agent self-paces renewal at 50% via
`/v1/agent/renew`.


### 🔓 `enum AuditContext`

Audit context attached to every issuance record. Distinguishes
/enroll from /renew in the audit log so operators can grep
post-incident.


### 🔓 `fn token_seen`

In-memory replay set for bootstrap-token nonces. PR-5 wraps this
in `Arc<RwLock<HashSet<String>>>` inside AppState.


### 🔓 `fn verify_token_signature`

Verify a bootstrap token's signature against the org root key.
Caller is responsible for: nonce-replay check, hostname match,
expected-pubkey-fingerprint match, expiry check. This function
only validates the cryptographic signature.


### 🔓 `fn validate_token_claims`

Validate the typed parts of a token's claims (expiry, hostname-vs-CN,
pubkey fingerprint). Pure function — caller has already verified
the signature and replay status.


### 🔓 `fn fingerprint`

SHA-256 of an SPKI DER (or any pubkey byte representation), base64-
encoded. Caller decides what bytes to feed in — we just hash.


### 🔓 `fn issue_cert`

Issue a signed agent certificate.

The CSR is parsed; the new cert inherits the CSR's subject DN
+ pubkey, gets a clientAuth EKU, a SAN dNSName matching the CN
(rustls/webpki rejects CN-only certs), and the configured
validity. Signed with the fleet CA private key loaded from
`ca_key_path`.

Caller is expected to have validated the CSR's CN already; this
function does not double-check.


### 🔓 `fn audit_log`

Append one JSON line to the audit log file. Best-effort —
failure to write the audit log warns but does not fail the
issuance (the journal still has a tracing record).


