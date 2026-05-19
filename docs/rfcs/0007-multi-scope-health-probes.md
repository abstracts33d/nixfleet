# RFC-0007: Multi-scope health probes

**Status.** Accepted.
**Depends on.** RFC-0001 (fleet topology + `mkFleet` API), RFC-0002 (reconciler + signed manifests), RFC-0005 (event-driven probe state), RFC-0006 (runtime architecture).
**Supersedes.** Sections of RFC-0002 referencing `HealthGate.compliance_probes.required` and `Channel.compliance.{mode,strict}` as channel-level booleans/enums; those fields are removed in favour of per-probe `mode`. Also supersedes the `host_reports`-as-canonical-storage references in RFC-0003, RFC-0009 (attestation events), RFC-0010 (`HostUnquarantined`), and RFC-0011 (`StaleTargetRejected`). Those event kinds land in `event_log` per RFC-0005 §4.3; gate-relevant subsets land additionally in the `probe_failures` derived view per §7.2.
**Scope.** The declarative shape operators use to define health probes across fleet, tag, and host scopes; the per-probe `mode` field that replaces the channel-level enforcement flag; the relationship between probe topology and the closure-hash signing chain. Does not cover runtime execution mechanics (those live in RFC-0005 + RFC-0006) or the agent-internal probe-runner pipeline.

## 1. Problem statement

Pre-v0.2 nixfleet had three places where probe-or-probe-like declarations lived, with three different shapes, lifecycles, and owners:

| Site | Scope | Shape | Who declares it |
|---|---|---|---|
| `services.nixfleet-agent.healthChecks` (NixOS module) | per-host | `{ http, tcp, exec }` declarations | host author |
| `fleet_resolved::HealthGate.compliance_probes.required: bool` (signed manifest, channel-level) | per-channel | gate-enforcement flag | fleet author |
| `crates/nixfleet-agent/src/compliance.rs` (deleted) | per-host implicit | code-driven collector wiring | nobody, implicit |

The split has two operational consequences:

- **Compliance gating was a special case.** `compliance_probes.required` was its own concept disconnected from the regular probe machinery. Two parallel paths existed in the agent (regular health probes via `health.rs` + compliance via `compliance.rs`), with two parallel paths in the CP gate logic.
- **There is no way to declare a probe for "every host in a tag" or "every host in the fleet."** Operators copy-paste the same probe declaration into every host's NixOS module, or add it as a scope. Both work, but neither expresses the intent "this probe applies to every web-tagged host" directly.

This RFC unifies the three sites and adds explicit scope-level declarations.

## 2. Design principles

1. **Per-host operationally; multi-scope declaratively.** Probes execute on the host they target (typically against `localhost` services). Operators declare them at whichever scope expresses the intent most cleanly — fleet-wide for cross-cutting probes like heartbeat, tag-scoped for service-class probes like `nginx-version`, host-scoped only when truly host-specific.

2. **Per-probe `mode` replaces channel-level enforcement.** Each probe carries its own `mode`: `enforce` (wave-promotion gate consults the result), `observe` (results surface in `event_log` but do not gate), `disabled` (declared but not run). The channel-level `compliance_probes.required: bool` from the manifest is removed; the same expressiveness is achieved per-probe.

3. **Closure-driven, not manifest-driven.** Probe topology is rendered into each host's NixOS closure by `_agent.nix`; the closure hash is signed by CI as part of the standard manifest signing flow; the agent reads its effective probe set from disk. The signed manifest does not carry probe declarations directly — the closure hash transitively signs them. (See §5 for the full flow.)

4. **Compliance is a probe kind, not a parallel pipeline.** A `kind = "evidence"` probe reads the latest evidence file produced by `compliance-evidence-collector.service` (existing systemd unit, operator-controlled cadence). The probe is a read-only consumer; production cadence lives with the collector unit. The wave-promotion gate consults `mode == "enforce"` evidence-probe results the same way it consults any other enforce-mode probe.

5. **Resolution is deterministic and visible at fleet-eval time.** Operator-controlled precedence with explicit collision warnings. No silent shadowing.

## 3. Declaration model

### 3.1 Four scopes

```nix
{
  # Fleet-wide — applies to every host
  nixfleet.healthChecks = {
    heartbeat = {
      kind = "http";
      url = "http://localhost/health";
      intervalSeconds = 30;
      mode = "observe";
    };
    evidence-nis2 = {
      kind = "evidence";
      framework = "nis2-essential";
      intervalSeconds = 60;
      mode = "enforce";
    };
  };

  # Tag-scoped — applies to every host carrying the tag
  nixfleet.tags.web.healthChecks = {
    nginx-version = {
      kind = "http";
      url = "http://localhost/version";
      expectStatus = 200;
      intervalSeconds = 15;
      mode = "enforce";
    };
  };

  # Per-host — only this host
  nixfleet.hosts.lab.healthChecks = {
    cache-disk-space = {
      kind = "exec";
      command = "/run/current-system/sw/bin/check-disk /var/lib/attic";
      intervalSeconds = 300;
      mode = "enforce";
    };
  };
}
```

### 3.2 Probe kinds

Every probe carries `kind` (discriminator), `intervalSeconds`, `mode`, plus kind-specific fields.

| `kind` | Required fields | Optional fields |
|---|---|---|
| `http` | `url` | `expectStatus` (default 200), `bodyContains`, `timeoutSecs` (default 3) |
| `tcp` | `host`, `port` | `connectTimeoutSecs` (default 3) |
| `exec` | `command` | `expectExitCode` (default 0), `timeoutSecs` (default 10) |
| `evidence` | `framework` | `evidencePath` (default `/var/lib/nixfleet-compliance/evidence.json`) |

Validation at fleet-eval time refuses the manifest if any required field is absent, `kind` is unknown, or the same probe name appears twice at the same scope.

### 3.3 `mode` semantics

| `mode` | Agent behaviour | CP gate behaviour |
|---|---|---|
| `enforce` | Run probe at `intervalSeconds`; emit `ProbeResult` events | Wave-promotion gate consults the latest result; refuses promote on `Fail` (or `Unknown` past the grace window) |
| `observe` | Run probe at `intervalSeconds`; emit `ProbeResult` events | Records results in `event_log` for operator visibility; does NOT gate |
| `disabled` | Probe entry present in manifest but agent does not run it | Treated as absent for gate purposes |

`disabled` covers the temporary-suppression case (e.g., turning off a probe during incident response without removing it from `fleet.nix`).

### 3.4 No channel-level mode override

Per-probe `mode` is the **sole** source of truth for the gate decision. The pre-v0.2 `Channel.compliance.mode` field is removed alongside `HealthGate.compliance_probes.required` (see §6 manifest schema delta). What §3.5 below introduces is channel-scoped **declaration**, not a channel-level mode override: an operator can say "all stable-channel hosts run this evidence probe set" without per-host tagging, but each probe still carries its own `mode` wherever it is declared and the gate consults that `mode` exclusively.

All probe kinds resolve through the same multi-scope hierarchy; evidence is not a special case at any scope.

### 3.5 Channel scope

Channel-scoped declarations sit between tag and host in the resolution order. Operators declare probes attached to a specific channel so all hosts assigned to that channel pick them up:

```nix
{
  # Channel-scoped — applies to every host whose `channel` is `stable`
  nixfleet.channels.stable.healthChecks = {
    evidence-nis2 = {
      kind = "evidence";
      framework = "nis2-essential";
      intervalSeconds = 60;
      mode = "enforce";
    };
  };
}
```

A host on `stable` resolves the same probe set it would have under fleet/tag/host scoping, plus any channel-scoped declarations. The multi-scope merge rule applies uniformly: later scopes override earlier ones on probe-name collision; collisions surface as `mkFleet` warnings (same shape as the existing tag-vs-fleet warnings).

Channel scope is a general declaration site for any probe kind (http, tcp, exec, evidence). Declaring an `http` health probe per channel is equally valid; this is not a compliance-specific affordance. The scope addresses the operator pattern "I want different probe sets on different channels without manually tagging every host," which tag scope handled awkwardly when channel and tag groupings did not naturally align.

### 3.6 Compliance shorthand: capability layer vs policy layer (v0.2)

Compliance probes have a presence in two distinct layers, and conflating them is the source of most operator confusion. The layering is:

| Layer | Lives in | Surfaces | What it declares |
|---|---|---|---|
| **L1 — capability** | The host's NixOS module config (`services.nixfleet-compliance.*`) | `compliance-evidence-collector.service` systemd unit, evidence-collector binary, `/var/lib/nixfleet-compliance/evidence.json` | Whether the host *can* produce evidence at all (collector unit present, controls available, host has the deps). |
| **L2/L3 — policy** | `fleet.nix` topology declarations (`channels.<ch>.compliance.frameworks`, plus refinements at `nixfleet.compliance` / `tags.<t>.compliance` / `hosts.<h>.compliance`) | `evidence-<framework>` probes synthesised into the host's `health-checks.json`; gate decisions | Whether the agent *consumes* that evidence, under what `mode`, and with which per-control exemptions. |

The split is deliberate. L1 is a NixOS-module capability declaration — same character as enabling `services.openssh` or `programs.zsh`. L2/L3 is fleet topology — same character as channel assignments, tag membership, rollout policy. Conflating them produces two failure modes:

1. Operators enable a framework at L2 without the collector unit at L1 → agent probes for missing evidence files; reports `Fail`.
2. Operators enable the collector at L1 without declaring the framework at L2 → evidence is produced and rotting on disk; no one consumes it, no gate effect.

The framework keeps L1 and L2 deliberately separate (no auto-coupling): the NixOS module owns capability; `fleet.nix` owns policy; an operator opts into both explicitly.

### 3.7 Compliance scope hierarchy (v0.2)

The channel-scope `compliance.frameworks` shorthand desugars to `evidence-<framework>` probes synthesised into each host's effective probe set (RFC-0007 §3.5 mechanism — the channel scope is the framework-set's source of truth). On top of that, v0.2 adds **per-framework refinement attrsets** at fleet, tag, and host scope:

```nix
{
  # Fleet-wide compliance refinement
  nixfleet.compliance.frameworks.nis2-essential = {
    mode = "observe";              # downgrade default for rollout window
    reason = "Q2 audit window: observe mode while collectors stabilise";
    controlOverrides."access-control" = {
      mode = "enforce";
      reason = "Always-enforce, even during observe window";
    };
  };

  # Tag-scoped refinement
  nixfleet.tags.audit.compliance.frameworks.nis2-essential = {
    mode = "enforce";              # tag carriers go back to enforce
    reason = "Audit-tagged hosts: always-enforce";
  };

  # Channel-scope declaration (existing, RFC-0007 §3.5)
  nixfleet.channels.stable.compliance.frameworks = ["nis2-essential"];

  # Per-host refinement (RFC-0007 §3.5 + v0.2 framework-level extension)
  nixfleet.hosts.aether.compliance.frameworks.nis2-essential = {
    mode = "disabled";             # Darwin host, no collector available
    reason = "Aether is a Darwin developer host: no NixOS compliance collector";
  };
}
```

**Precedence at synthesis time** (broadest → most-specific, later wins for non-null/non-empty):

```
fleet < tag < channel < host
```

with three field-level merge rules:

| Field | Merge rule |
|---|---|
| `mode` | Most-specific non-null wins. Bare-string channel entries (`frameworks = ["nis2-essential"]`) contribute `mode = null` — i.e. they explicitly defer to a broader-scope mode if any, falling back to the channel's `compliance.mode` default only when no scope declared a mode. Explicit channel-list-entry modes (`{name = "nis2-essential"; mode = "enforce";}`) DO contribute a definitive value at channel scope. |
| `reason` | Most-specific non-empty wins. Annotates `ProbeSubResult.override_reason` for downstream audit. |
| `controlOverrides.<id>` | Per-key deep merge: each scope's entry for a given control ID replaces the same-keyed entry from broader scopes (host > channel > tag > fleet). |

**Aether/Darwin shortcut.** A `mode = "disabled"` at host scope produces an `evidence-<framework>` entry with `mode = "disabled"` in the host's `health-checks.json`; the agent's probe-runner worker skips disabled probes (per RFC-0007 §3.3). Closes the class of "exempt this single host from this framework without carving probe-shadow overrides under `nixfleet.hosts.<h>.healthChecks`."

**Silent no-op for un-enabled frameworks.** Declaring a refinement at fleet/tag/host scope against a framework the channel hasn't enabled (e.g. `nixfleet.compliance.frameworks.iso27001 = { mode = "enforce"; };` when no channel includes `iso27001` in its shorthand list) is a silent no-op — channel scope is the framework-set's source of truth; broader scopes only refine. Operators who want to introduce a brand-new framework probe declare it explicitly under `healthChecks` (kind = "evidence", framework = "..."), not via the compliance shorthand.

**Aside — fleet.nix vs NixOS-state asymmetry.** `healthChecks` lives wholly in `fleet.nix`: every probe declaration at every scope is a topology-layer artifact, transitively signed via the closure hash chain (§5). Compliance is asymmetric: the *capability* to produce evidence (`services.nixfleet-compliance`) is a NixOS-module declaration on the host, while the *policy* to consume it is `fleet.nix` topology. This is documented for the operator's benefit — a probe declaration like `nixfleet.compliance.frameworks.nis2-essential.mode = "disabled"` doesn't disable the collector; it disables the agent's consumption. Disabling the collector itself is a NixOS-module change on the host. RFC-0004 §3 captures the broader pattern of where capability declarations belong (NixOS modules) vs where policy declarations belong (`fleet.nix`).

## 4. Resolution semantics

`mkFleet` computes the effective probe set for each host:

```
effective[host] = merge(
    nixfleet.healthChecks,                                              # fleet-wide
    ∪{nixfleet.tags.<tag>.healthChecks | tag ∈ host.tags},              # tag-scoped
    nixfleet.channels.<host.channel>.healthChecks,                      # channel-scoped
    nixfleet.hosts.<host>.healthChecks,                                 # host-scoped
)
```

**Precedence:** host > channel > tag > fleet. A probe of the same name at a lower scope (lower number above means lower in the merge) **wins outright** - the higher-scope declaration is shadowed in full, not field-merged. This matches the precedence convention from RFC-0001 §"infra tag pin example".

**`mkFleet` access to `host.tags`**: tags are already in scope of fleet-eval per RFC-0001 §3 (tag-driven scope inclusion). The resolver reuses the existing tag mechanism — no new fleet-eval graph traversal required.

**Name collision policy**: `mkFleet` emits a build-time warning when a lower-scope declaration shadows a higher-scope one, including the names of the overriding probe, the overridden scope, and the affected host. Silent shadowing is not permitted.

**Validation that runs at fleet-eval time:**

- Duplicate probe names within a single scope → eval error.
- Missing required field for the probe's kind → eval error.
- Unknown `kind` value → eval error.
- `mode` value outside `enforce | observe | disabled` → eval error.
- An `intervalSeconds <= 0` → eval error (use `mode = "disabled"` to suppress).

## 5. Closure flow (the signing chain)

```
fleet.nix (multi-scope declarations)
            │
            ▼
        mkFleet (fleet-eval-time resolver)
            │
            │  per-host effective probe set
            ▼
        mkHost <host>          # framework's existing config flow-back
            │
            │  injected into host's NixOS modules
            ▼
   _agent.nix renders /etc/nixfleet/agent/health-checks.json
            │
            ▼
   host's NixOS closure        # content-addressed
            │
            ▼
   manifest signs the closure hash      # topology transitively signed
            │
            ▼
   agent reads /etc/nixfleet/agent/health-checks.json on activation
            │
            ▼
   probe runners execute against the effective set
```

The signed manifest declares each host's `closure_hash`; the closure contains the rendered probe configuration; therefore the probe topology is cryptographically signed by the same key, with the same lifecycle, as the rest of the host's configuration. No separate signing surface; no new wire path; the agent reads probes from its own disk — same place it reads everything else.

This is the same flow-back pattern `mkFleet`/`mkHost` already uses for scopes, channels, tags, pins, and compliance frameworks (RFC-0001 §3). Probes plug into the existing pattern rather than introducing a new manifest-payload type.

## 6. Manifest schema delta

**Removed from `fleet_resolved`:**

```
HealthGate.compliance_probes      (dead placeholder)
Channel.compliance.mode           (channel-level enforcement kill-switch)
Channel.compliance.strict         (channel-level probe-error tolerance flag)
```

All three are replaced by per-probe `mode`. The wave-promotion gate's source of truth is now the `probe_failures` derived view written by the applier from `ProbeResult` events where the probe declaration had `mode = "enforce"` and `status = "Fail"`, per the projection in §7.2.

**Probe-error semantics:** uniform `strict = true` behaviour. A probe that errors (nonzero exit code, malformed output, network timeout) counts as `status = "Fail"` regardless of probe kind or channel. Operators who want "tolerate probe errors" use per-probe `mode = "observe"`. Observe-mode failures (whether genuine or erroneous) surface in `event_log` for visibility but do not gate wave promotion. The legacy `Channel.compliance.strict = false` affordance collapses into this one axis.

**No additions** to the manifest schema. Probe declarations live in the closure (rendered from the multi-scope merge), not in the signed manifest payload.

### 6.1 DB schema delta (not part of the wire manifest)

**Removed:** `host_reports` table (v0.1 artifact, no producer in v0.2, schema columns dead).

**Added:** `probe_failures` table — derived view of `event_log`, back-referenced via `event_log_seq` foreign key. Schema in §7.2.

## 7. Compliance as a probe kind

The `compliance-evidence-collector.service` is a systemd unit on each host that, on its own schedule (operator-configurable via `services.compliance-evidence-collector.interval`), produces a signed evidence file at `/var/lib/nixfleet-compliance/evidence.json`.

An `evidence` probe declaration tells the agent to consume the latest evidence file:

```nix
nixfleet.healthChecks.evidence-nis2 = {
  kind = "evidence";
  framework = "nis2-essential";
  intervalSeconds = 60;
  mode = "enforce";
};
```

The probe runner:

1. Stats the evidence file; if `mtime` hasn't advanced since last observation, reports the previous result without re-verification.
2. On new `mtime`: reads the file, verifies its ed25519 signature against the **host's local SSH host key public half** (loaded at agent startup, per RFC-0009 §5 — same source as the agent's evidence-signing identity), checks the framework's pass condition.
3. Emits `ProbeResult { name = "evidence-nis2", status = Pass | Fail, observed_at, sub_results }` via the existing event channel (RFC-0005 §4.2).

The agent does NOT receive the verifying pubkey via probe-config JSON — `cfg.host_pubkey` is loaded locally at startup from the host's own SSH key infrastructure. Probe declarations carry no key material; key plumbing stays in the existing RFC-0009 path.

**The probe runner does not invoke the collector.** Collector cadence and probe cadence are independent. The probe is a read-only consumer; the heavy work (evidence collection + signing) stays with the systemd unit on its operator-controlled schedule.

### 7.1 Per-control granularity in `ProbeResult` payload

`ProbeResult` events carry a kind-specific `sub_results` field. For `kind = "evidence"` probes, the payload preserves per-control accounting:

```rust
// in nixfleet-state-machine / OutboundAgentEvent::ProbeResult
struct ProbeResultPayload {
    name: String,
    status: ProbeStatus,                    // Pass | Fail (aggregate across controls)
    observed_at: DateTime<Utc>,
    mode: ProbeMode,                        // see §8.1 below
    sub_results: Option<Vec<ProbeSubResult>>,
}

struct ProbeSubResult {
    control_id: String,                     // e.g., "nis2.art21.a"
    status: ProbeStatus,                    // Pass | Fail per individual control
    framework: String,                      // e.g., "nis2-essential"
    article: Option<String>,                // e.g., "art.21.a"
}
```

For HTTP/TCP/exec probes, `sub_results` is `None`. For evidence probes, it carries one entry per control evaluated by the framework. The aggregate `status` is `Pass` iff every `sub_result.status == Pass`.

This preserves operator and auditor visibility into *which* controls fail on *which* host. Without `sub_results`, the gate would collapse to "host X compliance failing" and the per-control story would only be reachable via the raw evidence file. With it, `/v1/deferrals` and CP-side projections can report `host = web-01, failing controls = [nis2.art21.a, iso27001.A.5.1]` directly from `event_log`.

### 7.2 CP-side projection rebuild

The `compliance_wave` gate previously consumed `db::reports::outstanding_compliance_events_by_rollout`, a projection built over the v0.1-era `host_reports` table. That input pipeline has no producer in v0.2 (`agent::compliance::*` was removed). Under "v0.2 is a full rewrite, opt for optimal shapes," `host_reports` itself is also deleted in this RFC. The v0.1 schema is suboptimal for v0.2 query patterns: `signature_status` is dead, `report_json` duplicates `event_log.payload`, `event_id UNIQUE` is redundant with `event_log.seq` monotonicity.

Replacement pipeline:

1. **`event_log` is the sole canonical store.** Inbound `ProbeResult` events land in `event_log` with `kind = 'agent_event'` (RFC-0005 §4.3). Append-only audit; consumed by `/v1/rollouts/{id}/events` and replay tooling.
2. **A new `probe_failures` derived view** carries the typed denormalization the gate needs:

```sql
CREATE TABLE probe_failures (
    event_log_seq INTEGER PRIMARY KEY REFERENCES event_log(seq),  -- back-ref to canonical
    rollout_id    TEXT NOT NULL,
    host_id       TEXT NOT NULL,
    probe_name    TEXT NOT NULL,
    control_id    TEXT,                          -- NULL for non-evidence probes
    framework     TEXT,                          -- NULL for non-evidence probes
    observed_at   TEXT NOT NULL
);

CREATE INDEX idx_probe_failures_by_rollout_host_control
    ON probe_failures(rollout_id, host_id, control_id);
```

3. **Single writer.** The applier's `RemoteAppendEventLog` effect handler, on detecting `ProbeResult { mode = "enforce", status = "Fail" }`, writes the `event_log` row AND the per-`sub_result` `probe_failures` rows **in one transaction**. No two-writer divergence; no shadow state. For probes without sub_results (HTTP/TCP/exec aggregate fail), one `probe_failures` row with `control_id = NULL`. For evidence probes, one row per failing control.
4. **Re-derivable from canonical.** `event_log_seq` as a back-reference foreign key means `probe_failures` is provably derivable from `event_log`. If the table is ever lost (DB rebuild, schema rev), a walk over `event_log` reconstructs it. Soft state, hard reference.
5. **Gate reads `probe_failures`** via indexed `(rollout_id, host_id, control_id)`. The projection `db::probe_failures::outstanding_failing_enforce_probes_by_rollout` returns `HashMap<RolloutId, HashMap<HostId, usize>>` where the count is `COUNT(DISTINCT control_id)` per `(rollout, host)`.
6. **`FleetState` field rename:** `outstanding_compliance_events` becomes `outstanding_failing_enforce_probes` (same shape, name reflects the new gate-input model — across enforce-mode probes generally, not specifically compliance).

Gate logic unchanged: refuse to promote a wave if any host in the wave has outstanding failures from earlier waves. Only the input pipeline changes — one canonical store (`event_log`), one derived view (`probe_failures`), one writer (the applier), one consumer (the gate).

## 8. Per-rollout enforce-probe-set discovery

For the wave-promotion gate to distinguish "this enforce-mode probe hasn't reported yet — hold the wave" from "no enforce-mode probes are declared — advance," the CP must know the set of enforce-mode probes the agent is expected to run. The agent provides this two ways (belt-and-braces):

### 8.1 Topology declaration on activation

After every `LocalActivationCompleted` (RFC-0005 §4.2), the agent's probe worker re-reads `/etc/nixfleet/agent/health-checks.json` and emits one event:

```rust
Event::LocalProbeTopologyDeclared {
    rollout_id: RolloutId,
    probes: Vec<ProbeDecl>,                 // (name, kind, mode) for every declared probe
}
```

CP receives the corresponding outbound variant and writes one `event_log` row with `kind = 'agent_event'` carrying the topology. The CP-side projection knows, per `(rollout_id, hostname)`, which probes the agent has committed to running.

Deterministic: the same closure produces the same `LocalProbeTopologyDeclared` payload every time. Replay-friendly: an `event_log` walk reconstructs the topology without needing access to the closure on disk.

### 8.2 Mode field on every `ProbeResult`

Each `ProbeResult` event also carries the probe's `mode` (~4 bytes per event):

```rust
struct ProbeResultPayload {
    // ... (see §7.1)
    mode: ProbeMode,    // enforce | observe | disabled
}
```

`mode = "disabled"` never appears in results (disabled probes don't run); the field's purpose is to let the CP correlate a result to the topology declaration without joining tables.

### 8.3 Gate-side reconciliation

For each `(rollout_id, hostname)`, the gate has:

- The topology declaration: set of `(name, mode)` the agent declared.
- The stream of `ProbeResult`s with timestamps.

Gate logic for "this wave is safe to advance":

- Every probe with declared `mode = "enforce"` has a `ProbeResult` with `status = Pass` and `observed_at >= activation_completed_at`.
- No probe with `mode = "enforce"` has the most recent `ProbeResult.status = Fail`.

Absence handling: a missing `LocalProbeTopologyDeclared` (e.g., agent crashed before emitting) holds the wave with reason `"awaiting probe topology"` until either the topology arrives or operator intervention clears it. Defensive against silent gate-bypass on agent crash.

## 9. Operator workflow

### Adding a probe

- Cross-cutting: declare under `nixfleet.healthChecks` in `fleet.nix`.
- Service-class: declare under `nixfleet.tags.<tag>.healthChecks`.
- Host-specific: declare under `nixfleet.hosts.<host>.healthChecks`.

`nixos-rebuild`/CI signs the new closure; fleet rollout dispatches it; agents activate; probes begin running. Standard push flow — no separate manifest-republish step.

### Changing a probe

Edit the declaration at its current scope, push. The rebuilt closure has the new probe shape; the agent re-reads `/etc/nixfleet/agent/health-checks.json` on `ActivationCompleted` (RFC-0005 §4.2) and respawns runners with the new declarations.

### Removing a probe

Delete the declaration; push. The next closure activation drops it from `effective[host]`; the agent's `LocalResetProbeCache` effect kills the existing runner; the probe stops reporting. Any in-flight `event_log` rows for the probe remain (append-only audit log; RFC-0005 §4.3).

### Disabling a probe temporarily

Change the probe's `mode` to `"disabled"`, push. The probe entry stays in the declaration (so the change is auditable in `fleet.nix` history), but the agent doesn't run it and CP doesn't gate on it. Re-enable by changing `mode` back.

### Re-tagging a host

Add or remove a tag on a host. The merge changes; the host's effective probe set changes accordingly on the next closure activation. Standard tag-membership semantics from RFC-0001.


A new fleet rollout under v0.2 picks up the new shape automatically on the next push; no manual wipe required beyond the standard fresh-DB story.
