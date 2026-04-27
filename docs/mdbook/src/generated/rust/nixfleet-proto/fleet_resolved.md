# `nixfleet_proto::fleet_resolved`

`fleet.resolved.json` — CONTRACTS.md §I #1, RFC-0001 §4.1.

Produced by CI (Stream A invoking Stream B's Nix eval). Consumed
by the control plane and, on the fallback direct-fetch path, by
agents. Byte-identical JCS canonical bytes across Nix and Rust.

## Items

### 🔓 `struct FleetResolved`

_(no doc comment)_


### 🔓 `struct Host`

_(no doc comment)_


### 🔓 `struct Channel`

_(no doc comment)_


### `impl Channel`

- **`freshness_window_duration`** — Returns `freshness_window` as a [`std::time::Duration`].

The underlying field carries MINUTES (see the field doc); passing
it directly to `Duration::from_secs` would silently shrink the
window by 60×. Call this helper at the seam between proto and any
`Duration`-consuming API (`verify_artifact`, tick handlers, …).

### 🔓 `struct Compliance`

_(no doc comment)_


### 🔓 `struct RolloutPolicy`

_(no doc comment)_


### 🔓 `struct PolicyWave`

_(no doc comment)_


### 🔓 `struct Selector`

_(no doc comment)_


### 🔓 `struct HealthGate`

_(no doc comment)_


### 🔓 `struct SystemdFailedUnits`

_(no doc comment)_


### 🔓 `struct ComplianceProbes`

_(no doc comment)_


### 🔓 `struct Wave`

_(no doc comment)_


### 🔓 `struct Edge`

_(no doc comment)_


### 🔓 `struct DisruptionBudget`

_(no doc comment)_


### 🔓 `struct Meta`

_(no doc comment)_


