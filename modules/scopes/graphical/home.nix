{...}: {
  flake.modules.homeManager.graphical = {
    config,
    pkgs,
    lib,
    ...
  }: let
    hS = config.hostSpec;
  in {
    config = lib.mkIf hS.isGraphical {
      programs.firefox.enable = true;
      programs.google-chrome.enable = true;
      programs.chromium = {
        enable = true;
        package = pkgs.brave;
        extensions = [
          {id = "eimadpbcbfnmbkopoojfekhnkhdbieeh";}
          {id = "hipekcciheckooncpjeljhnekcoolahp";}
          {id = "iaiomicjabeggjcfkbimgmglanimpnae";}
        ];
        commandLineArgs = ["--disable-features=AutofillSavePaymentMethods"];
      };
      programs.vscode.enable = true;

      home.packages = with pkgs; [
        asciinema
        halloy
        spotifyd
        ffmpeg
        imagemagick
        neovide
        neomutt
        cmus
        mpd
        hack-font
      ];
    };
  };
}
