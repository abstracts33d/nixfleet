# modules/core/nixos.nix
{inputs, ...}: {
  flake.modules.nixos.core = {
    config,
    pkgs,
    lib,
    ...
  }: let
    hS = config.hostSpec;
    ifTheyExist = groups: builtins.filter (group: builtins.hasAttr group config.users.groups) groups;
  in {
    imports = [
      inputs.disko.nixosModules.disko
    ];

    # --- nixpkgs ---
    nixpkgs.config = {
      allowUnfree = true;
      allowBroken = false;
      allowInsecure = false;
      allowUnsupportedSystem = true;
    };

    # --- nix settings (from hosts/nixos/common/core/nix.nix) ---
    nix = {
      nixPath = lib.mkDefault [];
      settings = {
        allowed-users = ["${hS.userName}"];
        trusted-users =
          [
            "@admin"
          ]
          ++ lib.optional (!hS.isServer) "${hS.userName}";
        substituters = [
          "https://nix-community.cachix.org"
          "https://cache.nixos.org"
        ];
        trusted-public-keys = [
          "nix-community.cachix.org-1:mB9FSh9qf2dCimDSUo8Zy7bkq5CX+/rkCWyvRCYg3Fs="
          "cache.nixos.org-1:6NCHdD59X431o0gWypbMrAURkbJ16ZPMQFGspcDShjY="
        ];
        auto-optimise-store = true;
      };
      package = pkgs.nix;
      extraOptions = ''
        experimental-features = nix-command flakes
      '';
      gc = {
        automatic = true;
        dates = "weekly";
        options = "--delete-older-than 7d";
      };
    };

    # --- boot (from hosts/nixos/common/core/boot.nix) ---
    boot = {
      loader = {
        systemd-boot = {
          enable = true;
          configurationLimit = 42;
        };
        efi.canTouchEfiVariables = true;
      };
      initrd.availableKernelModules = [
        "xhci_pci"
        "ahci"
        "nvme"
        "usbhid"
        "usb_storage"
        "sd_mod"
      ];
      kernelPackages = pkgs.linuxPackages_latest;
      kernelModules = ["uinput"];
    };

    # --- localization ---
    time.timeZone = hS.timeZone;
    i18n.defaultLocale = hS.locale;
    console.keyMap = lib.mkDefault hS.keyboardLayout;

    # --- networking ---
    networking = {
      hostName = hS.hostName;
      useDHCP = false;
      networkmanager.enable = true;
      firewall.enable = true;
      interfaces = lib.mkIf (hS.networking ? interface) {
        "${hS.networking.interface}".useDHCP = true;
      };
    };

    # --- programs ---
    programs = {
      gnupg.agent = {
        enable = true;
        enableSSHSupport = true;
      };
      dconf.enable = true;
      git.enable = true;
      zsh = {
        enable = true;
        enableCompletion = false;
      };
    };

    # --- security ---
    security = {
      polkit.enable = true;
      sudo = {
        enable = true;
        extraRules = [
          {
            commands = [
              {
                command = "${pkgs.systemd}/bin/reboot";
                options = ["NOPASSWD"];
              }
            ];
            groups = ["wheel"];
          }
        ];
      };
    };

    # --- user ---
    users.users = {
      ${hS.userName} = {
        isNormalUser = true;
        extraGroups = lib.flatten [
          "wheel"
          (ifTheyExist [
            "audio"
            "video"
            "docker"
            "git"
            "networkmanager"
          ])
        ];
        shell = pkgs.zsh;
        openssh.authorizedKeys.keys = hS.sshAuthorizedKeys;
        # When hashedPasswordFile is null (no org password management), the user has
        # no password. Org modules must provide password files or an alternative auth
        # mechanism (SSH keys, initialPassword, etc.)
        hashedPasswordFile = lib.mkIf (hS.hashedPasswordFile != null) hS.hashedPasswordFile;
      };
      root = {
        openssh.authorizedKeys.keys = hS.sshAuthorizedKeys;
        # When hashedPasswordFile is null (no org password management), the user has
        # no password. Org modules must provide password files or an alternative auth
        # mechanism (SSH keys, initialPassword, etc.)
        hashedPasswordFile = lib.mkIf (hS.rootHashedPasswordFile != null) hS.rootHashedPasswordFile;
      };
    };

    # --- services ---
    services = {
      openssh = {
        enable = true;
        settings = {
          PermitRootLogin = "prohibit-password";
          PasswordAuthentication = false;
          KbdInteractiveAuthentication = false;
        };
      };
      printing.enable = false;
      xserver.xkb.layout = lib.mkDefault hS.keyboardLayout;
    };

    # --- hardware ---
    hardware = {
      ledger.enable = true;
    };

    # --- system packages ---
    environment.systemPackages = with pkgs; [
      git
      inetutils
    ];

    # --- Claude Code managed policy (/etc/claude-code/) ---
    # Level 1: Managed policy — non-overridable security floor.
    # These deny rules CANNOT be bypassed by project or user settings.
    # This is kept in the framework (not fleet-specific) because any NixFleet
    # fleet benefits from blocking destructive OS/git/nix commands at the org
    # level. Fleets that don't use Claude Code can ignore this — the file is
    # inert if Claude Code is not installed.
    environment.etc."claude-code/settings.json".text = builtins.toJSON {
      permissions = {
        deny = [
          # Destructive operations
          "Bash(rm -rf *)"
          "Bash(rm -r *)"
          "Bash(dd *)"
          "Bash(mkfs *)"
          "Bash(shred *)"
          # Privilege escalation
          "Bash(sudo *)"
          "Bash(pkexec *)"
          "Bash(doas *)"
          "Bash(su *)"
          # Dangerous git
          "Bash(git push --force *)"
          "Bash(git push -f *)"
          "Bash(git reset --hard *)"
          "Bash(git clean -fd *)"
          # Nix store manipulation
          "Bash(nix-store --delete *)"
          "Bash(nix store delete *)"
        ];
      };
    };

    system.stateVersion = lib.mkDefault "24.11";
  };
}
