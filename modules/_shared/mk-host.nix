# Helper functions to reduce host definition boilerplate.
# Usage: import from host files via `import ../_shared/mk-host.nix { inherit inputs config; }`
{
  inputs,
  config,
}: let
  nixosModules = config.flake.modules.nixos;
  darwinModules = config.flake.modules.darwin;
  hmModules = config.flake.modules.homeManager;
  hmLinuxModules = config.flake.modules.hmLinux;
  hmDarwinModules = config.flake.modules.hmDarwin;
  hostSpecModule = ./host-spec-module.nix;
  backupCmd = ''mv {} {}.nbkp.$(date +%Y%m%d%H%M%S) && ls -t {}.nbkp.* 2>/dev/null | tail -n +6 | xargs -r rm -f'';
  self = {
    mkNixosHost = {
      hostSpecValues,
      platform,
      hardwareModules ? [],
      extraNixosModules ? [],
      extraHmModules ? [],
      stateVersion ? "24.11",
    }:
      inputs.nixpkgs.lib.nixosSystem {
        modules =
          hardwareModules
          ++ [
            {nixpkgs.hostPlatform = platform;}
            hostSpecModule
            {hostSpec = hostSpecValues;}
          ]
          ++ (builtins.attrValues nixosModules)
          ++ extraNixosModules
          ++ [
            inputs.home-manager.nixosModules.home-manager
            {
              home-manager = {
                useGlobalPkgs = true;
                useUserPackages = true;
                backupCommand = backupCmd;
                users.${hostSpecValues.userName} = {
                  imports =
                    [hostSpecModule]
                    ++ (builtins.attrValues hmModules)
                    ++ (builtins.attrValues hmLinuxModules)
                    ++ extraHmModules;
                  hostSpec = hostSpecValues;
                  home = {
                    inherit stateVersion;
                    username = hostSpecValues.userName;
                    homeDirectory = "/home/${hostSpecValues.userName}";
                    enableNixpkgsReleaseCheck = false;
                  };
                  systemd.user.startServices = "sd-switch";
                };
              };
            }
          ];
      };

    # VM host: wraps mkNixosHost with virtio hardware, software rendering, SPICE, global DHCP.
    mkVmHost = {
      hostSpecValues,
      platform ? "x86_64-linux",
      hardwareModules ? [
        ../_hardware/qemu/disk-config.nix
        ../_hardware/qemu/hardware-configuration.nix
      ],
      extraNixosModules ? [],
      extraHmModules ? [],
      stateVersion ? "24.11",
    }:
      self.mkNixosHost {
        inherit hostSpecValues platform stateVersion extraHmModules;
        hardwareModules = hardwareModules;
        extraNixosModules =
          [
            ({
              lib,
              pkgs,
              ...
            }: {
              services.spice-vdagentd.enable = true;
              networking.useDHCP = lib.mkForce true;
              environment.variables.LIBGL_ALWAYS_SOFTWARE = "1";
              environment.systemPackages = [pkgs.mesa];
            })
          ]
          ++ extraNixosModules;
      };

    mkDarwinHost = {
      hostSpecValues,
      platform,
      extraDarwinModules ? [],
      extraHmModules ? [],
      stateVersion ? "23.11",
    }:
      inputs.darwin.lib.darwinSystem {
        modules =
          [
            {nixpkgs.hostPlatform = platform;}
            hostSpecModule
            {hostSpec = hostSpecValues;}
          ]
          ++ (builtins.attrValues darwinModules)
          ++ extraDarwinModules
          ++ [
            inputs.home-manager.darwinModules.home-manager
            {
              home-manager = {
                useGlobalPkgs = true;
                backupCommand = backupCmd;
                users.${hostSpecValues.userName} = {
                  imports =
                    [hostSpecModule]
                    ++ (builtins.attrValues hmModules)
                    ++ (builtins.attrValues hmDarwinModules)
                    ++ extraHmModules;
                  hostSpec = hostSpecValues;
                  home = {
                    inherit stateVersion;
                    username = hostSpecValues.userName;
                    homeDirectory = "/Users/${hostSpecValues.userName}";
                    enableNixpkgsReleaseCheck = false;
                  };
                };
              };
            }
          ];
      };
  };
in
  self
