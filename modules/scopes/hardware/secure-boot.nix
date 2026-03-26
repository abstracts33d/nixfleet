# Lanzaboote: Secure Boot for NixOS.
# Enable per host with useSecureBoot = true in hostSpecValues.
# Requires initial setup: https://github.com/nix-community/lanzaboote/blob/main/docs/QUICK_START.md
{inputs, ...}: {
  flake.modules.nixos.secure-boot = {
    config,
    pkgs,
    lib,
    ...
  }: let
    hS = config.hostSpec;
  in {
    imports = [inputs.lanzaboote.nixosModules.lanzaboote];
    config = lib.mkIf hS.useSecureBoot {
      environment.systemPackages = [pkgs.sbctl];

      # Lanzaboote replaces systemd-boot
      boot.loader.systemd-boot.enable = lib.mkForce false;
      boot.lanzaboote = {
        enable = true;
        pkiBundle = "/etc/secureboot";
      };

      # --- Impermanence: persist secure boot keys ---
      environment.persistence."/persist/system".directories = lib.mkIf hS.isImpermanent [
        "/etc/secureboot"
      ];
    };
  };
}
