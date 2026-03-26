# Niri — scrollable-tiling Wayland compositor (NixOS only).
# Uses programs.niri from nixpkgs for session/DRM/seat integration.
# Noctalia Shell is bundled as the desktop shell (bar, launcher, notifications).
{
  self,
  inputs,
  ...
}: {
  flake.modules.nixos.niri = {
    config,
    pkgs,
    lib,
    ...
  }: let
    hS = config.hostSpec;
    noctalia = self.packages.${pkgs.stdenv.hostPlatform.system}.noctalia;
  in {
    config = lib.mkIf hS.useNiri {
      programs.niri.enable = true;
      security.polkit.enable = true;
      environment.systemPackages = [noctalia];
    };
  };

  flake.modules.homeManager.niri = {
    lib,
    pkgs,
    osConfig,
    ...
  }: let
    hS = osConfig.hostSpec;
    noctalia = self.packages.${osConfig.nixpkgs.hostPlatform.system}.noctalia;
  in {
    config = lib.mkIf hS.useNiri {
      xdg.configFile."niri/config.kdl".source = pkgs.writeText "config.kdl" ''
        spawn-at-startup "${lib.getExe noctalia}"

        input {
          keyboard {
            xkb {
              layout "us"
            }
          }
        }

        layout {
          gaps 5
        }

        binds {
          Mod+Return { spawn "${lib.getExe pkgs.kitty}"; }
          Mod+Q { close-window; }
          Mod+S { spawn-sh "${lib.getExe noctalia} ipc call launcher toggle"; }
        }
      '';
    };
  };

  # Noctalia package (used by the niri scope above)
  perSystem = {pkgs, ...}: {
    packages.noctalia = inputs.wrapper-modules.wrappers.noctalia-shell.wrap {
      inherit pkgs;
    };
  };
}
