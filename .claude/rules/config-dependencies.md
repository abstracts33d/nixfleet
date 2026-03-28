# Config Dependency Chains (NixFleet Framework)

When modifying any file in the left column, check and update the right column.

## Framework API
- `_shared/lib/mk-fleet.nix` (API change) → CLAUDE.md, README.md, docs/src/architecture.md, docs/nixfleet/specs/mk-fleet-api.md
- `_shared/host-spec-module.nix` (new flag) → CLAUDE.md, README.md, docs/src/scopes/
- `_shared/lib/roles.nix` (role change) → docs/src/scopes/README.md roles table

## Modules
- New scope in `scopes/` → docs/src/scopes/<name>.md, docs/src/SUMMARY.md, eval test in tests/eval.nix
- New core module → docs/src/core/
- New app in `apps.nix` → docs/src/apps/<name>.md, docs/src/SUMMARY.md, README.md commands

## Rust workspace
- `shared/src/lib.rs` (type change) → agent/, control-plane/, cli/ (shared types)
- New API endpoint → docs/src/apps/control-plane.md
- Agent state machine change → docs/src/scopes/nixfleet-agent.md

## Documentation
- `CLAUDE.md` — framework AI context
- `README.md` — user-facing overview
- `docs/src/` — technical reference + user guide (mdbook, single tree)

## GitHub Issues
- Feature shipped → close linked issues via `Closes #XX` in PR
