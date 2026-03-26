# Theming with Catppuccin

Consistent colors across every application.

## The Approach

This config uses [Catppuccin](https://catppuccin.com/) (Macchiato flavor, Lavender accent) everywhere. The `catppuccin/nix` module auto-themes 200+ applications through Home Manager.

## What Gets Themed

Everything that supports it:
- Terminal (kitty)
- Shell prompt (starship)
- Editor (neovim)
- File manager, system monitor (btop), and more
- GTK and Qt applications
- The compositor and display manager

## How It Works

The catppuccin scope activates on any non-minimal host:

```nix
config = lib.mkIf (!hS.isMinimal) {
  catppuccin = {
    flavor = "macchiato";
    accent = "lavender";
  };
};
```

Home Manager's catppuccin module then applies the theme to every `programs.*` that supports it. No per-app configuration needed.

## Portable Too

The portable shell and terminal wrappers include Catppuccin colors in their config files (`_config/kitty.conf`, `_config/starship.toml`). You get the same look on any machine.

## Further Reading

- [Technical Catppuccin Details](../../src/scopes/catppuccin.md) — module configuration
