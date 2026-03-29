# Test helper functions for eval and VM checks.
# Usage: import from eval.nix, vm.nix, or vm-nixfleet.nix
{lib}: {
  # Build a runCommand that prints PASS/FAIL for each assertion and fails on first failure.
  mkEvalCheck = pkgs: name: assertions:
    pkgs.runCommand "eval-test-${name}" {} (
      lib.concatStringsSep "\n" (
        map (a:
          if a.check
          then ''echo "PASS: ${a.msg}"''
          else ''echo "FAIL: ${a.msg}" >&2; exit 1'')
        assertions
      )
      + "\ntouch $out\n"
    );

  # Default hostSpec values for VM test nodes.
  defaultTestSpec = {
    hostName = "testvm";
    userName = "testuser";
    organization = "test";
    isImpermanent = false;
  };

  # Build a nixosTest-compatible node config with stubbed secrets and known passwords.
  # Returns a NixOS module (attrset) — nixosTest handles calling nixosSystem.
  #
  # Parameters:
  #   inputs       — flake inputs (needs home-manager, nixpkgs)
  #   nixosModules    — deferred NixOS modules (builtins.attrValues config.flake.modules.nixos)
  #   hmModules       — deferred HM modules (builtins.attrValues config.flake.modules.homeManager)
  #   hmLinuxModules  — Linux-only HM modules (builtins.attrValues config.flake.modules.hmLinux, default [])
  #   hostSpecModule  — path to the hostSpec module
  #   hostSpecValues  — hostSpec attrset for this test node
  #   extraModules    — additional NixOS modules (default [])
  mkTestNode = {
    inputs,
    nixosModules,
    hmModules,
    hmLinuxModules ? [],
    hostSpecModule,
  }: {
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
                ++ hmModules
                ++ hmLinuxModules;
              hostSpec = hostSpecValues;
              home = {
                stateVersion = "24.11";
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
}
