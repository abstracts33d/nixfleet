# aether

## Purpose

Apple Silicon Mac running nix-darwin. Full dev environment with Homebrew casks, custom dock layout, and TouchID sudo.

## Location

- `modules/fleet.nix` (host entry via `mkHost`)

## Configuration

| Property | Value |
|----------|-------|
| Platform | aarch64-darwin |
| Constructor | `mkFleet` -> `mkDarwinHost` (internal) |
| User | <username> |
| AeroSpace WM | Disabled |
| Dev tools | Yes (default) |
| Graphical | Yes (default) |

## Extra Config

Custom dock layout via `extraModules`:
- Apps: Slack, Notion, Obsidian, Alacritty, Kitty, VS Code, RubyMine, Safari, Chrome, Zen, System Settings, UTM
- Folders: `.config/`, `.local/share/`, Downloads

Additional brew: `openssl@3`.

## Active Scopes

catppuccin (HM only), nix-index (HM only), base, graphical (HM), dev, homebrew, karabiner.

## Notes

- No impermanence (macOS has no ephemeral root concept)
- `nix.enable = false` (Determinate installer manages the nix daemon)
- No org-level Claude Code deny list (NixOS only)

## Dependencies

- No hardware modules (macOS)
- Secrets: `github-ssh-key`, `github-signing-key` (via agenix darwinModules)

## Links

- [Host Overview](README.md)
- [Darwin core](../core/darwin.md)
- [Homebrew scope](../scopes/darwin/homebrew.md)
