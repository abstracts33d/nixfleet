# Quick Start

Get NixFleet running in 5 minutes. For full documentation, see the [User Guide](docs/guide/README.md).

## Prerequisites

- [Nix](https://install.determinate.systems/nix) installed (with flakes enabled)

## 1. Clone

```sh
git clone https://github.com/abstracts33d/fleet && cd fleet
```

## 2. Explore the Fleet

```sh
# List all host configurations
nix eval .#nixosConfigurations --apply 'x: builtins.attrNames x' --json | jq .

# List Darwin configurations
nix eval .#darwinConfigurations --apply 'x: builtins.attrNames x' --json | jq .

# Check available packages and apps
nix flake show
```

## 3. Run the Demo

The demo spawns a local control plane and fleet agents to show the full lifecycle:

```sh
bash demo/spawn-fleet.sh demo
```

This starts:
- A control plane (Axum HTTP server)
- Two demo agents that register, poll for config, and report status
- Live output showing the agent <-> CP communication

## 4. Try the CLI

```sh
# See available commands
nix run .#nixfleet -- --help

# Dry-run a deployment
nix run .#nixfleet -- deploy --dry-run --hosts "krach"
```

## 5. Try the Portable Shell

Works on any machine with Nix -- no checkout needed:

```sh
# Full dev environment (zsh + starship + git + neovim + 20 more tools)
nix run github:abstracts33d/fleet#shell

# Configured kitty terminal wrapping the dev shell
nix run github:abstracts33d/fleet#terminal
```

## Next Steps

- [README.md](README.md) -- Full feature overview, host table, scope table
- [ARCHITECTURE.md](ARCHITECTURE.md) -- How the pieces fit together
- [TECHNICAL.md](TECHNICAL.md) -- Design decisions and Nix gotchas
- [CLAUDE.md](CLAUDE.md) -- Framework context, commands, conventions
- [docs/guide/](docs/guide/) -- Detailed user guide
- [docs/src/](docs/src/) -- Technical reference
