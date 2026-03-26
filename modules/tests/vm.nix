# Tier A — VM integration tests: boot a NixOS VM and assert runtime state.
# Only runs on x86_64-linux. Gated behind `nix run .#validate -- --vm`.
# Each test is a `pkgs.testers.nixosTest` that boots a VM and runs a Python test script.
{
  inputs,
  config,
  ...
}: {
  perSystem = {
    pkgs,
    system,
    lib,
    ...
  }: let
    # Collect all deferred modules (same as mkNixosHost uses)
    nixosModules = builtins.attrValues config.flake.modules.nixos;
    hmModules = builtins.attrValues config.flake.modules.homeManager;
    hostSpecModule = ../_shared/host-spec-module.nix;

    # Build a nixosTest-compatible node config with stubbed secrets and known passwords.
    # Returns a NixOS module (attrset) — nixosTest handles calling nixosSystem.
    mkTestNode = {
      hostSpecValues,
      extraModules ? [],
    }: {
      imports =
        [
          hostSpecModule
          {hostSpec = hostSpecValues;}
        ]
        ++ nixosModules
        ++ [
          inputs.home-manager.nixosModules.home-manager
          {
            # --- Test user with known password ---
            users.users.${hostSpecValues.userName} = {
              hashedPasswordFile = lib.mkForce null;
              password = lib.mkForce "test";
            };
            users.users.root = {
              hashedPasswordFile = lib.mkForce null;
              password = lib.mkForce "test";
            };

            # --- Handle nixpkgs for test nodes ---
            # nixosTest injects pkgs externally, but our core/nixos.nix sets nixpkgs.config
            # which triggers an assertion. Override with a pkgs instance that has allowUnfree.
            nixpkgs.pkgs = lib.mkForce (import inputs.nixpkgs {
              system = "x86_64-linux";
              config = {
                allowUnfree = true;
                allowBroken = false;
                allowInsecure = false;
                allowUnsupportedSystem = true;
              };
            });
            nixpkgs.config = lib.mkForce {};
            nixpkgs.hostPlatform = lib.mkForce "x86_64-linux";

            # --- HM config for the test user ---
            home-manager = {
              useGlobalPkgs = true;
              useUserPackages = true;
              users.${hostSpecValues.userName} = {
                imports =
                  [hostSpecModule]
                  ++ hmModules;
                hostSpec = hostSpecValues;
                home = {
                  stateVersion = "21.05";
                  username = hostSpecValues.userName;
                  homeDirectory = "/home/${hostSpecValues.userName}";
                  enableNixpkgsReleaseCheck = false;
                };
                systemd.user.startServices = "sd-switch";
              };
            };
          }
        ]
        ++ extraModules;
    };

    # Default hostSpec values for test nodes
    defaultTestSpec = {
      hostName = "testvm";
      userName = "testuser";
      githubUser = "test";
      githubEmail = "test@test.com";
      organization = "test";
      isImpermanent = false;
    };
  in
    lib.optionalAttrs (system == "x86_64-linux") {
      checks = {
        # --- vm-core: multi-user, SSH, NetworkManager, firewall, user/groups ---
        vm-core = pkgs.testers.nixosTest {
          name = "vm-core";
          nodes.machine = mkTestNode {
            hostSpecValues =
              defaultTestSpec
              // {
                isGraphical = false;
                isDev = false;
              };
          };
          testScript = ''
            machine.wait_for_unit("multi-user.target")
            machine.wait_for_unit("sshd")
            machine.wait_for_unit("NetworkManager")
            machine.succeed("iptables -L | grep -q 'Chain INPUT'")
            machine.succeed("id testuser")
            machine.succeed("groups testuser | grep -q wheel")
            machine.succeed("su - testuser -c 'which zsh'")
            machine.succeed("su - testuser -c 'which git'")
          '';
        };

        # --- vm-shell-hm: HM activation, zsh, git, starship, nvim, tmux, fzf ---
        vm-shell-hm = pkgs.testers.nixosTest {
          name = "vm-shell-hm";
          nodes.machine = mkTestNode {
            hostSpecValues =
              defaultTestSpec
              // {
                isGraphical = false;
                isDev = false;
              };
          };
          testScript = ''
            machine.wait_for_unit("multi-user.target")

            # Wait for HM activation to complete
            machine.wait_for_unit("home-manager-testuser.service")

            # Core HM programs
            machine.succeed("su - testuser -c 'test -e ~/.config/starship.toml'")
            machine.succeed("su - testuser -c 'which starship'")
            machine.succeed("su - testuser -c 'which nvim'")
            machine.succeed("su - testuser -c 'which tmux'")
            machine.succeed("su - testuser -c 'which fzf'")
            machine.succeed("su - testuser -c 'which eza'")
            machine.succeed("su - testuser -c 'which rg'")
            machine.succeed("su - testuser -c 'which bat'")
            machine.succeed("su - testuser -c 'git config user.name'")
          '';
        };

        # --- vm-graphical: greetd, niri, kitty, pipewire, fonts, niri config ---
        vm-graphical = pkgs.testers.nixosTest {
          name = "vm-graphical";
          nodes.machine = mkTestNode {
            hostSpecValues =
              defaultTestSpec
              // {
                useNiri = true;
                # useNiri implies isGraphical + useGreetd via host-spec-module
              };
          };
          testScript = ''
            machine.wait_for_unit("multi-user.target")
            machine.wait_for_unit("greetd")
            machine.succeed("which niri")
            # tuigreet is referenced by greetd config, verify it exists in the store
            machine.succeed("find /nix/store -name tuigreet -type f 2>/dev/null | head -1 | grep -q tuigreet")

            # Wait for HM activation
            machine.wait_for_unit("home-manager-testuser.service")

            machine.succeed("su - testuser -c 'which kitty'")
            machine.succeed("su - testuser -c 'test -f ~/.config/niri/config.kdl'")

            # Pipewire may not fully start without audio hardware, but the unit should exist
            machine.succeed("systemctl list-unit-files | grep -q pipewire")

            # Fonts installed
            machine.succeed("fc-list | grep -qi meslo")
          '';
        };

        # --- vm-minimal: negative test (no graphical, no dev, no docker) ---
        vm-minimal = pkgs.testers.nixosTest {
          name = "vm-minimal";
          nodes.machine = mkTestNode {
            hostSpecValues =
              defaultTestSpec
              // {
                isMinimal = true;
                # isMinimal implies isGraphical = false, isDev = false
              };
          };
          testScript = ''
            machine.wait_for_unit("multi-user.target")

            # Core always present
            machine.succeed("su - testuser -c 'which zsh'")

            # No graphical
            machine.fail("which niri")

            # Wait for HM activation
            machine.wait_for_unit("home-manager-testuser.service")

            # Note: kitty is in core HM (simple.nix), so it's present even on minimal
            # Verify no graphical HM apps like firefox/chrome
            machine.fail("su - testuser -c 'which firefox'")

            # Docker should not be running (isDev = false via isMinimal)
            machine.fail("systemctl is-active docker")
          '';
        };
      };
    };
}
