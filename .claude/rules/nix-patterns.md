# Nix Patterns (nixfleet specifics)

Project-specific Nix patterns beyond the generic rules in `~/.claude/rules/`.

## Deferred Module Pattern

Modules register via `config.flake.modules.{nixos,darwin,homeManager}.<name>`. Auto-included by `mkNixosHost`/`mkDarwinHost` via `builtins.attrValues`. Scope modules self-activate with `lib.mkIf hS.<flag>`.

```nix
{ config, inputs, lib, ... }: {
  config.flake.modules.nixos.myScope = { config, pkgs, lib, ... }: {
    # Applied to all hosts; self-activate with lib.mkIf
  };
}
```

## hostSpec Smart Defaults

Compositor flags auto-propagate via `lib.mkDefault` in `host-spec-module.nix`:
- `useNiri` -> `isGraphical = true`, `useGreetd = true`
- `useHyprland` -> `isGraphical = true`, `useGreetd = true`
- `useGnome` -> `isGraphical = true`, `useGdm = true`
- `isMinimal` -> `isGraphical = false`, `isDev = false`

Priority: org defaults (mkDefault) < role defaults (mkDefault, later in mkMerge) < smart defaults (mkDefault) < host values (no mkDefault).

## The `_` Prefix Convention

Directories prefixed with `_` are excluded from import-tree. Pulled in via explicit imports:
- `_shared/` -- framework API, hostSpec options, disk templates
- `_config/` -- config files shared between HM and wrappers
- `_hardware/` -- per-host hardware configs
- `core/_home/` -- HM tool config fragments (imported by `core/home.nix`)

## Flake Ecosystem

- Built on **flake-parts** + **import-tree**: `flake.nix` calls `mkFlake` with `inputs.import-tree ./modules`
- Every `.nix` under `modules/` is auto-imported except `_`-prefixed
- Systems: `x86_64-linux`, `aarch64-linux`, `aarch64-darwin`, `x86_64-darwin`
- Formatter: **treefmt-nix** (alejandra + shfmt)

## Impermanence

Btrfs root subvolume wiped on every boot. Only explicitly persisted paths survive.

**Scope-aware persistence**: persist paths live alongside their program definitions, not centralized. Use the **HM persistence module** (`home.persistence."/persist"`), not the NixOS one.

```nix
home.persistence."/persist" = lib.optionalAttrs (!hS.isDarwin) {
  directories = [ ".local/share/myprogram" ];
};
```

## HM Module Organization

HM tool configs live in `core/_home/` (zsh, git, starship, ssh, neovim, tmux, etc.), imported by `core/home.nix`. Catppuccin auto-themes all HM-managed tools via `hostSpec.theme.flavor`/`accent`.

Shared config files in `_config/` are consumed by both HM and wrappers (kitty.conf, starship.toml, gitconfig, zsh/).

## Nix Gotchas (project-specific)

1. `perSystem` pkgs don't inherit `allowUnfree` -- unfree packages must go in NixOS/HM modules, not wrappers
2. `catppuccin/nix` has no `darwinModules` -- only `nixosModules` and `homeModules`
3. Wrapped packages conflict with HM `programs.*` -- never wrap a tool that HM manages
4. `home.persistence` doesn't exist on Darwin -- use `lib.optionalAttrs (!hS.isDarwin)`, not `lib.mkIf`
5. Don't persist `.ssh`/`.gnupg` -- agenix creates parent dirs as root, causing HM permission errors
6. Agenix secrets paths write to ephemeral `~/.ssh/`, not `/persist/.ssh/`
7. `nix.gc` on Darwin requires `nix.enable = true` -- incompatible with Determinate installer
8. QEMU nixpkgs hardcodes `/run/opengl-driver` -- needs sudo shim on non-NixOS
9. `nixos-anywhere --chown` args with `:` are misinterpreted by SSH -- use activation scripts
10. `constructFiles` keys in nix-wrapper-modules become bash variable names -- underscores only
11. Niri is NixOS-only -- Wayland compositors need host GPU drivers
12. `spawn-qemu` script -- always use named flags (`--iso`, `--disk`)
