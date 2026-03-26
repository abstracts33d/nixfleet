# greetd

## Purpose

Greetd display manager with tuigreet TUI. Auto-detects the compositor to launch based on hostSpec flags (Niri, Hyprland, or fallback to bash).

## Location

- `modules/scopes/display/greetd.nix`

## Configuration

**Gate:** `useGreetd`

### Session command selection
```
if useNiri  -> "niri"
if useHyprland -> "Hyprland"
else -> "bash"
```

### NixOS module
- `services.greetd.enable = true`
- Default session: `tuigreet --time --remember --cmd <session>`
- Session user: `greeter`
- PAM: GNOME keyring enabled for greetd

## Dependencies

- Depends on: hostSpec `useGreetd` flag
- Activated by: `useNiri` or `useHyprland` (via smart defaults)
- Used by: [krach](../../hosts/krach.md), [krach-qemu](../../hosts/vm/krach-qemu.md)

## Links

- [Scope Overview](../README.md)
- [Niri](../desktop/niri.md)
- [Hyprland](../desktop/hyprland.md)
