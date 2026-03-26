# Contributing to NixFleet

## Getting Started

```sh
# Clone the repo
git clone https://github.com/abstracts33d/fleet && cd fleet

# Enter the dev shell (sets up git hooks, formatters, tools)
nix develop

# Or use the portable shell (no flake checkout needed)
nix run .#shell
```

## Developer Certificate of Origin

We use DCO (Signed-off-by), not a CLA. Add `-s` to your commits:

```sh
git commit -s -m "feat: add edge role"
```

This certifies you wrote the code or have the right to submit it under the project's license.

## Branch Naming

| Prefix | Use |
|--------|-----|
| `feat/` | New features |
| `fix/` | Bug fixes |
| `refactor/` | Code restructuring |
| `docs/` | Documentation only |
| `infra/` | CI, tooling, hooks |

## Pull Request Workflow

1. Create a branch from `main` with the appropriate prefix
2. Make your changes
3. Ensure CI passes: `nix fmt` (formatting) + `nix run .#validate` (all builds)
4. Open a PR -- squash-merge is the default
5. All PRs require review before merge

Pre-commit hooks run `nix fmt --fail-on-change` automatically. Pre-push hooks run `nix run .#validate`.

## Common Tasks

### Adding a Host

1. Add a `mkHost` entry in `modules/fleet.nix`
2. Create `modules/_hardware/<name>/disk-config.nix` (use templates from `_shared/disk-templates/`)
3. Run `nix run .#validate` to verify it builds
4. Update docs: README.md hosts table, `docs/src/hosts/`

### Adding a Scope

1. Create `modules/scopes/<scope>/<nixos|home>.nix`
2. Gate with `lib.mkIf hS.<flag>` on a hostSpec flag
3. Add the flag to `modules/_shared/host-spec-module.nix` if new
4. Add eval tests in `modules/tests/eval.nix`
5. Update docs: README.md scopes table, `docs/src/`

### Adding a Claude Agent

1. Create `.claude/agents/<name>.md` with model, role, and allowed tools
2. If the agent is dispatched by a skill, update the skill file
3. Update CLAUDE.md agents table

## Code Style

- **Nix:** Format with `alejandra` (via `nix fmt`). See `.claude/rules/nix-style.md`
- **Rust:** Format with `cargo fmt`, lint with `cargo clippy`
- **Shell:** Format with `shfmt` (via `nix fmt`)

## Testing

Three tiers:
- **Eval tests** (`modules/tests/eval.nix`): Config correctness. Run with `nix flake check --no-build`
- **VM tests** (`modules/tests/vm.nix`): Runtime behavior. Run with `nix run .#validate -- --vm`
- **Smoke tests**: Post-deploy verification (future)

When adding a feature, add eval assertions. If it has runtime behavior, add a VM test.

## Documentation

All doc trees must stay in sync. When making changes, update:
- `CLAUDE.md` -- AI context
- `README.md` -- User-facing
- `docs/src/` -- Technical mdbook
- `docs/guide/` -- User guide mdbook
- `docs/nixfleet/` -- Business docs (if applicable)

See `.claude/rules/config-dependencies.md` for the full dependency map.
