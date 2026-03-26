{...}: {
  flake.modules.nixos.graphical = {
    config,
    pkgs,
    lib,
    ...
  }: let
    hS = config.hostSpec;
  in {
    config = lib.mkIf hS.isGraphical {
      security = {
        rtkit.enable = true;
        pam.services.login.enableGnomeKeyring = true;
      };

      programs = {
        seahorse.enable = true;
        ssh.askPassword = lib.mkForce "${pkgs.seahorse}/libexec/seahorse/ssh-askpass";
        gnupg.agent.pinentryPackage = pkgs.pinentry-gnome3;
      };

      xdg.portal = {
        enable = true;
        wlr.enable = true;
        extraPortals = [pkgs.xdg-desktop-portal-gtk];
        config.common.default = "*";
      };

      services = {
        pipewire = {
          enable = true;
          alsa.enable = true;
          alsa.support32Bit = true;
          pulse.enable = true;
          jack.enable = true;
        };
        libinput.enable = true;
        gvfs.enable = true;
        tumbler.enable = true;
        devmon.enable = true;
        gnome.gnome-keyring.enable = true;
      };

      hardware.graphics.enable = true;

      fonts.packages = with pkgs; [
        nerd-fonts.meslo-lg
        dejavu_fonts
        jetbrains-mono
        font-awesome
        noto-fonts
        noto-fonts-color-emoji
      ];

      environment.systemPackages = with pkgs; [
        libreoffice
        vlc
        pavucontrol
        flameshot
        zathura
      ];
    };
  };

  flake.modules.homeManager.graphicalPersistence = {
    lib,
    osConfig,
    ...
  }: let
    hS = osConfig.hostSpec;
  in {
    config = lib.mkIf (hS.isGraphical && hS.isImpermanent) (lib.optionalAttrs (!hS.isDarwin) {
      home.persistence."/persist".directories = [
        ".config/google-chrome"
        ".config/firefox"
        ".config/BraveSoftware"
        ".config/Code"
        ".config/Slack"
        ".local/share/halloy"
      ];
    });
  };
}
