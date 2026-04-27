# Qemu VM disk layout for nixfleet's test hosts (isVm = true).
# Uses the btrfs-impermanence disk template absorbed into nixfleet's
# own modules/disk-templates/. Imports the disko NixOS module
# (mk-host no longer auto-injects it).
{
  inputs,
  lib,
  ...
}: let
  diskConfig =
    import ../../disk-templates/btrfs-impermanence-disk.nix
    {
      inherit lib;
    };
in {
  imports = [inputs.disko.nixosModules.disko];
  disko.devices = diskConfig;
}
