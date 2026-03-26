# Choosing a Desktop

This config supports multiple desktop environments. Pick the one that fits your workflow.

## Options

| Desktop | Flag | Best for |
|---------|------|----------|
| Niri + Noctalia | `useNiri = true` | Tiling Wayland, keyboard-driven workflow |
| Hyprland | `useHyprland = true` | Tiling Wayland, animation-focused |
| GNOME | `useGnome = true` | Traditional desktop, touchpad gestures |
| None | `isGraphical = false` | Servers, headless machines |

## How to Switch

Change one flag in your host file and rebuild:

```nix
hostSpecValues = {
  useNiri = true;    # switch to: useHyprland or useGnome
};
```

The smart defaults handle the rest — display manager, graphics packages, and theming all follow automatically.

## Display Managers

Display managers are auto-selected based on your compositor:
- **Niri/Hyprland** use greetd (lightweight, Wayland-native)
- **GNOME** uses GDM (integrated with GNOME)

Override with `useGreetd` or `useGdm` if needed.

## macOS

On macOS, use AeroSpace for tiling window management:

```nix
hostSpecValues = {
  useAerospace = true;
};
```

## Further Reading

- [Niri + Noctalia](niri.md) — the default compositor setup
- [Theming](theming.md) — consistent look across all desktops
