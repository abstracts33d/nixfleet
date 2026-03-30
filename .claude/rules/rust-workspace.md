# Rust Workspace (nixfleet specifics)

4 crates: `shared/` (nixfleet-types), `agent/`, `control-plane/`, `cli/`.

## Crate Map

| Crate | Type | Key modules |
|-------|------|-------------|
| `shared` | lib | DesiredGeneration, Report, MachineStatus, MachineLifecycle, API path constants |
| `agent` | bin | main (clap+tokio), config, comms (reqwest), nix (CLI wrapper), health, state (FSM), store (SQLite) |
| `control-plane` | bin | main (Axum), routes, state (FleetState: HashMap+RwLock), db (SQLite) |
| `cli` | bin | deploy, status, rollback, host (add, provision) |

## Control Plane API

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/health` | GET | Health check |
| `/api/v1/machines` | GET | List fleet (with lifecycle) |
| `/api/v1/machines/{id}/desired-generation` | GET | Agent polls this |
| `/api/v1/machines/{id}/set-generation` | POST | Admin sets target |
| `/api/v1/machines/{id}/report` | POST | Agent reports (auto-activates Pending->Active) |
| `/api/v1/machines/{id}/register` | POST | Pre-register machine |
| `/api/v1/machines/{id}/lifecycle` | PATCH | Change lifecycle state |

Machine lifecycle: `Pending -> Active -> Maintenance -> Decommissioned`. First report auto-activates.

## Key Patterns

- **No OpenSSL** -- all crates use `reqwest` with `rustls-tls`
- **Shared types** -- agent + CP + CLI use same types from `nixfleet-types`
- **Async everywhere** -- tokio runtime, `tokio::fs`, `tokio::process::Command`
- **Store errors non-fatal** -- `store.log_*()` failures logged as warnings
- **Graceful shutdown** -- `tokio::signal::ctrl_c()` + `tokio::select!`
- **String interpolation safety** -- `lib.escapeShellArg` for NixOS module ExecStart

## Nix Packaging

Both binaries via `rustPlatform.buildRustPackage`:
- `nix build .#nixfleet-agent` / `.#nixfleet-control-plane` / `.#nixfleet-cli`
- Workspace root as `src`, `cargoBuildFlags = ["-p" "<crate-name>"]`

## Testing

125+ tests: `cargo test --workspace --bins --tests --lib`
- Agent: unit tests per module + `tests/integration.rs`
- CP: unit + `tests/agent_integration.rs` (real HTTP on port 0)
- Shared: serde round-trip tests
- VM: `vm-nixfleet` 2-node nixosTest (CP + agent cycle)
