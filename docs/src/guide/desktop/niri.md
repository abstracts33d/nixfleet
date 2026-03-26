# Niri + Noctalia

The default desktop: a scrollable tiling Wayland compositor with a custom shell.

## What Is Niri?

[Niri](https://github.com/YaLTeR/niri) is a scrollable tiling Wayland compositor. Unlike traditional tiling WMs that arrange windows in a grid, Niri arranges them in an infinite scrollable strip. Think of it as a horizontal workspace that extends as you open more windows.

## What Is Noctalia?

Noctalia is a custom shell (panel/bar) built for Niri. It provides:
- System tray
- Workspace indicators
- Clock and status bar
- Notification area

It is packaged as a Nix wrapper in this config and deployed alongside Niri.

## Why Niri?

- **Simple mental model** — windows scroll left/right, no complex layouts
- **Wayland-native** — no X11 compatibility layer
- **Keyboard-driven** — efficient navigation without touching the mouse
- **NixOS integration** — uses `programs.niri` from nixpkgs

## Configuration

Niri config is deployed via Home Manager's `xdg.configFile`. The compositor itself is NixOS-only (it needs host GPU drivers), but config management is platform-agnostic.

Enable it with one flag:

```nix
hostSpecValues = {
  useNiri = true;
};
```

This automatically enables `isGraphical` and `useGreetd`.

## Further Reading

- [Choosing a Desktop](choosing.md) — compare all options
- [Theming](theming.md) — Catppuccin integration
- [Technical Desktop Details](../../scopes/desktop/niri.md) — module internals
