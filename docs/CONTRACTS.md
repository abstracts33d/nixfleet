# Boundary contracts

The single authoritative reference for every artifact, key, and format that crosses a layer boundary during v0.2. If it is not listed here, it is not a contract — it is implementation detail that can change without coordination.

Every entry declares:
- **Producer** — the layer/component that emits the artifact.
- **Consumer(s)** — layers/components that read it.
- **Schema/version** — current version and the discipline for evolving it.
- **Verification** — what a consumer must check before trusting the content.

Boundaries cross between three layers:
- **CI / infra** — M70q coordinator, out of tree; lives in `fleet` repo.
- **Nix declarative** — this repo's `lib/`, `modules/`, + `nixfleet-compliance`.
- **Rust runtime** — this repo's `crates/` (agent + control plane).

---

## I. Data contracts

### 1. `fleet.resolved.json`

| | |
|---|---|
| **Producer** | CI (lab CI invokes the Nix layer's eval) |
| **Consumer** | Control plane, agents (fallback direct fetch) |
| **Schema** | v1 — shape defined in RFC-0001 §4.1 |
| **Canonicalization** | JCS (RFC 8785), see §IV |
| **Signature** | CI release key (see §II #1) |
| **Metadata** | `meta.signedAt` (RFC 3339), `meta.ciCommit`, `meta.schemaVersion`, `meta.signatureAlgorithm` (`"ed25519"` \| `"ecdsa-p256"`; optional, defaults to `"ed25519"`) |

**Evolution discipline.** Within v1, fields may be added; consumers MUST ignore unknown fields. Removing or changing the meaning of a field requires `schemaVersion: 2` and a migration window. `meta.signatureAlgorithm` was added after the initial `schemaVersion: 1` draft — artifacts without the field MUST be interpreted as `"ed25519"` for backward compatibility.

**Consumer MUST verify before use:**
1. JCS bytes match the canonicalized payload.
2. `meta.signatureAlgorithm` (default `"ed25519"`) matches the algorithm of the pinned `nixfleet.trust.ciReleaseKey`.
3. Signature verifies against the pinned `nixfleet.trust.ciReleaseKey` using the declared algorithm.
4. `(now − meta.signedAt) ≤ channel.freshnessWindow` (units: minutes; see RFC-0001 §4.1).
5. `meta.schemaVersion` is within the consumer's accepted range.

**Producer pipeline (`nixfleet-release`).** The framework ships one orchestrator binary that produces this artifact: build host closures → inject `closureHash = basename(toplevel)` → stamp `meta.{signedAt, ciCommit, signatureAlgorithm}` → canonicalize via `nixfleet_canonicalize` → invoke a sign hook → write `releases/fleet.resolved.json{,.sig}`. The orchestration is a contract; the cache-push and signing tools it shells out to are not.

**Producer hook contract (binding):**
- `--push-cmd` (optional) is invoked once per built closure with `cwd` = invocation cwd and these env vars set: `NIXFLEET_HOST` (host name), `NIXFLEET_PATH` (absolute store path), `NIXFLEET_CLOSURE_HASH` (basename of the path). Non-zero exit aborts the run.
- `--sign-cmd` (required) is invoked once with `NIXFLEET_INPUT` (path to a tempfile containing the canonical bytes) and `NIXFLEET_OUTPUT` (path the hook MUST write the raw signature bytes to). Non-zero exit, missing output file, or 0-byte output aborts the run.

These env-var names are part of the contract — renaming them is a §VIII amendment. The shell command strings themselves and any tools they shell out to (attic, nix copy, tpm-sign, cosign, GPG, ssh-keygen -Y, …) are operator-supplied and not framework concerns.

### 2. Wire protocol (agent ↔ control plane)

| | |
|---|---|
| **Producer/Consumer** | Both agent and CP (Rust runtime) |
| **Schema** | v1 — RFC-0003 §4 |
| **Transport** | HTTP/2 over TLS 1.3, mTLS mandatory |
| **Version header** | `X-Nixfleet-Protocol: 1` |

**Evolution discipline.** Major version in header; mismatched major = HTTP 400. Additive fields within a major; MUST-ignore-unknown-fields on both sides. Removing a field requires a major bump and dual-version CP support during migration.

### 3. Probe descriptor

| | |
|---|---|
| **Producer** | `nixfleet-compliance` (Nix layer) |
| **Consumer** | Agent (Rust runtime) at runtime |
| **Schema** | Per-control `schema = "<framework>/<version>"` field (e.g. `"anssi-bp028/v1"`) |
| **Payload** | `{ command, args, timeoutSecs, expect, schema }` |

**Evolution discipline.** Each framework+version pair is immutable once shipped. New version = new schema string (`anssi-bp028/v2`); agent ships a handler registry keyed on `(control, schema)`. Controls MAY support multiple schema versions during migration.

### 4. Probe output

| | |
|---|---|
| **Producer** | Agent (executing the probe command) |
| **Consumer** | CP (aggregation), auditor (verification) |
| **Schema** | Declared by the control (§I.3 above) |
| **Canonicalization** | JCS |
| **Signature** | Host SSH ed25519 (see §II #4) |

**Evolution discipline.** Output shape is part of the control declaration — changes go through the control schema version. Signature covers the canonicalized bytes plus `{ control, schema, hostname, bootId, generationHash, ts }`.

### 5. Secret recipient list

| | |
|---|---|
| **Producer** | `fleet.nix` (Nix layer) |
| **Consumer** | agenix encryption tooling at commit time; agent at activation |
| **Schema** | agenix-native, pinned by `flake.lock` |

**Evolution discipline.** Pinned to the `agenix` version in `flake.lock`. Upgrading agenix is a coordinated commit that re-encrypts all secrets; treat as a spine-level change, not a routine dependency bump.

### 6. Log / event schema

| | |
|---|---|
| **Producer** | CP (reconciler), agent |
| **Consumer** | Operator queries, auditors reading historical state |
| **Schema** | RFC-0002 §7 — structured event with `logSchemaVersion` field |

**Evolution discipline.** Same as wire protocol — additive within a major, bump on breaking changes. Historical events MUST remain parseable for the declared audit retention window.

---

## II. Trust roots

Four keys. Everything else is derived. For each: **who holds the private key, where the public key is declared, and who verifies.**

### 1. CI release key

| | |
|---|---|
| **Private** | HSM / TPM-backed keyslot on M70q (operator infra) |
| **Public (declared)** | `nixfleet.trust.ciReleaseKey` in `fleet.nix` (Nix layer) |
| **Verified by** | CP (on `fleet.resolved` load), optionally agents |
| **Algorithm** | `ed25519` **or** `ecdsa-p256` — declared alongside the public key; the signature's algorithm (§I #1 `meta.signatureAlgorithm`) must match |
| **Rotation grace** | `nixfleet.trust.ciReleaseKey.previous` valid for 30 days after rotation |

**Algorithm rationale.** ed25519 is the preferred default for HSMs, YubiKeys, cloud KMS, and software-held keys. ECDSA P-256 exists as a second-class citizen because commodity TPM2 hardware (Intel PTT, AMD fTPM, most discrete TPMs) exposes RSA + NIST P-256 but not the ed25519 curve (TPM2_ECC_CURVE_ED25519 = 0x0040 is rare). Both algorithms produce 64-byte signatures and have comparable security margins (~128-bit). Producers (lab CI) pick one at install time based on hardware; the trust-root declaration tells consumers which verifier to use.

**Public-key encoding.**
- `ed25519` — raw 32-byte public key, base64-encoded in `fleet.nix` (matches the format used by `ssh-keygen`, agenix, minisign).
- `ecdsa-p256` — uncompressed point, 64 bytes (`X ‖ Y`, no `0x04` prefix), base64-encoded. Consumers convert to SEC1 / DER SPKI at verify time.

The declaration shape:

```nix
nixfleet.trust.ciReleaseKey = {
  algorithm = "ecdsa-p256";  # or "ed25519"
  public    = "<base64 of raw bytes>";
};
```

**Signature encoding.** Raw 64 bytes for both algorithms — `R ‖ S` for ECDSA, standard `R ‖ S` for ed25519. No DER wrapping, no PGP armour. Put next to the canonical payload as `fleet.resolved.json.sig`.

**Rotation procedure.**
1. Generate new keypair (operator infra) — may differ in algorithm from the outgoing one.
2. Commit: set `ciReleaseKey = <new>`, `ciReleaseKey.previous = <old>` in `fleet.nix`. Consumers that pin both must accept signatures under either algorithm during the overlap.
3. CI starts signing with new key on next build.
4. After 30 days, remove `previous` from `fleet.nix`; old-key-signed artifacts rejected.

**Compromise response.** Immediate: remove compromised key from `fleet.nix`, set `rejectBefore = <timestamp>` (all artifacts signed before that are refused regardless of key). Rebuild CI environment. Sign a fresh fleet.resolved from known-clean CI. Document in `SECURITY.md`.

### 2. Cache trust keys

| | |
|---|---|
| **Private** | Each cache implementation's own keystore (harmonia signing key file, attic signing key, cachix authtoken-derived, etc.) |
| **Public (declared)** | `nixfleet.trust.cacheKeys` (Nix layer) — flat list of opaque strings |
| **Verified by** | nix's substituter (via `nix.settings.trusted-public-keys`) before every closure activation |
| **Format** | Implementation-defined string. Stock `<name>:<base64>` (harmonia, nix-serve, cachix) and attic's `attic:<host>:<base64>` are both accepted by nix and may be mixed in one list. |
| **Rotation grace** | Add the new key alongside the old in the list; remove the old once all hosts have switched. |

**Framework agnosticism.** The framework forwards these strings opaquely — it does not parse, dispatch on, or otherwise discriminate between cache implementations. Choosing harmonia, attic, cachix, plain `nix-serve`, or a custom HTTP cache is a fleet-side decision; the framework's only requirement is that the chosen impl serves the standard nix-cache HTTP protocol so that `services.nixfleet-cache.cacheUrl` works.

### 3. Org root key

| | |
|---|---|
| **Private** | Offline hardware (Yubikey) held by operator |
| **Public (declared)** | `nixfleet.trust.orgRootKey` (Nix layer) |
| **Verified by** | CP, when validating enrollment tokens |
| **Algorithm** | ed25519 |
| **Rotation grace** | 90 days; effectively never under normal operation |

**Rotation procedure.** Rare. If it rotates, every bootstrap token generated from the old key becomes invalid — every host re-enrollment requires a new token signed by the new key. Not a routine event.

**Compromise response.** Catastrophic: every enrollment token is potentially forgeable. Revoke old key, issue all hosts new bootstrap tokens, re-enroll fleet. Consider this the equivalent of an "infrastructure rebuild" event.

### 4. Host SSH key

| | |
|---|---|
| **Private** | Per-host `/etc/ssh/ssh_host_ed25519_key` (generated at provision) |
| **Public (declared)** | `fleet.nix` host entry (`hosts.<n>.pubkey`) (Nix layer) |
| **Verified by** | Auditor (probe output signatures), CP (mTLS cert binding at enrollment) |
| **Algorithm** | ed25519 (OpenSSH-compatible) |
| **Rotation grace** | Host key change = re-enrollment; no grace |

**Rotation procedure.** If a host's key changes, the old host is considered gone and a new one is being enrolled. Secrets must be re-encrypted for the new recipient; probe-output signatures chain through the boot/generation record.

---

## III. Canonicalization

**JCS (RFC 8785) with a single Rust implementation, byte-identical across all signers and verifiers.**

Producer-side (the Nix layer's `lib/mk-fleet.nix`) MUST emit values that round-trip through JCS losslessly: ints only (no floats), deterministic attr order, no JSON-incompatible types. Consumer-side (the Rust runtime's `bin/nixfleet-canonicalize`) pins the library.

- **Library choice.** Pinned to [`serde_jcs`](https://crates.io/crates/serde_jcs) `0.2`, hosted by `crates/nixfleet-canonicalize`. Rationale: direct RFC 8785 implementation over `serde_json::Value`; handles UTF-16 key sorting and ECMAScript number formatting per spec. Any change to this pin is a contract change (§VII) requiring signoff from every layer that signs or verifies artifacts (CI/infra, Nix, Rust).
- **Golden-file test.** `crates/nixfleet-canonicalize/tests/fixtures/jcs-golden.{json,canonical}` with byte-exact equality asserted in `tests/jcs_golden.rs`. Runs on every push via pre-push `cargo nextest run --workspace`; fails loudly on any drift. The ed25519-signed-bytes extension of this fixture lands alongside the CI release key.
- **Usage.** Every signed artifact (fleet.resolved, probe output) is canonicalized via this single library before signing and before verification. No ad-hoc serializers in Nix, shell, or other crates.

When the Nix layer needs to produce a JCS-canonical artifact (e.g. CI signing fleet.resolved), it invokes the same Rust canonicalizer via a small shell tool (`nixfleet-canonicalize`). Do not reimplement in Nix or shell.

---

## IV. Control-plane storage purity rule

The control plane's SQLite database exists to cache operational state. Every column MUST satisfy one of:

1. **Derivable from git + agent check-ins.** Documented in a line comment on the column:
   ```sql
   CREATE TABLE hosts (
     hostname TEXT PRIMARY KEY,          -- derivable from: fleet.resolved
     current_gen TEXT,                   -- derivable from: agent check-in
     last_seen_at DATETIME,              -- derivable from: agent check-in
     ...
   );
   ```
2. **Explicitly listed in "accepted data loss."** See below.

**Accepted data loss list** — state that is intentionally not preserved through a control-plane teardown:

| State | Reason | Recovery |
|---|---|---|
| Certificate revocation history | Revocations are operational decisions, not automated. | Operator re-declares revocations after teardown. |
| Per-rollout event log (> 30 days old) | Historical trace, not operational. | Available via log aggregation (§I.6), not CP-internal. |

**Rule.** A new column that is neither derivable nor on the accepted-loss list is a contract violation. It fails the teardown test (`#14`) and must be either removed or moved into the declarative state.

---

## V. Versioning summary

| Contract | Current version | Evolution |
|---|---|---|
| `fleet.resolved.json` | `schemaVersion: 1` | Additive within v1; bump for breaking changes. `meta.signatureAlgorithm` added in v1 — optional, defaults to `"ed25519"` when absent. |
| Wire protocol | v1 (header) | Additive within major; dual-support during migration |
| Probe descriptor per framework | `<framework>/v1` per framework | New string for new shape; old shape kept during migration |
| Probe output | Tracked with the control | Same as descriptor |
| Log/event | `logSchemaVersion: 1` | Same pattern as wire protocol |
| Agenix format | Pinned by `flake.lock` | Treat upgrade as spine change |

---

## VI. Implementation agnosticism

The framework promises *mechanism*, not *implementation*. The following are explicit non-commitments — the framework runtime contains no code that depends on these choices, and a fleet may freely substitute any conforming alternative without forking nixfleet.

| Concern | Framework requires | Fleet picks |
|---|---|---|
| **GitOps source** for the channel-refs poll | An HTTPS URL pair (artifact + signature) that returns the raw signed bytes when GET'd, optionally with `Authorization: Bearer <token>`. Configured via `services.nixfleet-control-plane.channelRefsSource.{artifactUrl, signatureUrl, tokenFile}`. | Forgejo / Gitea / GitHub / GitLab / sourcehut / plain HTTPS / S3 with presigned URLs / anything HTTP-shaped. URL templates for common forges live in `nixfleet-scopes.scopes.gitops.*` as pure data — adding a new forge is one `.nix` file, no Rust changes. |
| **Binary cache server** | Nothing — the framework does not ship a cache-server module. Hosts that should serve a cache import a scope. | `nixfleet-scopes.nixosModules.harmonia`, `nixfleet-scopes.nixosModules.attic-server`, plain `services.nix-serve`, cachix as a service, or a hand-rolled wrapper. |
| **Binary cache client** | An HTTPS URL + a public key string. Configured via `services.nixfleet-cache.{cacheUrl, publicKey}`. | Any cache speaking the standard nix-cache HTTP protocol (narinfo + nar). Identical client config regardless of which server impl is upstream. |
| **Cache trust keys** | A flat list of opaque strings forwarded to `nix.settings.trusted-public-keys`. Configured via `nixfleet.trust.cacheKeys`. | Stock `<name>:<base64>`, attic `attic:<host>:<base64>`, or both at once — see §II #2. |
| **PKI / mTLS issuer** | Cert + key file paths on disk. The framework reads them; their provenance is not a contract. | Caddy's internal CA (current fleet choice), Smallstep, vault-pki, hand-rolled scripts, or a public CA — anything that produces RSA / ECDSA / Ed25519 cert files compatible with rustls. |
| **Secrets backend** | Cert / key / token *paths* in option fields. The framework reads files; how they got there is not a contract. | agenix (current fleet choice), sops-nix, plain nixops, manual secret-staging scripts, or systemd-creds. |
| **Disk layout** | A `disko.devices` attrset on the host. | `nixfleet`'s bundled disk-templates for common cases, hand-rolled disko config, or none if filesystems are pre-provisioned. |
| **Impermanence** | An `environment.persistence` option must exist (the framework's own service modules contribute to it). The framework imports the upstream `impermanence` flake to satisfy this. | Activate via `nixfleet.impermanence.enable = true`, or leave disabled — the schema is always declared. |

**What this means for fleets.** Every framework binary or NixOS module touches only the contract surface above. A fleet that wants GitHub instead of Forgejo, harmonia instead of attic, sops-nix instead of agenix, or vault-pki instead of Caddy CA changes its scope imports and its option values — the framework code is rebuilt without modification.

**What this means for nixfleet maintainers.** New tech-specific impls land as scopes (in `nixfleet-scopes` or out-of-tree), not as framework features. If something tech-specific *must* enter the framework — e.g. a new wire-protocol participant — it's a contract change governed by §VII below.

### Irreducible technology assumptions

A small set of technology choices are **load-bearing** for the framework — they're not implementation choices a fleet can swap. Replacing one of these means building a different framework.

| Assumption | Why load-bearing | Replacing means |
|---|---|---|
| **Nix + flakes** | The whole declarative side (mkHost, mkFleet, the option system, hostSpec contract, fleet.resolved evaluation) is built on Nix evaluator semantics; the framework has no non-Nix front-end. | Re-implementing the declarative layer in another DSL — different framework. |
| **NixOS** (system layer) | The Linux agent's activation pipeline assumes NixOS' generation model: `/run/current-system` resolves to the active toplevel, `nixos-rebuild switch --system <path>` is the activation primitive, post-switch verification reads `basename(realpath /run/current-system)`. The §I #1 contract refers to "closure hash"; that concept is meaningful in NixOS terms. | A separate activation backend abstraction — see roadmap. Until that lands, non-NixOS Linux is out of scope. |
| **systemd** | Every framework NixOS module declares `systemd.services.nixfleet-*`. Hardening, restart policy, credential plumbing, dependency ordering all use systemd primitives. | Rewriting the system-service layer for runit/s6/launchd — same scope as a non-NixOS port. |
| **mTLS over HTTP/1.1** | Agent ↔ control-plane authentication identity is the client cert CN; authorisation is per-route. The CP's rustls config is the trust boundary the agent verifies; replacing TLS means a different wire protocol. | A different wire protocol (Noise, Tailscale ACL, mutual auth over WireGuard). Different framework. |

**TPM is *not* on this list.** TPM hardware is a *fleet's choice* of signing keyslot, not a framework requirement. The `tpm-keyslot` scope lives in `nixfleet-scopes`; the framework runtime never links a TPM library. A fleet using a YubiKey, software key, HSM, or KMS for the CI release key is fully framework-supported — see §I #1's hook contract. The current fleet happens to use TPM-backed ECDSA P-256; that's deployment opinion.

**Why call these out.** Phases 1–9 of the agnosticism work made it easy to add new tech-specific impls as scopes. The four assumptions above cannot be captured by the same pattern — there is no scope a fleet can import to replace systemd. Documenting them here prevents the framework from drifting into pretending they're substitutable, and gives future maintainers a clear test: *if it's listed below the agnosticism table, scope-side; if it's listed in this irreducible-assumptions table, framework-side and out of scope to abstract.*

---

## VII. Non-contracts (explicit)

The following are NOT contracts — they may change without coordination:

- Internal CP SQLite layout (as long as §IV rule holds).
- Internal agent process structure (threads, tokio tasks).
- Internal reconciler intermediate data structures.
- Nix module option defaults (overridable per-host).
- Formatter choices, lint rules.
- Directory layout inside `crates/` beyond crate names.

If something that should be a contract is drifting, propose it as an addition to this document via PR — do not unilaterally stabilize it in code.

> **Implementation status disclosure.** Some contracts in §I — notably parts of `CheckinResponse.target` (RFC-0003 §4.1) and the soak-timer / rollback-and-halt semantics in the reconciler (RFC-0002 §3.1, §5.1) — are **schema-honored but behavior-partial**. The framework declares the wire shape and the option surface, but specific code paths are deferred. See [`docs/roadmap/0002-v0.2-completeness-gaps.md`](roadmap/0002-v0.2-completeness-gaps.md) for the full audit and remediation cost estimates. This disclosure is not a contract weakening — the listed contracts remain authoritative and additive — but it makes explicit that "passes verification" does not yet mean "exercises every documented field."

---

## VIII. Amendment procedure

1. Open a PR that modifies this document.
2. Label it `contract-change`.
3. Review requires a signoff from each layer whose code implements the contract.
4. Merge only after the code change that implements the new contract is ready in the same PR (or a linked follow-up that must land within the same spine milestone).
