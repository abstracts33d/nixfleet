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

        # --- vm-shell-hm — Moved to fleet (HM programs are fleet-specific) ---
        # vm-shell-hm: starship, nvim, tmux, fzf, eza, rg, bat — all from core/_home/

        # --- vm-graphical — Moved to fleet (scopes are fleet-specific) ---
        # vm-graphical: greetd, niri, kitty, pipewire, fonts

        # --- vm-minimal: negative test (core only, no scopes) ---
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

            # Core always present (from core/nixos.nix)
            machine.succeed("su - testuser -c 'which zsh'")
            machine.succeed("su - testuser -c 'which git'")

            # No graphical (no scope modules in framework)
            machine.fail("which niri")

            # Docker should not be present (no dev scope in framework)
            machine.fail("systemctl is-active docker")
          '';
        };
      };
    };
}
