# Persistence impl: upstream `impermanence` + btrfs root-wipe initrd hook.
#
# Supports BOTH initrd backends:
#   - classic initrd: `boot.initrd.postResumeCommands` shell hook
#   - systemd-initrd: `boot.initrd.systemd.services.impermanence-root-wipe`
#
# The active branch is selected by `config.boot.initrd.systemd.enable`. Hosts
# pairing impermanence with lanzaboote / srvos / measured boot want
# systemd-initrd; hosts on a minimal config can stay classic. Either path
# produces the same on-disk outcome: @root is rotated into old_roots/<ts>
# and a fresh empty @root is created at every boot.
{
  inputs,
  config,
  lib,
  pkgs,
  ...
}: let
  hS = config.hostSpec;
  cfg = config.nixfleet.persistence;
  impl = config.nixfleet.persistence.impermanence;

  # Same root-wipe semantics in both initrd backends.
  #
  # CRITICAL — mountpoint pre-creation: after recreating an empty @root,
  # mount that fresh subvolume and `mkdir` the structural mountpoints the
  # rest of the fileSystems config expects: /nix, /persist, /boot, plus
  # optionally /.swapvol. Without these, sysroot's submounts (e.g.
  # /sysroot/nix.mount for @nix) fail because the target directory does
  # not exist, and the kernel's `init=/nix/store/<sys>/init` path becomes
  # unreachable after switch_root, producing "switch root target contains
  # no usable init" and a stuck initrd boot. Classic initrd's shell
  # stage-1 script created these dirs implicitly while running mount
  # commands; systemd-initrd does not.
  rootWipeScript = ''
    mkdir -p /btrfs_tmp
    mount -o subvol=/ ${impl.rootDevice} /btrfs_tmp
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
    for i in $(find /btrfs_tmp/old_roots/ -maxdepth 1 -mtime +${toString impl.oldRootsRetentionDays}); do
        delete_subvolume_recursively "$i"
    done
    btrfs subvolume create /btrfs_tmp/@root

    # Mount the fresh @root briefly to lay down the mount-point dirs the
    # subsequent sysroot submounts need. See banner comment above for why.
    mkdir -p /btrfs_tmp_root
    mount -o subvol=@root ${impl.rootDevice} /btrfs_tmp_root
    mkdir -p \
      /btrfs_tmp_root/nix \
      /btrfs_tmp_root/persist \
      /btrfs_tmp_root/boot \
      /btrfs_tmp_root/.swapvol
    umount /btrfs_tmp_root
    rmdir /btrfs_tmp_root 2>/dev/null || true

    umount /btrfs_tmp
  '';

  # Translate a /dev path into the systemd .device unit name.
  # systemd unit naming: strip leading '/', then dashes inside path components
  # become '\x2d' and component separators become '-'.
  # e.g. /dev/disk/by-label/root  ->  dev-disk-by\x2dlabel-root.device
  escapeUnit = path: let
    stripped = lib.removePrefix "/" path;
    withHex = lib.replaceStrings ["-"] ["\\x2d"] stripped;
    withDashes = lib.replaceStrings ["/"] ["-"] withHex;
  in
    withDashes;

  rootDeviceUnit = "${escapeUnit impl.rootDevice}.device";
in {
  imports = [inputs.impermanence.nixosModules.impermanence];

  options.nixfleet.persistence.impermanence = {
    rootDevice = lib.mkOption {
      type = lib.types.str;
      default = "/dev/disk/by-label/root";
      description = ''
        Block device holding the btrfs root volume. Mounted briefly
        in the initrd to move @root -> old_roots/<timestamp> and
        create a fresh empty @root. Override if your fleet labels
        the root differently or uses by-uuid / direct device paths.
      '';
    };

    oldRootsRetentionDays = lib.mkOption {
      type = lib.types.int;
      default = 30;
      description = ''
        How many days of old @root snapshots to keep under
        old_roots/ before recursive btrfs delete. Trade disk space
        against rollback window.
      '';
    };
  };

  config = lib.mkIf cfg.enable (lib.mkMerge [
    {
      environment.persistence.${cfg.persistRoot} = {
        directories = cfg.directories;
        files = cfg.files;
      };

      # GOTCHA: pre-create persist home dir with correct ownership so HM bind-mounts succeed; recursive chown on .keys covers rotation.
      system.activationScripts.persistHomeOwnership = lib.mkIf (hS.userName != "") {
        text = ''
          install -d -o ${lib.escapeShellArg hS.userName} -g users ${lib.escapeShellArg "${cfg.persistRoot}/home/${hS.userName}"}
          if [ -d ${lib.escapeShellArg "${cfg.persistRoot}/home/${hS.userName}/.keys"} ]; then
            chown -R ${lib.escapeShellArg hS.userName}:users ${lib.escapeShellArg "${cfg.persistRoot}/home/${hS.userName}/.keys"}
          fi
        '';
        deps = [];
      };

      fileSystems.${cfg.persistRoot}.neededForBoot = true;
    }

    # systemd-initrd branch (modern; required for srvos / lanzaboote / measured boot).
    (lib.mkIf config.boot.initrd.systemd.enable {
      # systemd-initrd ships systemd binaries + a minimal set; everything else
      # we use in the wipe service must be declared explicitly.
      boot.initrd.systemd.storePaths = with pkgs; [
        btrfs-progs
        coreutils
        findutils
        util-linux
      ];

      boot.initrd.systemd.services.impermanence-root-wipe = {
        description = "Wipe root subvolume on boot (impermanence)";
        wantedBy = ["initrd.target"];
        # Wait for udev to settle so the by-label / by-uuid symlink exists.
        requires = [rootDeviceUnit];
        after = [rootDeviceUnit];
        # Run before NixOS bind-mounts the new root at /sysroot.
        before = ["sysroot.mount"];
        unitConfig.DefaultDependencies = "no";
        serviceConfig = {
          Type = "oneshot";
          RemainAfterExit = true;
        };
        script = rootWipeScript;
      };
    })

    # Classic initrd branch (default for minimal configs without systemd-initrd).
    (lib.mkIf (!config.boot.initrd.systemd.enable) {
      # FOOTGUN: btrfs root-wipe - moves @root -> old_roots/<ts> and recreates empty @root every boot; prunes past retention.
      boot.initrd.postResumeCommands = lib.mkAfter rootWipeScript;
    })
  ]);
}
