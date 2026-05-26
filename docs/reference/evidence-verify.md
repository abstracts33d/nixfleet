# `nixfleet evidence verify`

Defensive re-verification of a `fleet-evidence.json` record. Replays per-host signatures from the embedded bytes, recomputes the summary block, and re-checks schema invariants. Reports each layer independently and exits non-zero on any failure.

## Quick start

```sh
nixfleet evidence verify --record ./fleet-evidence-2026-05-26T10-00-00Z.json
```

Exit code `0` ⇔ every check passed.

## Flags

| Flag | Default | Meaning |
|---|---|---|
| `--record <PATH>` | required | Path to a `fleet-evidence.json` record (produced by `nixfleet evidence collect`). |

## What it checks

| Layer | What | Failure means |
|---|---|---|
| Per-host signature replay | For each host with `signature.signatureBytes` populated: JCS-recanonicalise `evidence`, decode `signatureBytes` + `publicKey`, run ed25519 verify. | The wrapper's `valid: true` claim is contradicted by the embedded bytes - either the wrapper was tampered with after collect wrote it, or there's a serialiser/deserialiser bug. |
| Summary recomputation | Recompute `summary` from `hosts` array; compare against stored block. | The summary block was post-hoc tampered with even though the per-host signatures still verify. This is the attacker's only remaining wrapper-tamper avenue once signatures replay. |
| Schema sanity | `schemaVersion == 1` and `hosts` sorted ASCII-ascending by hostname (assembler invariant). | The record wasn't produced by `nixfleet evidence collect` (or by a future incompatible version). |

## Reading the output

```
evidence verify: loaded 5 host(s) from /tmp/fleet-evidence.json
  aether                          sig=SKIPPED        summary=ok       fetch did not produce a signature at collect time
  krach                           sig=valid          summary=ok       -
  lab                             sig=valid          summary=ok       -
  ohm                             sig=valid          summary=ok       -
  pixel                           sig=SKIPPED        summary=ok       fetch did not produce a signature at collect time
evidence verify: 3/5 signatures re-verified, 0 invalid, 2 skipped; summary recompute MATCH; schema ok
```

| Label | Meaning |
|---|---|
| `sig=valid` | ed25519 replay against embedded bytes succeeded. |
| `sig=INVALID` | replay failed - wrapper's `valid: true` does not hold. Trailing detail carries the specific reason. |
| `sig=SKIPPED` | nothing to replay. Two paths: collect-time fetch never produced a signature (e.g., SSH refused), or the wrapper was produced before the signature-bytes lift (`signatureBytes = null`). Trailing detail distinguishes them. |
| `summary=ok` | recomputed summary matches the stored block. |
| `schema ok` / `schema INVARIANT-VIOLATION` | schemaVersion + sort-order check. |

## Pre-lift records (`signatureBytes: null`)

Wrappers produced before the signature-bytes schema field was added carry `signatureBytes: null` for every host. Verify reports those hosts as `SKIPPED` with an explanatory note rather than failing the run - the wrapper is still trustworthy at the summary-block level, just not replayable per-host without re-collecting.

To upgrade: re-run `nixfleet evidence collect` against the same fleet; the new wrapper will populate `signatureBytes` and verify against it cleanly.

## Trust posture preserved

`verify` recomputes from inputs the wrapper already carries. It does not contact the fleet, does not synthesise a new signature, and signs nothing. The auditor can replay the same logic offline using any ed25519 + JCS-capable toolchain (`nixfleet-compliance-verify` ships one).

## Exit codes

| Code | Meaning |
|---|---|
| 0 | All three layers passed for every host (skipped hosts do not contribute to the failure tally). |
| Non-zero | At least one per-host replay returned INVALID, or the summary recomputation mismatched, or a schema invariant is violated. The error message names which. |

## See also

- [evidence-collect.md](./evidence-collect.md) — produces the record this command verifies
- [docs/design/fleet-evidence-schema.md](../design/fleet-evidence-schema.md) — per-field reference for the record itself
