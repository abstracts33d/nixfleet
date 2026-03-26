# Enterprise scope: Samba/CIFS file sharing
# Network drive mounts, SMB browsing, autofs on-demand mounting
# Phase 2+: reads network drive list from org defaults
{...}: {
  flake.modules.nixos.enterpriseFilesharing = {
    config,
    pkgs,
    lib,
    ...
  }: let
    hS = config.hostSpec;
  in {
    config = lib.mkIf hS.useFilesharing {
      # Samba client + CIFS mount support
      # TODO: declarative CIFS mounts from org config (fileSystems with cifs type)
      # TODO: credential management via agenix (credentials file per share)
      # TODO: autofs for on-demand mounting (services.autofs)
      # TODO: gvfs for file manager integration (Nautilus/Thunar SMB browsing)
      # TODO: Kerberos authentication for AD-joined shares

      environment.systemPackages = with pkgs; [
        cifs-utils
        samba
      ];

      # Enable gvfs for GUI file manager network browsing
      services.gvfs.enable = lib.mkDefault hS.isGraphical;
    };
  };
}
