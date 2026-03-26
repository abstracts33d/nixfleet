# modules/homebrew.nix
{inputs, ...}: {
  flake.modules.darwin.homebrew = {
    config,
    lib,
    ...
  }: let
    hS = config.hostSpec;
  in {
    imports = [inputs.nix-homebrew.darwinModules.nix-homebrew];

    nix-homebrew = {
      enable = true;
      user = hS.userName;
      taps = {
        "homebrew/homebrew-core" = inputs.homebrew-core;
        "homebrew/homebrew-cask" = inputs.homebrew-cask;
        "homebrew/homebrew-bundle" = inputs.homebrew-bundle;
      };
      mutableTaps = false;
      autoMigrate = true;
    };

    homebrew = lib.mkIf (!hS.isMinimal) {
      enable = true;
      taps = builtins.attrNames config.nix-homebrew.taps;
      brews = []; # Org fills via darwinModules
      casks = []; # Org fills via darwinModules
      masApps = {};
      onActivation = {
        autoUpdate = true;
        cleanup = "zap";
        upgrade = true;
      };
    };
  };
}
