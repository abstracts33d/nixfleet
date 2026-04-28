# Persistence implementation: upstream `impermanence` flake +
# btrfs root-wipe initrd hook.
#
# Implements the framework-declared `nixfleet.persistence.*` schema
# from `nixfleet/modules/contracts/persistence.nix`. Reads the
# accumulated `directories` and `files` lists (framework baseline +
# scope/fleet contributions) and applies them via the upstream
# `impermanence` NixOS module.
#
# The btrfs wipe-on-boot model: at every boot, the root subvolume
# `@root` is moved to `old_roots/<timestamp>` and a fresh empty
# `@root` is created. State that should survive lives on the
# `persistRoot` btrfs subvolume; the impermanence module bind-mounts
# the listed paths back into the wiped root before activation.
#
# Alternative implementations (ZFS rollback, snapper, none) are
# sibling files in this directory or future ones. Each reads the
# same `nixfleet.persistence.*` schema; fleets pick exactly one.
{
  inputs,
  config,
  lib,
  ...
}: let
  hS = config.hostSpec;
  cfg = config.nixfleet.persistence;
in {
  imports = [inputs.impermanence.nixosModules.impermanence];

  config = lib.mkIf cfg.enable {
    environment.persistence.${cfg.persistRoot} = {
      directories = cfg.directories;
      files = cfg.files;
    };

    # Ensure the persist tree's home directory exists with the right
    # ownership so the agent + HM bind-mounts succeed. The .keys
    # subdirectory is the agenix decryption target — flag-recursive
    # chown so rotation drops new files in place with the right uid.
    system.activationScripts.persistHomeOwnership = lib.mkIf (hS.userName != "") {
      text = ''
        install -d -o ${lib.escapeShellArg hS.userName} -g users ${lib.escapeShellArg "${cfg.persistRoot}/home/${hS.userName}"}
        if [ -d ${lib.escapeShellArg "${cfg.persistRoot}/home/${hS.userName}/.keys"} ]; then
          chown -R ${lib.escapeShellArg hS.userName}:users ${lib.escapeShellArg "${cfg.persistRoot}/home/${hS.userName}/.keys"}
        fi
      '';
      deps = [];
    };

    # Btrfs root-wipe: every boot, move the active root subvol to
    # old_roots/<timestamp> and create a fresh empty @root. State
    # survives only via the persisted bind-mounts above. Old roots
    # past 30 days are recursively deleted.
    boot.initrd.postResumeCommands = lib.mkAfter ''
      mkdir /btrfs_tmp
      mount /dev/disk/by-label/root /btrfs_tmp
      if [[ -e /btrfs_tmp/@root ]]; then
          mkdir -p /btrfs_tmp/old_roots
          timestamp=$(date --date="@$(stat -c %Y /btrfs_tmp/@root)" "+%Y-%m-%-d_%H:%M:%S")
          mv /btrfs_tmp/@root "/btrfs_tmp/old_roots/$timestamp"
      fi
      delete_subvolume_recursively() {
          IFS=$'\n'
          for i in $(btrfs subvolume list -o "$1" | cut -f 9- -d ' '); do
              delete_subvolume_recursively "/btrfs_tmp/$i"
          done
          btrfs subvolume delete "$1"
      }
      for i in $(find /btrfs_tmp/old_roots/ -maxdepth 1 -mtime +30); do
          delete_subvolume_recursively "$i"
      done
      btrfs subvolume create /btrfs_tmp/@root
      umount /btrfs_tmp
    '';

    fileSystems.${cfg.persistRoot}.neededForBoot = true;
  };
}
