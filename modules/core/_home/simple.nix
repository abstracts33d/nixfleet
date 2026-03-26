# HM-managed programs — catppuccin auto-themes these.
# Portable versions live in wrappers/shell.nix for `nix run .#shell`.
{
  config,
  pkgs,
  lib,
  ...
}: let
  hS = config.hostSpec;
in {
  programs.bat = {
    enable = true;
    config.style = "changes,header";
    extraPackages = builtins.attrValues {
      inherit (pkgs.bat-extras) batgrep batdiff batman;
    };
  };
  programs.btop.enable = true;
  programs.kitty = lib.mkIf (!hS.isMinimal) {
    enable = true;
    extraConfig = lib.mkDefault (builtins.readFile ../../_config/kitty.conf);
  };
  programs.alacritty = lib.mkIf (!hS.isMinimal) {
    enable = true;
    settings = {
      cursor.style = "Block";
      window.opacity = lib.mkForce 0.8;
      font.normal.family = lib.mkForce "MesloLGS NF";
    };
  };
  programs.helix.enable = true;
  programs.yazi = {
    enable = true;
    shellWrapperName = "y";
  };
  programs.zellij = {
    enable = true;
    enableBashIntegration = false;
    enableZshIntegration = false;
  };
  programs.zoxide = {
    enable = true;
    enableBashIntegration = true;
    enableZshIntegration = true;
  };
}
