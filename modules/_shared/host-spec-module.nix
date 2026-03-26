# Specifications For Differentiating Hosts
{
  config,
  lib,
  ...
}: {
  config.hostSpec = lib.mkMerge [
    (lib.mkIf config.hostSpec.isMinimal {
      isGraphical = lib.mkDefault false;
      isDev = lib.mkDefault false;
    })
    (lib.mkIf config.hostSpec.useNiri {
      isGraphical = lib.mkDefault true;
      useGreetd = lib.mkDefault true;
    })
    (lib.mkIf config.hostSpec.useHyprland {
      isGraphical = lib.mkDefault true;
      useGreetd = lib.mkDefault true;
    })
    (lib.mkIf config.hostSpec.useGnome {
      isGraphical = lib.mkDefault true;
      useGdm = lib.mkDefault true;
    })
  ];

  options.hostSpec = {
    # Data variables that don't dictate configuration settings
    userName = lib.mkOption {
      type = lib.types.str;
      description = "The username of the host";
    };
    hostName = lib.mkOption {
      type = lib.types.str;
      description = "The hostname of the host";
    };
    networking = lib.mkOption {
      default = {};
      type = lib.types.attrsOf lib.types.anything;
      description = "An attribute set of networking information";
    };
    githubUser = lib.mkOption {
      type = lib.types.str;
      description = "The handle of the user";
    };
    githubEmail = lib.mkOption {
      type = lib.types.str;
      description = "The email of the user";
    };

    # NixFleet framework options
    organization = lib.mkOption {
      type = lib.types.str;
      description = "Organization this host belongs to (set by mkFleet)";
    };
    role = lib.mkOption {
      type = lib.types.nullOr lib.types.str;
      default = null;
      description = "Named role within the organization (optional)";
    };
    secretsPath = lib.mkOption {
      type = lib.types.nullOr lib.types.str;
      default = null;
      description = "Hint for secrets repo path. Framework-agnostic — no agenix coupling.";
    };
    timeZone = lib.mkOption {
      type = lib.types.str;
      default = "UTC";
      description = "IANA timezone (e.g. Europe/Paris)";
    };
    locale = lib.mkOption {
      type = lib.types.str;
      default = "en_US.UTF-8";
      description = "System locale";
    };
    keyboardLayout = lib.mkOption {
      type = lib.types.str;
      default = "us";
      description = "XKB keyboard layout";
    };
    gpgSigningKey = lib.mkOption {
      type = lib.types.nullOr lib.types.str;
      default = null;
      description = "GPG key fingerprint for git commit signing. Null disables signing.";
    };
    sshAuthorizedKeys = lib.mkOption {
      type = lib.types.listOf lib.types.str;
      default = [];
      description = "SSH public keys for authorized_keys (primary user and root).";
    };

    home = lib.mkOption {
      type = lib.types.str;
      description = "The home directory of the user";
      default = let
        hS = config.hostSpec;
      in
        if hS.isDarwin
        then "/Users/${hS.userName}"
        else "/home/${hS.userName}";
    };

    # Configuration Settings
    isMinimal = lib.mkOption {
      type = lib.types.bool;
      default = false;
      description = "Used to indicate a minimal host";
    };
    isServer = lib.mkOption {
      type = lib.types.bool;
      default = false;
      description = "Used to indicate a server host";
    };
    isDarwin = lib.mkOption {
      type = lib.types.bool;
      default = false;
      description = "Used to indicate a host that is darwin";
    };
    isImpermanent = lib.mkOption {
      type = lib.types.bool;
      default = false;
      description = "Used to indicate an impermanent host";
    };
    isDev = lib.mkOption {
      type = lib.types.bool;
      default = true;
      description = "Used to indicate a development host";
    };
    isGraphical = lib.mkOption {
      type = lib.types.bool;
      default = true;
      description = "Used to indicate a host that is graphical";
    };
    useGnome = lib.mkOption {
      type = lib.types.bool;
      default = false;
      description = "Used to indicate a host that uses a Gnome";
    };
    useHyprland = lib.mkOption {
      type = lib.types.bool;
      default = false;
      description = "Used to indicate a host that uses Hyprland";
    };
    useNiri = lib.mkOption {
      type = lib.types.bool;
      default = false;
      description = "Used to indicate a host that uses Niri";
    };
    useGdm = lib.mkOption {
      type = lib.types.bool;
      default = false;
      description = "Used to indicate a host that uses a GDM";
    };
    useGreetd = lib.mkOption {
      type = lib.types.bool;
      default = false;
      description = "Used to indicate a host that uses a Greetd";
    };
    useAerospace = lib.mkOption {
      type = lib.types.bool;
      default = false;
      description = "Used to indicate a host that uses a aerospace";
    };

    # Hardware Configuration Settings
    hasBluetooth = lib.mkOption {
      type = lib.types.bool;
      default = false;
      description = "Used to indicate a host that has bluetooth capabilities";
    };
    useSecureBoot = lib.mkOption {
      type = lib.types.bool;
      default = false;
      description = "Used to indicate a host that uses Secure Boot (lanzaboote)";
    };
    hashedPasswordFile = lib.mkOption {
      type = lib.types.nullOr lib.types.str;
      default = null;
      description = "Path to hashed password file for primary user. Null = no managed password.";
    };
    rootHashedPasswordFile = lib.mkOption {
      type = lib.types.nullOr lib.types.str;
      default = null;
      description = "Path to hashed password file for root. Null = no managed password.";
    };
    wifiNetworks = lib.mkOption {
      type = lib.types.listOf lib.types.str;
      default = [];
      description = "List of WiFi network secret names to bootstrap (must exist in nix-secrets as wifi-<name>.age)";
    };

    theme = {
      flavor = lib.mkOption {
        type = lib.types.str;
        default = "macchiato";
        description = "Catppuccin flavor (latte, frappe, macchiato, mocha).";
      };
      accent = lib.mkOption {
        type = lib.types.str;
        default = "lavender";
        description = "Catppuccin accent color.";
      };
    };

    # Enterprise Features
    useVpn = lib.mkOption {
      type = lib.types.bool;
      default = false;
      description = "Corporate VPN client (WireGuard/OpenVPN)";
    };
    useFilesharing = lib.mkOption {
      type = lib.types.bool;
      default = false;
      description = "Samba/CIFS file sharing and network drives";
    };
    useLdap = lib.mkOption {
      type = lib.types.bool;
      default = false;
      description = "LDAP/AD authentication (sssd/PAM)";
    };
    usePrinting = lib.mkOption {
      type = lib.types.bool;
      default = false;
      description = "Network printing (CUPS + auto-discovery)";
    };
    useCorporateCerts = lib.mkOption {
      type = lib.types.bool;
      default = false;
      description = "Corporate CA trust and client certificate management";
    };
    useProxy = lib.mkOption {
      type = lib.types.bool;
      default = false;
      description = "System-wide HTTP/HTTPS proxy configuration";
    };
  };
}
