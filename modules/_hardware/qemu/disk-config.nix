{lib, ...}: let
  diskConfig =
    import ../../_shared/disk-templates/btrfs-impermanence-disk.nix
    {
      inherit lib;
    };
in {
  disko.devices = diskConfig;
}
