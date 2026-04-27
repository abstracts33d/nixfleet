# NixFleet Framework Export
#
# Auto-imported by import-tree. Exposes the framework API.
#
# Exports:
#   flake.lib.nixfleet.mkHost  - the API
#   flake.nixosModules.nixfleet-core - for users who want modules without mkHost
#   flake.diskoTemplates - reusable disk layout templates
#   flakeModules.apps/tests/iso/formatter - for fleet repos (transitional)
{
  inputs,
  lib,
  ...
}: let
  nixfleetLib = import ./_shared/lib/default.nix {inherit inputs lib;};
in {
  options.nixfleet.lib = lib.mkOption {
    type = lib.types.attrs;
    default = nixfleetLib;
    readOnly = true;
    description = "NixFleet library (mkHost)";
  };

  config.flake = {
    # Primary API - nixfleet.lib.mkHost
    lib = nixfleetLib;

    # For consumers who don't want mkHost (just raw modules)
    nixosModules.nixfleet-core = ./core/_nixos.nix;

    # Disk-template data layer absorbed from former nixfleet-scopes.
    # nixfleet's own QEMU test fixtures (modules/_hardware/qemu/) use
    # `btrfs-impermanence`; the rest are exposed for fleet consumers
    # who want a curated starting point.
    diskoTemplates = {
      btrfs = ./disk-templates/btrfs-disk.nix;
      btrfs-bios = ./disk-templates/btrfs-disk-bios.nix;
      btrfs-impermanence = ./disk-templates/btrfs-impermanence-disk.nix;
      btrfs-impermanence-bios = ./disk-templates/btrfs-impermanence-disk-bios.nix;
      ext4 = ./disk-templates/ext4-disk.nix;
      luks-btrfs-impermanence = ./disk-templates/luks-btrfs-impermanence-disk.nix;
    };

    # DEPRECATED — re-export of nixfleet-scopes. Will be removed once
    # consumers (currently only abstracts33d/fleet) switch to importing
    # `nixfleet-scopes` as a direct input. See decoupling plan, phase 3.
    scopes = inputs.nixfleet-scopes.scopes;

    # Transitional flakeModules for fleet repos
    flakeModules = {
      apps = ./apps.nix;
      tests = {
        imports = [
          ./tests/eval.nix
        ];
      };
      iso = ./iso.nix;
      formatter = ./formatter.nix;
    };
  };
}
