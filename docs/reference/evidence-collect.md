# `nixfleet evidence collect`

Operator-side aggregator. Pulls per-host signed compliance evidence
from every host declared in `fleet.resolved.json`, verifies signatures
locally, runs the configured `EvidenceCollector` adapters, and writes
a single JCS-canonical fleet-evidence record on disk.

## Quick start

```sh
nixfleet evidence collect \
  --fleet ./fleet.resolved.json \
  --out ./audit-2026-q2.json
```

Output: a single JCS-canonical JSON file plus a per-host status table
on stdout.

## Flags

| Flag | Default | Meaning |
|---|---|---|
| `--fleet <PATH>` | required | Path to `fleet.resolved.json`. Authoritative source of host names + declared per-host pubkeys. |
| `--out <PATH>` | `./fleet-evidence-<UTC>.json` | Where the record is written. JCS bytes + trailing newline. |
| `--fleet-name <STRING>` | `null` | Optional fleet identifier embedded in the record metadata. Auditor sees this in `fleet.name`. |
| `--operator-identity <STRING>` | operator's username | Free-form human label embedded in `fleet.operatorIdentity`. The aggregator does not sign anything; this is metadata, not a credential. |
| `--ssh-user <STRING>` | inherits from operator's ssh config | SSH login user. Passes through as `ssh -l <user>`. |
| `--ssh-port <PORT>` | `22` | SSH port. |
| `--continue-on-error <bool>` | `true` | When a per-host fetch fails: record the failure in the wrapper and continue with sibling hosts. Set to `false` to abort the run on first failure. |

## Trust posture (read this once)

- The wrapper is **untrusted stitching**. The aggregator signs nothing.
- The trust chain is per-host: each `evidence.json` is signed by the host's ed25519 SSH key. That signature lives verbatim inside the wrapper at `hosts[].signature`. An auditor re-verifies it independently via `nixfleet-compliance-verify`.
- Tampering with the wrapper either (a) breaks one of the embedded per-host signatures, or (b) leaves them intact but changes the wrapper's `summary` block — which the auditor recomputes deterministically from the canonical `hosts` array.
- The wrapper output is JCS-canonical (RFC 8785). Re-running with the same inputs and the same wall-clock minute produces byte-identical bytes; diffing two outputs reflects state change, not serializer state.

## Reading the per-host status table

```
  cp                              fetch=ok    sig=valid     pub=match       -
      collectors: nix-derivation, facter
  web-01                          fetch=ok    sig=valid     pub=match       -
      collectors: nix-derivation, facter
  web-02                          fetch=FAIL  sig=absent    pub=absent      ssh: connection refused
```

| Label | Meaning |
|---|---|
| `fetch=ok` / `fetch=FAIL` | SSH fetch of the three required files (`evidence.json`, `.sig`, `evidence.host.pub`) succeeded or failed. |
| `sig=valid` / `sig=INVALID` / `sig=absent` | ed25519 verification on JCS-canonical evidence bytes. Absent = no sidecar fetched. |
| `pub=match` / `pub=MISMATCH` / `pub=undeclared` / `pub=absent` | MitM cross-check between the fetched `evidence.host.pub` and the pubkey declared in `fleet.resolved.json` for the host. Mismatch = key rotated without re-declaring in fleet.resolved, OR active intercept. Either is worth investigating. |
| trailing field | Most recent error message (fetch stderr tail or verification failure reason). `-` when none. |

## Output schema

See [docs/design/fleet-evidence-schema.md](../design/fleet-evidence-schema.md) for the per-field reference. A summary, in pseudo-JSON:

```json
{
  "schemaVersion": 1,
  "fleet": { "name": null, "collectedAt": "...", "operatorIdentity": "..." },
  "hosts": [ {
    "hostname": "...",
    "fetch": { "source": "ssh", "fetchedAt": "...", "ok": true, "error": null },
    "evidence": { ...verbatim per-host evidence.json... },
    "signature": { "present": true, "valid": true, "publicKey": "...", "algorithm": "ed25519", "pubkeyMatchesDeclared": "match", "verifiedAt": "...", "error": null },
    "collectors": [ { "collectorId": "nix-derivation", "data": ... }, { "collectorId": "facter", "data": ... } ]
  } ],
  "summary": { "hostsTotal": N, "hostsBySignatureStatus": {...}, "hostsByPubkeyMatch": {...}, "controlsByStatus": {...}, "frameworkCoverage": {...} }
}
```

## Viewing the record

The output is single-line JCS-canonical JSON. Operator-friendly view:

```sh
jq < ./fleet-evidence-<UTC>.json
```

For the per-framework summary:

```sh
jq .summary.frameworkCoverage < ./fleet-evidence-<UTC>.json
```

## SSH transport

The aggregator subprocesses `ssh` for each per-host fetch. That means:

- Your `~/.ssh/config`, ssh-agent, identity file, `ProxyCommand`, jump hosts, and `ControlMaster` multiplexing all transfer to the aggregator for free.
- `BatchMode=yes` is set per call: password prompts are refused. Use ssh-agent or an unencrypted identity file with strict file permissions (you should be doing this anyway).
- `ConnectTimeout=10` is the default per host.
- Fetches run with bounded parallelism (16 hosts in flight at any time).

## Failure modes + what to do

| Symptom | Cause | Operator action |
|---|---|---|
| `ssh: connection refused` | Host unreachable. | Investigate the host. The run continues; record marks the host `fetch=FAIL` and `unverifiable` in the summary. |
| `ssh: BatchMode is on, but password is required` | No ssh-agent or no identity file. | Start ssh-agent + `ssh-add`, or pass `--ssh-user` with a working identity. |
| `sig=INVALID` on every host | Operator passed the wrong `fleet.resolved.json` (different commit than what the hosts were rolled out from). | Use the `fleet.resolved.json` from the same release as the running hosts. |
| `pub=MISMATCH` on one host | The host's SSH ed25519 key rotated since the last `fleet.resolved.json` was signed. Or active MitM. | Re-sign `fleet.resolved.json` with the new pubkey, or investigate. |
| `evidence.json parse: ...` | The per-host evidence file is not valid JSON. | Investigate the host-side probe-runner. The wrapper records the parse error in `signature.error` and `evidence: null`. |

## Exit codes

| Code | Meaning |
|---|---|
| 0 | Run completed; record written to `--out`. Any per-host failures are recorded inside the record, not in the exit code. |
| Non-zero | The aggregator itself failed (e.g., could not read `--fleet`, could not write `--out`). Per-host failures alone do not produce a non-zero exit. |

## Smoke test against `nixfleet-demo`

After `nix run .#fleet-up` in `nixfleet-demo`, drive a collect against the demo:

```sh
# In nixfleet-demo, after fleet-up:
ls fleet.resolved.json  # confirm artefact exists

# From the operator-side worktree:
nixfleet evidence collect \
  --fleet ../nixfleet-demo/fleet.resolved.json \
  --out /tmp/demo-evidence.json

jq .summary < /tmp/demo-evidence.json
```

Expected: `hostsBySignatureStatus.valid` equals the demo's host count once the hosts have run their first compliance probe. `pub=match` on all hosts (the demo's `fleet.resolved.json` declares the same keys the hosts sign with).

## Compliance framework reporting

`summary.frameworkCoverage` reports per-framework counts:

| Framework | Article prefix |
|---|---|
| NIS2 | `NIS2-` (e.g., `NIS2-21(d)`) |
| DORA | `DORA-` |
| ISO 27001 | `ISO27001-` |
| ANSSI BP-028 | `ANSSI-BP-028-` |

`controlsTracked` counts every control whose `frameworkArticles[]` contains an article in that framework. `controlsPassed` counts the subset with `passed: true`. Articles with unknown prefixes are ignored.

## See also

- [docs/design/fleet-evidence-schema.md](../design/fleet-evidence-schema.md) — per-field reference
- [docs/design/evidence-collector-trait.md](../design/evidence-collector-trait.md) — adapter authoring
