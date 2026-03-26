# gdm

## Purpose

GNOME Display Manager as a standalone display manager (can be used without the full GNOME desktop). Enables supporting services for a complete desktop experience.

## Location

- `modules/scopes/display/gdm.nix`

## Configuration

**Gate:** `useGdm`

### NixOS module
- `services.xserver.displayManager.gdm.enable = true`
- PAM: GNOME keyring enabled for GDM

**Supporting services:**
- gvfs, devmon, udisks2, upower
- power-profiles-daemon, accounts-daemon
- GNOME sushi (file previewer), glib-networking, gnome-online-accounts

## Dependencies

- Depends on: hostSpec `useGdm` flag
- Activated by: `useGnome` (via smart defaults)
- Used by: [ohm](../../hosts/ohm.md)

## Links

- [Scope Overview](../README.md)
- [GNOME](../desktop/gnome.md)
