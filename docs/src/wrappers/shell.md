# shell

## Purpose

Portable dev environment: zsh with 20+ CLI tools and all configs bundled from `_config/`. Run from any machine with Nix: `nix run .#shell`.

## Location

- `modules/wrappers/shell.nix`

## Configuration

### Bundled tools
- **Editors:** neovim, helix
- **Shell:** starship, tmux, bat, btop, zellij, fastfetch
- **Files:** eza, fd, fzf, yazi, tree, jq, yq, ripgrep
- **Git:** git, gh
- **Network:** curl, wget, zoxide
- **Zsh plugins:** fzf-tab, vi-mode, history-substring-search, syntax-highlighting, autosuggestions

### Bundled configs (from `_config/`)
- `.zshrc` -- composed from `wrapperrc.zsh` + plugin sourcing + `aliases.zsh` + `functions.zsh`
- `starship.toml` -- prompt config
- `gitconfig` -- shared git settings

### Environment variables
- `STARSHIP_CONFIG` -> bundled starship.toml
- `GIT_CONFIG_GLOBAL` -> bundled gitconfig
- `BAT_STYLE` -> `changes,header`
- `ZDOTDIR` -> directory containing bundled `.zshrc`

## Dependencies

- Input: `wrapper-modules`
- Config source: `_config/zsh/`, `_config/starship.toml`, `_config/gitconfig`
- Depended on by: [terminal](terminal.md) wrapper

## Links

- [Wrappers Overview](README.md)
- [Core HM zsh](../core/home.md) (HM equivalent)
