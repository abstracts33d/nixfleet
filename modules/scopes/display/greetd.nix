{...}: {
  flake.modules.nixos.greetd = {
    config,
    pkgs,
    lib,
    ...
  }: let
    hS = config.hostSpec;
    tuigreet = "${pkgs.greetd.tuigreet}/bin/tuigreet";
    sessionCmd =
      if hS.useNiri
      then "niri"
      else if hS.useHyprland
      then "Hyprland"
      else "bash";
  in {
    config = lib.mkIf hS.useGreetd {
      services.greetd = {
        enable = true;
        settings.default_session = {
          command = "${tuigreet} --time --remember --cmd ${sessionCmd}";
          user = "greeter";
        };
      };
      security.pam.services.greetd.enableGnomeKeyring = true;
    };
  };
}
