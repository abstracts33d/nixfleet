# Core Modules

## Purpose

Core modules are always active on every host. They provide the foundational NixOS, Darwin, and Home Manager configuration that all hosts share: boot, networking, users, security, secrets, and tool configs.

## Location

- `modules/core/nixos.nix` -- NixOS system config
- `modules/core/darwin.nix` -- Darwin system config
- `modules/core/home.nix` -- HM entry point (imports `_home/` tools)
- `modules/core/_home/` -- Individual HM tool configs

## Module Table

| Module | Scope | Key Responsibilities |
|--------|-------|---------------------|
| [nixos](nixos.md) | `flake.modules.nixos.core` | Boot, networking, users, SSH, firewall, agenix, org-level Claude deny list |
| [darwin](darwin.md) | `flake.modules.darwin.core` | Nix settings, TouchID sudo, dock management, system defaults, agenix |
| [home](home.md) | `flake.modules.homeManager.core` | Imports 9 tool configs from `_home/` |

## HM Tool Fragments (`_home/`)

| File | Programs |
|------|----------|
| `zsh.nix` | zsh with zplug, fzf, syntax-highlighting, autosuggestions, vi-mode |
| `git.nix` | git with signing, delta diff, GitHub user/email from hostSpec |
| `starship.nix` | starship prompt from `_config/starship.toml` |
| `ssh.nix` | SSH config with GitHub host |
| `keys.nix` | SSH key management |
| `neovim.nix` | Neovim with config from `_config/nvim/` |
| `tmux.nix` | tmux with catppuccin, vim keys, resurrect |
| `simple.nix` | kitty, bat, btop, eza, fzf, yazi, zoxide, zellij |
| `gpg.nix` | GnuPG agent config |

## Links

- [Architecture](../architecture.md)
- [Scope System](../scopes/README.md)
