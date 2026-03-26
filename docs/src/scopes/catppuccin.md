# catppuccin

## Purpose

Consistent Catppuccin Macchiato theming with lavender accent across all tools. Applied via both NixOS and HM modules from the catppuccin/nix input. Themes bat, btop, kitty, GTK, and many more apps automatically.

## Location

- `modules/scopes/catppuccin.nix`

## Configuration

**Gate:** `!isMinimal`

```nix
catppuccin = {
  enable = true;
  flavor = hS.theme.flavor;   # hostSpec default: "macchiato"
  accent = hS.theme.accent;   # hostSpec default: "lavender"
};
```

Theme values are read from `hostSpec.theme.flavor` and `hostSpec.theme.accent`, so they can be overridden per-org, per-role, or per-host via `hostSpecDefaults`/`hostSpecValues`.

Registered as both `flake.modules.nixos.catppuccin` and `flake.modules.homeManager.catppuccin`.

## Platform Notes

- **NixOS:** Uses `catppuccin.nixosModules.catppuccin`
- **HM (all platforms):** Uses `catppuccin.homeModules.catppuccin`
- **Darwin:** No `darwinModules` available in catppuccin/nix. Theming comes exclusively through the HM module.

## Dependencies

- Input: `catppuccin` (github:catppuccin/nix)
- Depends on: hostSpec `isMinimal` flag

## Links

- [Scope Overview](README.md)
