{...}: {
  flake.modules.nixos.hyprland = {
    config,
    pkgs,
    lib,
    ...
  }: let
    hS = config.hostSpec;
  in {
    config = lib.mkIf hS.useHyprland {
      programs.hyprland = {
        enable = true;
        xwayland.enable = true;
        withUWSM = true;
      };
      environment.sessionVariables = {
        NIXOS_OZONE_WL = "1";
        QT_WAYLAND_DISABLE_WINDOWDECORATION = "1";
      };
      environment.systemPackages = with pkgs; [
        file-roller
        nautilus
        totem
        brightnessctl
        networkmanagerapplet
        pavucontrol
        wf-recorder
      ];
      xdg.portal.extraPortals = [pkgs.xdg-desktop-portal-hyprland];
    };
  };

  flake.modules.homeManager.hyprland = {
    osConfig,
    pkgs,
    lib,
    ...
  }: let
    hS = osConfig.hostSpec;
  in {
    config = lib.mkIf hS.useHyprland {
      xdg.enable = true;
      wayland.windowManager.hyprland = {
        enable = true;
        settings = {
          "$mod" = "SUPER";
          "$terminal" = "kitty";
          "$browser" = "firefox";
          "$launcher" = "tofi-drun";
          "$launcher_alt" = "tofi-run";
          "$launcher2" = "wofi --show drun -n";
          "$launcher2_alt" = "wofi --show run -n";
          "$editor" = "code";
          bind = [
            "$mod, return, exec, $terminal"
            "$mod, a, exec, $launcher"
            "$mod, s, exec, $launcher_alt"
            "$mod, d, exec, $launcher2"
            "$mod, f, exec, $launcher2_alt"
            "$mod SHIFT, q, killactive"
            "$mod SHIFT, e, exit"
            "$mod SHIFT, l, exec, ${pkgs.hyprlock}/bin/hyprlock"
          ];
        };
      };
      programs.hyprlock.enable = true;
      programs.waybar = {
        enable = true;
        systemd.enable = true;
      };
      programs.wofi.enable = true;
      programs.tofi.enable = true;
      programs.wlogout.enable = true;
    };
  };
}
