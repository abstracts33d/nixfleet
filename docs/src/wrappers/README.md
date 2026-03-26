# Portable Wrappers

## Purpose

Self-contained portable environments using nix-wrapper-modules. Run on any machine with Nix installed -- configs bundled from `_config/` (same source as HM). No GPU drivers needed.

## Location

- `modules/wrappers/shell.nix`
- `modules/wrappers/terminal.nix`

## Wrapper Table

| Package | Command | Description |
|---------|---------|-------------|
| [shell](shell.md) | `nix run .#shell` | Zsh + 20 CLI tools with bundled configs |
| [terminal](terminal.md) | `nix run .#terminal` | Kitty wrapping the portable shell |

## Boundary Rule

Wrappers are for **portable composites only**. Individual tools (kitty, git, starship) go to HM `programs.*` for catppuccin auto-theming and shell integrations. Never wrap a tool that HM manages.

## Dependencies

- Input: `wrapper-modules` (github:BirdeeHub/nix-wrapper-modules)
- Config source: `modules/_config/` (shared with HM)

## Links

- [Architecture](../architecture.md)
- [Core HM](../core/home.md) (shares config files)
