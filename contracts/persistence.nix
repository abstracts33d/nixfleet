# Framework-level persistence schema.
#
# Pure schema, no implementation. Auto-imported by mkHost so that
# nixfleet's own service modules (agent, control-plane, microvm-host)
# can contribute persisted-directory paths via
# `nixfleet.persistence.directories = lib.mkIf cfg.enable [ ... ]`
# without each having to know which persistence implementation a
# fleet uses (impermanence-flake-based wipe-on-boot, ZFS rollback,
# snapper-driven snapshots, or none).
#
# Implementations live in `impls/persistence/*` (this repo) and are
# exposed at `flake.scopes.persistence.<impl>`. A persistence-aware
# fleet imports exactly one of them; the impl reads
# `config.nixfleet.persistence.{enable, persistRoot, directories,
# files}` and translates to whatever its mechanism requires (e.g.
# `environment.persistence."/persist".directories = ...` for the
# impermanence impl).
#
# Schema lives here in `contracts/`; the impermanence implementation
# lives in `impls/`. Fleets that don't enable the impermanence impl
# pay no runtime cost from the upstream `inputs.impermanence` input.
#
# Home Manager-side persistence remains a separate concern — fleets
# wire it via their own HM module imports.
{
  config,
  lib,
  ...
}: let
  cfg = config.nixfleet.persistence;
in {
  options.nixfleet.persistence = {
    enable = lib.mkEnableOption ''
      NixFleet system-level persistence. Activates the persistence
      implementation imported by the consumer (e.g.
      `inputs.nixfleet.scopes.persistence.impermanence`). Without
      that import, this option toggles only the schema — the
      framework's own service-module contributions to
      `nixfleet.persistence.directories` are silently merged but
      nothing materialises them.
    '';

    persistRoot = lib.mkOption {
      type = lib.types.str;
      default = "/persist";
      description = "Mount point where the implementation persists state.";
    };

    directories = lib.mkOption {
      type = lib.types.listOf (lib.types.either lib.types.str (lib.types.attrsOf lib.types.anything));
      default = [];
      description = ''
        Directories that must survive across reboots. Framework
        modules contribute baselines (the agent's state, the
        control-plane's state, microvm-host registry); fleets and
        scopes append app-specific paths. The active persistence
        implementation reads this list and applies its mechanism
        (impermanence bind-mounts, ZFS subvol layout, etc.).

        Entries may be plain strings (just the path) or attrsets
        carrying additional metadata the impl can use:

            { directory = "/var/lib/forgejo"; user = "forgejo";
              group = "forgejo"; mode = "0750"; }

        Plain strings are the common case. The impl scope is
        responsible for understanding any richer shape; the
        framework forwards the list opaquely.
      '';
      example = ["/var/lib/nixfleet" "/etc/nixos"];
    };

    files = lib.mkOption {
      type = lib.types.listOf (lib.types.either lib.types.str (lib.types.attrsOf lib.types.anything));
      default = [];
      description = ''
        Individual files that must survive across reboots.
        Counterpart to `directories` for impl mechanisms (notably
        impermanence) that distinguish file-level from
        directory-level persistence. Same string-or-attrset
        shape as `directories`.
      '';
      example = ["/etc/machine-id"];
    };
  };

  # Framework-baseline contributions — every fleet that enables a
  # persistence implementation should keep system identity and logs
  # across reboots regardless of what else they pick to persist.
  config = lib.mkIf cfg.enable {
    nixfleet.persistence.directories = [
      "/etc/nixos"
      "/etc/NetworkManager/system-connections"
      "/var/lib/systemd"
      "/var/lib/nixos"
      "/var/log"
    ];
    nixfleet.persistence.files = ["/etc/machine-id"];
  };
}
