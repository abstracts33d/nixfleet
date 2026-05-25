# Fleet-evidence schema (v1)

The canonical wire format produced by `nixfleet evidence collect` and
consumed by the M2 renderer + the auditor's offline verifier.
Schema-versioned independently of the per-host `evidence.json` schema
(which lives in `nixfleet-compliance` at its own v1).

The full record is JCS-canonical (RFC 8785) on serialization: keys
sorted, no whitespace, byte-reproducible re-runs.

## Top level

```json
{
  "schemaVersion": 1,
  "fleet": { ... },
  "hosts": [ ... ],
  "summary": { ... }
}
```

| Field | Type | Meaning |
|---|---|---|
| `schemaVersion` | u32 | Schema version. v1 at first ship. Bumped only for breaking changes; additive fields do not bump. |
| `fleet` | object | Operator-side metadata for this run. |
| `hosts` | array | Per-host records in ASCII-ascending hostname order. |
| `summary` | object | Pre-computed counts. Deterministic function of `hosts`. |

## `fleet`

```json
{
  "name": null,
  "collectedAt": "2026-05-22T12:00:00Z",
  "operatorIdentity": "alice"
}
```

| Field | Type | Meaning |
|---|---|---|
| `name` | string \| null | Optional fleet identifier the operator passes via `--fleet-name`. Free-form. |
| `collectedAt` | RFC 3339 UTC | Operator-side wall-clock at the start of the collect run. Single value for the whole record. |
| `operatorIdentity` | string \| null | Optional human label (operator's username by default). The aggregator signs nothing; this is metadata, not a credential. |

## `hosts[]`

```json
{
  "hostname": "web-01",
  "fetch": { ... },
  "evidence": { ... },
  "signature": { ... },
  "collectors": [ ... ]
}
```

Each entry is one host. Order is ASCII-ascending by `hostname` (re-run determinism anchor).

### `fetch`

```json
{
  "source": "ssh",
  "fetchedAt": "2026-05-22T12:00:00Z",
  "ok": true,
  "error": null
}
```

| Field | Type | Meaning |
|---|---|---|
| `source` | enum | One of `"ssh"`, `"agent-relay"` (future), `"local"` (test fixture). |
| `fetchedAt` | RFC 3339 UTC | Per-host fetch timestamp. |
| `ok` | bool | `true` ⇔ all three required files (`evidence.json`, `evidence.json.sig`, `evidence.host.pub`) fetched. |
| `error` | string \| null | Tail of stderr when `ok = false`. |

### `evidence`

The verbatim parsed per-host `evidence.json` (JSON value), or `null` when the host failed to fetch or the bytes did not parse as JSON.

### `signature`

```json
{
  "present": true,
  "valid": true,
  "publicKey": "<base64 32-byte ed25519 pubkey>",
  "algorithm": "ed25519",
  "pubkeyMatchesDeclared": "match",
  "verifiedAt": "2026-05-22T12:00:00Z",
  "error": null
}
```

| Field | Type | Meaning |
|---|---|---|
| `present` | bool | `true` ⇔ `evidence.json.sig` was fetched. |
| `valid` | bool | `true` ⇔ ed25519 verification of the signature against the JCS-canonical evidence bytes passed. |
| `publicKey` | string \| null | Raw 32-byte ed25519 pubkey (base64, standard alphabet). The auditor re-verifies offline against this — no OpenSSH re-parsing required. |
| `algorithm` | string | Always `"ed25519"` at v1. Explicit so a future algorithm transition is not silent. |
| `pubkeyMatchesDeclared` | enum | Cross-check outcome between the fetched `evidence.host.pub` and the pubkey declared in `fleet.resolved.json` for this host. One of `"match"`, `"mismatch"`, `"declared-absent"`, `"fetched-absent"`. `"mismatch"` is the MitM / unrecorded-rotation signal. |
| `verifiedAt` | RFC 3339 UTC | When verification ran. |
| `error` | string \| null | Reason when `valid = false` or when parse / fetch failed in a way the wrapper records here. |

### `collectors[]`

```json
[
  { "collectorId": "nix-derivation", "data": { "closureHash": "sha256-abc...", "channel": "stable", "platform": "x86_64-linux" } },
  { "collectorId": "facter",         "data": { "available": true, "raw": { ... } } }
]
```

Each entry is one `EvidenceCollector` adapter's output for this host. Order matches the operator-side configured collector list (currently fixed: `nix-derivation`, `facter`).

| Field | Type | Meaning |
|---|---|---|
| `collectorId` | string | Stable adapter identifier (e.g., `"nix-derivation"`, `"facter"`). Auditor sees this verbatim; renderer keys off it. |
| `data` | any JSON | Adapter-specific payload. Schema is per-adapter. See [evidence-collector-trait.md](evidence-collector-trait.md). |

## `summary`

Deterministic function of `hosts`. Including it canonically means downstream consumers do not recompute.

```json
{
  "hostsTotal": 3,
  "hostsBySignatureStatus": { "valid": 2, "invalid": 0, "missing": 0, "unverifiable": 1 },
  "hostsByPubkeyMatch": { "match": 2, "mismatch": 0, "declared-absent": 0, "fetched-absent": 1 },
  "controlsByStatus": { "passed": 14, "failed": 1, "unknown": 0 },
  "frameworkCoverage": {
    "NIS2": { "controlsTracked": 11, "controlsPassed": 11 },
    "DORA": { "controlsTracked": 4, "controlsPassed": 4 },
    "ISO27001": { "controlsTracked": 6, "controlsPassed": 6 },
    "ANSSI-BP-028": { "controlsTracked": 8, "controlsPassed": 7 }
  }
}
```

### `hostsBySignatureStatus`

| Bucket | Definition |
|---|---|
| `valid` | `fetch.ok && signature.present && signature.valid` |
| `invalid` | `fetch.ok && signature.present && !signature.valid` |
| `missing` | `fetch.ok && !signature.present` (host did not ship a signature) |
| `unverifiable` | `!fetch.ok` (upstream fetch failure prevented verification) |

### `hostsByPubkeyMatch`

Tally of `signature.pubkeyMatchesDeclared` outcomes across the fleet. `mismatch > 0` is the MitM / unrecorded-rotation signal.

### `controlsByStatus`

Tally across every control in every host's `evidence.controls[]`:

| Bucket | Definition |
|---|---|
| `passed` | control's `passed: true` |
| `failed` | control's `passed: false` |
| `unknown` | control has no `passed` field |

### `frameworkCoverage`

Per-framework tally. Articles in each control's `frameworkArticles[]` route to a bucket by prefix-before-dash (`NIS2-21(d)` → NIS2; `ANSSI-BP-028-R8` → ANSSI-BP-028). Unknown prefixes ignored.

| Bucket | Definition |
|---|---|
| `controlsTracked` | Number of controls with at least one article in this framework. Incremented once per article (a control with two NIS2 articles increments NIS2 by two). |
| `controlsPassed` | Subset of `controlsTracked` with `passed: true`. |

## Trust posture

The wrapper is untrusted stitching. The aggregator signs nothing. The
trust chain is per-host: each `evidence.json` is signed by the host's
ed25519 SSH key, and that signature lives verbatim inside the wrapper.

The auditor re-verifies each per-host signature independently using
`nixfleet-compliance-verify`. Any wrapper tampering either breaks one
of the embedded per-host signatures or leaves them intact but
changes the `summary` block (which the auditor recomputes from the
canonical `hosts` array).

Operator-side timestamps (`fleet.collectedAt`, `hosts[].fetch.fetchedAt`,
`hosts[].signature.verifiedAt`) are not signed and not authoritative
across the trust boundary. Treat them as wall-clock observation
records.

## Versioning

Schema v1 is forward-compatible: adding optional fields does not bump.
Removing or renaming fields, or changing semantics of existing fields,
requires `schemaVersion: 2`. Renderers (M2) read `schemaVersion` and
either render or fail-fast with a clear message.
