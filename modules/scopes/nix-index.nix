# nix-index-database + comma: command-not-found replacement.
# Provides pre-built weekly nix-index database so `command-not-found` works instantly.
# comma (,) lets you run any package without installing: `, cowsay hello`
{inputs, ...}: {
  flake.modules.nixos.nix-index = {
    config,
    lib,
    ...
  }: let
    hS = config.hostSpec;
  in {
    imports = [inputs.nix-index-database.nixosModules.nix-index];
    config = lib.mkIf (!hS.isMinimal) {
      programs.nix-index.enable = true;
      programs.nix-index-database.comma.enable = true;
      programs.command-not-found.enable = false; # replaced by nix-index
    };
  };

  flake.modules.homeManager.nix-index = {
    config,
    lib,
    ...
  }: let
    hS = config.hostSpec;
  in {
    imports = [inputs.nix-index-database.homeModules.nix-index];
    config = lib.mkIf (!hS.isMinimal) {
      programs.nix-index.enable = true;
    };
  };
}
