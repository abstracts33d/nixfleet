# Home Manager (nixfleet specifics)

Extends generic HM patterns from the claude-core plugin.

## HM Module Organization

HM tool configs live in `core/_home/`:
- `zsh.nix` -- shell configuration (zplug, plugins, aliases)
- `git.nix` -- git config (signing, aliases, delta)
- `starship.nix` -- prompt config (loads `_config/starship.toml`)
- `ssh.nix` -- SSH client config
- `keys.nix` -- public key deployment
- `neovim.nix` -- editor (loads `_config/nvim/`)
- `tmux.nix` -- terminal multiplexer
- `simple.nix` -- tool enables (bat, btop, kitty, fzf, etc.)
- `gpg.nix` -- GPG agent configuration

These are imported by `core/home.nix`, not by import-tree.

## Catppuccin Auto-Theming

All HM-managed tools get catppuccin theming automatically via the `catppuccin.nix` scope. Flavor and accent are set from `hostSpec.theme.flavor` / `hostSpec.theme.accent`. This is why individual tools should never be wrapped -- wrapping bypasses catppuccin integration.

**Important:** catppuccin/nix provides only `nixosModules` and `homeModules` -- no `darwinModules`. Never import `nixosModules` into Darwin (class mismatch error).

## Shared Config Files (`_config/`)

`_config/` files are the shared source of truth consumed by both HM and wrappers:

| File | HM consumer | Wrapper consumer |
|------|------------|-----------------|
| `_config/kitty.conf` | `programs.kitty.extraConfig` | `wrappers/terminal.nix` |
| `_config/starship.toml` | `programs.starship.settings` | `wrappers/shell.nix` |
| `_config/gitconfig` | `programs.git` (adds user/email/signing on top) | `wrappers/shell.nix` |
| `_config/zsh/wrapperrc.zsh` | N/A (wrapper only) | `wrappers/shell.nix` |
| `_config/zsh/aliases.zsh` | both | both |
| `_config/nvim/` | `programs.neovim` | N/A |

**HM-only settings** (not in wrappers): zplug plugins, syntax-highlighting, autosuggestion, nix-daemon source, dynamic `hS.githubUser`/`hS.githubEmail`.

## Persistence Integration

HM programs with state need persist paths added in the same scope module using `home.persistence."/persist"`. This is the HM impermanence module (not the NixOS one), which creates dirs with correct user ownership.

Guard persistence with `lib.optionalAttrs (!hS.isDarwin)` -- not just `lib.mkIf`, because `home.persistence` option type does not exist on Darwin and `mkIf` still evaluates the type.
