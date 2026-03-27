# Rust Workspace

Knowledge about the NixFleet Rust workspace (4 crates).

## Workspace Structure

```
Cargo.toml              # Workspace root
shared/                 # nixfleet-types (library) — DesiredGeneration, Report, MachineStatus, MachineLifecycle
agent/                  # nixfleet-agent (binary) — poll loop, state machine, nix CLI wrapper
control-plane/          # nixfleet-control-plane (binary) — Axum server, fleet state, SQLite, machine registry
cli/                    # nixfleet-cli (binary) — deploy, status, rollback, host add/provision
```

## Agent (`agent/src/`)

| Module | Responsibility |
|--------|---------------|
| `main.rs` | CLI (clap), tokio runtime, state machine loop with graceful shutdown |
| `config.rs` | Config struct (CP URL, poll interval, cache, dry-run) |
| `comms.rs` | HTTP client for control plane (reqwest + rustls) |
| `nix.rs` | Nix ops: current_generation (async), fetch, apply, rollback |
| `health.rs` | `systemctl is-system-running` check |
| `state.rs` | AgentState enum (Idle→Checking→Fetching→Applying→Verifying→Reporting + RollingBack) |
| `store.rs` | SQLite persistence with transactions, mutex error handling |
| `types.rs` | Re-exports from `nixfleet-types` |

## Control Plane (`control-plane/src/`)

| Module | Responsibility |
|--------|---------------|
| `main.rs` | CLI + Axum router setup |
| `lib.rs` | `build_app()` for integration tests |
| `routes.rs` | API handlers (list machines, get/set generation, report, register, lifecycle) |
| `state.rs` | In-memory FleetState (HashMap + RwLock), machine lifecycle states |
| `db.rs` | SQLite: generations, reports, machines tables |

### API Endpoints

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/health` | GET | Health check |
| `/api/v1/machines` | GET | List fleet (with lifecycle status) |
| `/api/v1/machines/{id}/desired-generation` | GET | Agent polls this |
| `/api/v1/machines/{id}/set-generation` | POST | Admin sets target |
| `/api/v1/machines/{id}/report` | POST | Agent reports (auto-activates Pending→Active) |
| `/api/v1/machines/{id}/register` | POST | Pre-register a machine |
| `/api/v1/machines/{id}/lifecycle` | PATCH | Change lifecycle state |

### Machine Lifecycle

`Pending → Active → Maintenance → Decommissioned`

Auto-activate: first agent report transitions Pending/Provisioning → Active.

## CLI (`cli/src/`)

| Module | Subcommands |
|--------|-------------|
| `main.rs` | deploy, status, rollback, host (add, provision) |
| `deploy.rs` | Build closures, POST to CP or SSH fallback |
| `status.rs` | GET /machines, table output |
| `host.rs` | Scaffold hardware config, generate fleet.nix snippet, nixos-anywhere |

## Shared Types (`shared/src/lib.rs`)

`DesiredGeneration`, `Report`, `MachineStatus`, `MachineLifecycle`, API path constants.

## Key Patterns

- **No OpenSSL** — all crates use `reqwest` with `rustls-tls` feature
- **Shared types** — agent + CP + CLI use same types from `nixfleet-types`
- **Async everywhere** — `tokio` runtime, `tokio::fs` for file ops, `tokio::process::Command` for nix
- **Store errors non-fatal** — `store.log_*()` failures logged as warnings, don't crash agent
- **Graceful shutdown** — `tokio::signal::ctrl_c()` + `tokio::select!`
- **String interpolation safety** — `lib.escapeShellArg` for NixOS module ExecStart

## Nix Packaging

Both binaries via `rustPlatform.buildRustPackage`:
- `nix build .#nixfleet-agent` / `nix build .#nixfleet-control-plane` / `nix build .#nixfleet-cli`
- Workspace root as `src`, `cargoBuildFlags = ["-p" "<crate-name>"]`
- NixOS modules: `services.nixfleet-agent`, `services.nixfleet-control-plane`

## Testing

125+ tests across workspace:
- `cargo test --workspace --bins --tests --lib` runs all
- Agent: unit tests in each module + `tests/integration.rs`
- Control plane: unit tests + `tests/agent_integration.rs` (real HTTP server on port 0)
- Shared: serde round-trip tests
