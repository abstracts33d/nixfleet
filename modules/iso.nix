# Custom NixOS minimal ISO with SSH key pre-configured for automated installs.
# Available as `packages.iso` on Linux systems only.
# The SSH key comes from _shared/keys.nix (framework test key by default).
# Fleets should override sshAuthorizedKeys in their org definition; the ISO
# uses the keys.nix file directly since it builds outside the fleet context.
{inputs, ...}: {
  perSystem = {
    system,
    lib,
    ...
  }: let
    isLinux = builtins.elem system ["x86_64-linux" "aarch64-linux"];
    keys = (import ./_shared/keys.nix).sshPublicKeys;
  in
    lib.optionalAttrs isLinux {
      packages.iso = let
        isoSystem = inputs.nixpkgs.lib.nixosSystem {
          modules = [
            "${inputs.nixpkgs}/nixos/modules/installer/cd-dvd/installation-cd-minimal.nix"
            {
              nixpkgs.hostPlatform = system;

              # SSH key for passwordless root access (ISO only)
              users.users.root.openssh.authorizedKeys.keys = keys;
              services.openssh = {
                enable = true;
                settings.PermitRootLogin = "prohibit-password";
              };

              # QEMU guest support
              services.qemuGuest.enable = true;
              services.spice-vdagentd.enable = true;

              # Useful tools for installation
              environment.systemPackages = let
                pkgs = import inputs.nixpkgs {inherit system;};
              in [
                pkgs.git
                pkgs.parted
                pkgs.vim
              ];
            }
          ];
        };
      in
        isoSystem.config.system.build.isoImage;
    };
}
