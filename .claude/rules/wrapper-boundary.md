# Wrapper Boundary

Two categories of packages in this repo:

## Wrappers (`modules/wrappers/`)
Portable, no GPU deps. `nix run .#shell` works on any machine with Nix.

## NixOS scopes (`modules/scopes/`)
Platform-specific. Niri, noctalia are NixOS-only (need host GPU drivers).

## Individual tools
Always go to HM `programs.*` -- never wrap them. HM provides catppuccin auto-theming, shell integrations, and avoids binary conflicts.

| Layer | Managed by | Why |
|-------|-----------|-----|
| Individual tools (kitty, git, starship, bat, etc.) | HM `programs.*` | Catppuccin auto-themes, shell integrations, no conflicts |
| Portable dev shell (`nix run .#shell`) | Wrapper | Self-contained zsh + tools for remote machines |
| Portable terminal (`nix run .#terminal`) | Wrapper | Kitty wrapping the portable shell |
| Desktop session (niri + noctalia) | NixOS scope | Needs host GPU drivers |

## Decision rule
- New tool -> HM module
- New portable composite -> `wrappers/`
- New desktop component -> `scopes/desktop/`
- Never wrap anything that needs GPU drivers
