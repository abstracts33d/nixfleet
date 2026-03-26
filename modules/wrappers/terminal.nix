# Portable terminal — `nix run .#terminal` from any machine.
# Kitty wrapping the portable shell environment.
# Config from _config/kitty.conf (same source as HM).
{inputs, ...}: let
  wlib = inputs.wrapper-modules.lib;
  terminalWrapper = wlib.wrapModule ({
    pkgs,
    config,
    ...
  }: {
    package = pkgs.kitty;
    constructFiles.kitty_config = {
      content = builtins.readFile ../_config/kitty.conf;
      relPath = "kitty.conf";
    };
    flags."--config" = config.constructFiles.kitty_config.path;
  });
in {
  perSystem = {
    pkgs,
    self',
    lib,
    ...
  }: {
    packages.terminal = let
      wrapped-kitty = terminalWrapper.wrap {inherit pkgs;};
    in
      # Wrap kitty to launch the portable shell by default
      pkgs.writeShellScriptBin "terminal" ''
        exec ${lib.getExe wrapped-kitty} -e ${lib.getExe self'.packages.shell} "$@"
      '';
  };
}
