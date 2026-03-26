# core/home.nix

## Purpose

Entry point for all Home Manager tool configurations. Imports 9 tool config fragments from `_home/` that apply to every host.

## Location

- `modules/core/home.nix`
- `modules/core/_home/` -- individual tool configs

## Imported Modules

| File | What it configures |
|------|--------------------|
| `zsh.nix` | Zsh with zplug plugins, fzf integration, syntax highlighting, autosuggestions, vi-mode, history-substring-search |
| `git.nix` | Git with delta diff, GPG signing, GitHub user/email from hostSpec, gitconfig from `_config/` |
| `starship.nix` | Starship prompt from `_config/starship.toml` |
| `ssh.nix` | SSH client config with GitHub host entry |
| `keys.nix` | SSH key file management |
| `neovim.nix` | Neovim with NvChad config from `_config/nvim/` |
| `tmux.nix` | tmux with catppuccin theme, vim-style keys, resurrect plugin |
| `simple.nix` | kitty, bat, btop, eza, fzf, yazi, zoxide, zellij |
| `gpg.nix` | GnuPG agent configuration |

## Config Sources (`_config/`)

HM modules and wrappers share config files from `_config/`:

| Config file | Used by HM | Used by wrapper |
|-------------|-----------|-----------------|
| `_config/starship.toml` | `starship.nix` | `shell.nix` |
| `_config/gitconfig` | `git.nix` | `shell.nix` |
| `_config/kitty.conf` | `simple.nix` | `terminal.nix` |
| `_config/zsh/wrapperrc.zsh` | -- | `shell.nix` |
| `_config/zsh/aliases.zsh` | -- | `shell.nix` |
| `_config/zsh/functions.zsh` | -- | `shell.nix` |
| `_config/nvim/` | `neovim.nix` | -- |
| `_config/karabiner.json` | `karabiner.nix` | -- |

## Dependencies

- Config source: `modules/_config/`
- Registered as: `flake.modules.homeManager.core`

## Links

- [Core Overview](README.md)
- [Wrappers](../wrappers/README.md) (share config files)
