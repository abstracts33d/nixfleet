# graphical

## Purpose

Graphical desktop foundation: audio (Pipewire), fonts, XDG portals, hardware graphics, browsers, editors, and media tools. Split across NixOS system config and HM user programs.

## Location

- `modules/scopes/graphical/nixos.nix` -- system-level services and packages
- `modules/scopes/graphical/home.nix` -- user-level programs and packages

## Configuration

**Gate:** `isGraphical`

### NixOS module

**Services:**
- Pipewire (ALSA, PulseAudio, JACK)
- libinput, gvfs, tumbler, devmon
- GNOME Keyring

**Security:** rtkit, PAM GNOME keyring, seahorse, pinentry-gnome3

**XDG portal:** enabled with wlr + gtk portals

**Fonts:** MesloLG Nerd Font, DejaVu, JetBrains Mono, Font Awesome, Noto (+ emoji)

**System packages:** LibreOffice, VLC, pavucontrol, flameshot, zathura

### HM module

**Browsers:** Firefox, Google Chrome, Brave (with extensions: Dark Reader, Bitwarden, Tab Session Manager)

**Editors:** VS Code

**Media:** asciinema, halloy (IRC), spotifyd, ffmpeg, imagemagick, neovide, neomutt, cmus, mpd

### Impermanence persist paths

Chrome, Firefox, Brave, VS Code, Slack, halloy configs.

## Dependencies

- Depends on: hostSpec `isGraphical` flag
- Desktop scopes (niri, hyprland, gnome) build on top of this

## Links

- [Scope Overview](README.md)
- [Niri](desktop/niri.md)
- [Hyprland](desktop/hyprland.md)
- [GNOME](desktop/gnome.md)
