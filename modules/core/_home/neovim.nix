# NOTE(phase2): neovim config reads from _config/nvim/ which is org-specific.
# Complex structured data — not wrapped with mkDefault here.
# Will move to org overlay in Phase 2 of the NixFleet framework decontamination.
{config, ...}: let
  hS = config.hostSpec;
in {
  programs.neovim.enable = true;
  home.file."${hS.home}/.config/nvim/" = {
    source = ../../_config/nvim;
    recursive = true;
  };
}
