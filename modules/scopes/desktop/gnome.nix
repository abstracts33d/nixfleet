{...}: {
  flake.modules.nixos.gnome = {
    config,
    pkgs,
    lib,
    ...
  }: let
    hS = config.hostSpec;
  in {
    config = lib.mkIf hS.useGnome {
      services.xserver = {
        enable = true;
        desktopManager.gnome.enable = true;
      };
      environment.sessionVariables = {
        NIXOS_OZONE_WL = "1";
        QT_WAYLAND_DISABLE_WINDOWDECORATION = "1";
      };
      environment.gnome.excludePackages = with pkgs; [
        gedit
        gnome-connections
        gnome-console
        gnome-photos
        gnome-tour
        snapshot
        atomix
        cheese
        epiphany
        evince
        geary
        gnome-calendar
        gnome-characters
        gnome-clocks
        gnome-contacts
        gnome-initial-setup
        gnome-logs
        gnome-maps
        gnome-music
        gnome-terminal
        gnome-weather
        hitori
        iagno
        simple-scan
        tali
        yelp
      ];
      programs.dconf.enable = true;
      environment.systemPackages = with pkgs; [gnome-tweaks];
      xdg.portal.extraPortals = [pkgs.xdg-desktop-portal-gnome];
    };
  };

  flake.modules.homeManager.gnome = {
    lib,
    osConfig,
    ...
  }: let
    hS = osConfig.hostSpec;
  in {
    config = lib.mkIf (hS.useGnome && hS.isImpermanent) (lib.optionalAttrs (!hS.isDarwin) {
      home.persistence."/persist".directories = [
        ".config/dconf"
        ".local/share/gnome-online-accounts"
      ];
    });
  };
}
