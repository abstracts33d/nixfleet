# Config Dependency Chains

When modifying any file in the left column, check and update the right column.

## hostSpec flags
- `_shared/host-spec-module.nix` (new flag) -> CLAUDE.md flags table + README.md scopes table
- `_shared/host-spec-module.nix` (smart defaults) -> verify all hosts still build

## Impermanence paths
- New HM program with state -> add `home.persistence` in same scope module
- Guard with `lib.optionalAttrs (!hS.isDarwin)` -- Darwin has no `home.persistence`

## Host changes
- New/renamed host -> README.md hosts table + CLAUDE.md module tree
- VM host changes -> verify `mkVmHost` still works, update `spawn-qemu`/`spawn-utm` docs

## Documentation trees (ALL must stay in sync — blocking for merge)
Any code/architecture change must update ALL affected doc trees:
- `CLAUDE.md` — AI context (module tree, flags, skills, architecture)
- `README.md` — user-facing (hosts, scopes, commands)
- `docs/src/` — technical mdbook (architecture, hosts, scopes, testing, apps)
- `docs/guide/` — user guide mdbook (getting started, concepts, advanced, development)
- `docs/nixfleet/` — business docs (roadmap.yaml phase status, specs)

Specific triggers:
- New module/file → `docs/src/` page + `SUMMARY.md`
- New hostSpec flag → CLAUDE.md flags table + README.md
- New host → `docs/src/hosts/` page + fleet count in README
- Phase completed → `docs/nixfleet/data/roadmap.yaml` status
- New role → CLAUDE.md roles table + `docs/src/` architecture
- Command changed → `docs/guide/` getting-started/development pages

