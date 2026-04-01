# NixOS service module for the NixFleet fleet agent.
# Auto-included by mkHost (disabled by default).
{
  config,
  lib,
  pkgs,
  ...
}: let
  cfg = config.services.nixfleet-agent;
  nixfleet-agent = pkgs.callPackage ../../../agent {};
in {
  options.services.nixfleet-agent = {
    enable = lib.mkEnableOption "NixFleet fleet management agent";

    controlPlaneUrl = lib.mkOption {
      type = lib.types.str;
      example = "https://fleet.example.com";
      description = "URL of the NixFleet control plane.";
    };

    machineId = lib.mkOption {
      type = lib.types.str;
      default = config.networking.hostName;
      defaultText = lib.literalExpression "config.networking.hostName";
      description = "Machine identifier reported to the control plane.";
    };

    pollInterval = lib.mkOption {
      type = lib.types.int;
      default = 300;
      description = "Poll interval in seconds.";
    };

    cacheUrl = lib.mkOption {
      type = lib.types.nullOr lib.types.str;
      default = null;
      example = "https://cache.fleet.example.com";
      description = "Binary cache URL for nix copy --from. Falls back to control plane default.";
    };

    dbPath = lib.mkOption {
      type = lib.types.str;
      default = "/var/lib/nixfleet/state.db";
      description = "Path to the SQLite state database.";
    };

    dryRun = lib.mkOption {
      type = lib.types.bool;
      default = false;
      description = "When true, check and fetch but do not apply generations.";
    };
  };

  config = lib.mkIf cfg.enable {
    systemd.services.nixfleet-agent = {
      description = "NixFleet Fleet Management Agent";
      wantedBy = ["multi-user.target"];
      after = ["network-online.target"];
      wants = ["network-online.target"];

      serviceConfig = {
        Type = "simple";
        ExecStart = lib.concatStringsSep " " (
          [
            "${nixfleet-agent}/bin/nixfleet-agent"
            "--control-plane-url"
            (lib.escapeShellArg cfg.controlPlaneUrl)
            "--machine-id"
            (lib.escapeShellArg cfg.machineId)
            "--poll-interval"
            (toString cfg.pollInterval)
            "--db-path"
            (lib.escapeShellArg cfg.dbPath)
          ]
          ++ lib.optionals (cfg.cacheUrl != null) [
            "--cache-url"
            (lib.escapeShellArg cfg.cacheUrl)
          ]
          ++ lib.optionals cfg.dryRun [
            "--dry-run"
          ]
        );
        Restart = "always";
        RestartSec = 30;
        StateDirectory = "nixfleet";

        # Hardening
        NoNewPrivileges = true;
        ProtectHome = true;
        PrivateTmp = true;
        PrivateDevices = true;
        ProtectKernelTunables = true;
        ProtectKernelModules = true;
        ProtectControlGroups = true;
        ReadWritePaths = ["/var/lib/nixfleet" "/nix/var/nix"];
        ReadOnlyPaths = ["/nix/store" "/run/current-system"];
      };
    };

    # Impermanence: persist agent state across reboots
    environment.persistence."/persist".directories =
      lib.mkIf
      (config.hostSpec.isImpermanent or false)
      ["/var/lib/nixfleet"];
  };
}
