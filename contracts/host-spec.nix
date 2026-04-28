# hostSpec - identity carrier for every host.
#
# Framework-level options only - scope/role/profile/hardware concerns
# live elsewhere:
# - `nixfleet.<scope>.*` options for contract impls come from
#   `flake.scopes.*` (this repo's `impls/`).
# - Service options, role bundles, hardware, and `fleet.*` options
#   come from the consuming fleet.
#
# Posture flags (`isImpermanent`, `isServer`, `isMinimal`) that lived
# here in earlier revisions have been removed — their roles are played
# by per-scope `enable` options set in fleet-side role bundles.
{
  config,
  lib,
  ...
}: {
  options.hostSpec = {
    # --- Identity ---
    hostName = lib.mkOption {
      type = lib.types.str;
      description = "The hostname of the host";
    };
    userName = lib.mkOption {
      type = lib.types.str;
      description = ''
        Primary user name on this host. Set explicitly, or populated by
        a fleet-side operators scope from its own
        `nixfleet.operators.primaryUser` (or equivalent) namespace.
      '';
    };
    home = lib.mkOption {
      type = lib.types.str;
      description = "The home directory of the primary user";
      default = let
        hS = config.hostSpec;
      in
        if hS.isDarwin
        then "/Users/${hS.userName}"
        else "/home/${hS.userName}";
      defaultText = lib.literalExpression ''
        if config.hostSpec.isDarwin
        then "/Users/''${config.hostSpec.userName}"
        else "/home/''${config.hostSpec.userName}"
      '';
    };

    # --- Locale / keyboard ---
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

    # --- Access ---
    rootHashedPasswordFile = lib.mkOption {
      type = lib.types.nullOr lib.types.str;
      default = null;
      description = "Path to hashed password file for root. Null = no managed password.";
    };

    rootSshKeys = lib.mkOption {
      type = lib.types.listOf lib.types.str;
      default = [];
      description = ''
        SSH public keys authorized for root login. Empty list = no
        managed root keys. Populated either by hand or automatically
        by a fleet-side operators scope from its own
        `nixfleet.operators.rootSshKeys` (or equivalent) namespace.
      '';
    };

    # --- Networking ---
    networking = lib.mkOption {
      default = {};
      type = lib.types.attrsOf lib.types.anything;
      description = "An attribute set of networking information (e.g. `interface` hint for DHCP).";
    };

    # --- Secrets backend hint (backend-agnostic) ---
    secretsPath = lib.mkOption {
      type = lib.types.nullOr lib.types.str;
      default = null;
      description = "Hint for secrets repo path. Framework-agnostic - no agenix coupling.";
    };

    # --- Platform ---
    isDarwin = lib.mkOption {
      type = lib.types.bool;
      default = false;
      description = "Whether this host runs nix-darwin. Set automatically by mkHost for Darwin platforms.";
    };
  };
}
