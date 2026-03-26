# niri

## Purpose

Niri is a scrollable-tiling Wayland compositor (NixOS-only). This scope enables Niri via `programs.niri` from nixpkgs, bundles Noctalia Shell as the desktop shell (bar, launcher, notifications), and deploys a KDL config via HM.

## Location

- `modules/scopes/desktop/niri.nix`

## Configuration

**Gate:** `useNiri`

**Smart defaults:** `useNiri` implies `isGraphical = true` and `useGreetd = true`.

### NixOS module
- `programs.niri.enable = true`
- `security.polkit.enable = true`
- Noctalia Shell in system packages

### HM module
Deploys `~/.config/niri/config.kdl` with:
- Noctalia Shell spawns at startup
- US keyboard layout
- Layout gaps: 5px
- Keybinds: `Mod+Return` (kitty), `Mod+Q` (close), `Mod+S` (launcher toggle)

### Noctalia package
Built as a `perSystem` package via nix-wrapper-modules:
```nix
packages.noctalia = inputs.wrapper-modules.wrappers.noctalia-shell.wrap { inherit pkgs; };
```

## Notes

Niri is NixOS-only -- Wayland compositors need host GPU drivers. Cannot be in portable wrappers.

## Dependencies

- Input: `wrapper-modules` (for Noctalia Shell)
- Depends on: hostSpec `useNiri` flag
- Activates: [greetd](../display/greetd.md) (via smart defaults)
- Requires: [graphical](../graphical.md) scope (via smart defaults)

## Links

- [Scope Overview](../README.md)
- [greetd](../display/greetd.md)
- [krach host](../../hosts/krach.md)
