# Starship needs HM integration to add `eval "$(starship init zsh)"` to shell rc.
# The wrapped starship package provides the configured binary;
# this HM module provides the shell integration.
{
  pkgs,
  lib,
  ...
}: {
  programs.starship = {
    enable = true;
    settings = lib.mkDefault (pkgs.lib.importTOML ../../_config/starship.toml);
  };
}
