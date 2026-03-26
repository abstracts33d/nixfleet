# Catppuccin theming — consistent colors across shell and graphical apps.
# Activates on all non-minimal hosts (themes bat, btop, fish, kitty, gtk, etc.)
# https://github.com/catppuccin/nix
{inputs, ...}: {
  flake.modules.nixos.catppuccin = {
    config,
    lib,
    ...
  }: let
    hS = config.hostSpec;
  in {
    imports = [inputs.catppuccin.nixosModules.catppuccin];
    config = lib.mkIf (!hS.isMinimal) {
      catppuccin = {
        enable = true;
        flavor = hS.theme.flavor;
        accent = hS.theme.accent;
      };
    };
  };

  flake.modules.homeManager.catppuccin = {
    config,
    lib,
    ...
  }: let
    hS = config.hostSpec;
  in {
    imports = [inputs.catppuccin.homeModules.catppuccin];
    config = lib.mkIf (!hS.isMinimal) {
      catppuccin = {
        enable = true;
        flavor = hS.theme.flavor;
        accent = hS.theme.accent;
      };
    };
  };

  # Darwin: catppuccin only works via homeModules (no darwinModules available).
  # The HM module above handles Darwin theming.
}
