# Qemu VM disk layout for nixfleet's test hosts (isVm = true).
# Uses the co-located btrfs-impermanence disk template. Imports the
# disko NixOS module (mk-host no longer auto-injects it).
{
  inputs,
  lib,
  ...
}: let
  diskConfig =
    import ./disk-template.nix
    {
      inherit lib;
    };
in {
  imports = [inputs.disko.nixosModules.disko];
  disko.devices = diskConfig;
}
