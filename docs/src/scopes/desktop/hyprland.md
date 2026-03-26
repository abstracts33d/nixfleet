# hyprland

## Purpose

Hyprland dynamic tiling Wayland compositor with XWayland support, UWSM session manager, and a full set of desktop utilities (waybar, wofi, tofi, hyprlock, wlogout).

## Location

- `modules/scopes/desktop/hyprland.nix`

## Configuration

**Gate:** `useHyprland`

**Smart defaults:** `useHyprland` implies `isGraphical = true` and `useGreetd = true`.

### NixOS module
- `programs.hyprland.enable = true` (with XWayland, UWSM)
- Session variables: `NIXOS_OZONE_WL=1`, `QT_WAYLAND_DISABLE_WINDOWDECORATION=1`
- System packages: file-roller, nautilus, totem, brightnessctl, networkmanagerapplet, pavucontrol, wf-recorder
- Portal: xdg-desktop-portal-hyprland

### HM module
- Hyprland settings: SUPER as mod key, kitty terminal, firefox browser
- Launchers: tofi-drun, tofi-run, wofi
- Keybinds: `$mod+Return` (terminal), `$mod+a/s` (tofi), `$mod+d/f` (wofi), `$mod+SHIFT+q` (kill), `$mod+SHIFT+e` (exit), `$mod+SHIFT+l` (hyprlock)
- Programs: hyprlock, waybar (systemd), wofi, tofi, wlogout

## Dependencies

- Depends on: hostSpec `useHyprland` flag
- Activates: [greetd](../display/greetd.md) (via smart defaults)
- Requires: [graphical](../graphical.md) scope (via smart defaults)

## Links

- [Scope Overview](../README.md)
- [greetd](../display/greetd.md)
