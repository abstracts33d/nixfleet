# Day-to-Day Usage

Your daily workflow after installation.

## The Core Loop

1. Edit Nix files in `modules/`
2. `git add .` (Nix only sees tracked files)
3. `nix run .#build-switch`
4. Done. Changes are live.

## Common Commands

| Command | What it does |
|---------|-------------|
| `nix run .#build-switch` | Rebuild and switch to latest config |
| `nix run .#validate` | Run all checks (format, eval, builds) |
| `nix run .#validate -- --vm` | Include slow VM integration tests |
| `nix fmt` | Format all Nix files |
| `nix run .#docs` | Serve technical docs locally |
| `nix run .#docs-guide` | Serve this guide locally |

## Updating Inputs

```sh
# Update everything
nix flake update

# Update just secrets
nix flake update secrets

# Update just nixpkgs
nix flake update nixpkgs
```

After updating, rebuild with `nix run .#build-switch`.

## Portable Environments

Use these on any machine with Nix installed — no clone needed:

```sh
# Full dev shell (zsh + tools + config)
nix run github:abstracts33d/nixfleet#shell

# Dev shell + kitty terminal
nix run github:abstracts33d/nixfleet#terminal
```

## Rolling Back

On macOS, if a rebuild breaks something:

```sh
nix run .#rollback
```

On NixOS, select a previous generation from the boot menu, or:

```sh
sudo nixos-rebuild switch --rollback
```

## Development Workflow

The repo uses git hooks (activated by the devShell):

- **pre-commit:** Format check (`nix fmt --fail-on-change`)
- **pre-push:** Full validation (`nix run .#validate`)

Enter the devShell with `nix develop` or use direnv for automatic activation.

## Next Steps

- [The Scope System](../concepts/scopes.md) — understand how features are organized
- [Testing](../development/testing.md) — the test pyramid
- [Adding a New Host](../advanced/new-host.md) — expand your fleet
