# gnome

## Purpose

GNOME desktop environment with extensive bloatware removal. Enables GDM, GNOME desktop manager, and Wayland session variables. Persists dconf settings for impermanent hosts.

## Location

- `modules/scopes/desktop/gnome.nix`

## Configuration

**Gate:** `useGnome`

**Smart defaults:** `useGnome` implies `isGraphical = true` and `useGdm = true`.

### NixOS module
- `services.xserver.enable = true`
- `displayManager.gdm.enable = true`
- `desktopManager.gnome.enable = true`
- Session variables: `NIXOS_OZONE_WL=1`, `QT_WAYLAND_DISABLE_WINDOWDECORATION=1`
- Portal: xdg-desktop-portal-gnome
- System packages: gnome-tweaks
- `programs.dconf.enable = true`

**Excluded GNOME packages:** gedit, gnome-connections, gnome-console, gnome-photos, gnome-tour, snapshot, atomix, cheese, epiphany, evince, geary, gnome-calendar, gnome-characters, gnome-clocks, gnome-contacts, gnome-initial-setup, gnome-logs, gnome-maps, gnome-music, gnome-terminal, gnome-weather, hitori, iagno, simple-scan, tali, yelp.

### HM module (impermanence persist)
- `.config/dconf`
- `.local/share/gnome-online-accounts`

## Dependencies

- Depends on: hostSpec `useGnome` flag
- Activates: [GDM](../display/gdm.md) (via smart defaults)
- Used by: [ohm](../../hosts/ohm.md)

## Links

- [Scope Overview](../README.md)
- [GDM](../display/gdm.md)
- [ohm host](../../hosts/ohm.md)
