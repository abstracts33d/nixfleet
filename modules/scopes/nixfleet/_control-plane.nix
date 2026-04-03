# NixOS service module for the NixFleet control plane server.
# Auto-included by mkHost (disabled by default).
{
  config,
  lib,
  pkgs,
  ...
}: let
  cfg = config.services.nixfleet-control-plane;
  nixfleet-control-plane = pkgs.callPackage ../../../control-plane {};
in {
  options.services.nixfleet-control-plane = {
    enable = lib.mkEnableOption "NixFleet control plane server";

    listen = lib.mkOption {
      type = lib.types.str;
      default = "0.0.0.0:8080";
      description = "Address and port to listen on.";
    };

    dbPath = lib.mkOption {
      type = lib.types.str;
      default = "/var/lib/nixfleet-cp/state.db";
      description = "Path to the SQLite state database.";
    };

    openFirewall = lib.mkOption {
      type = lib.types.bool;
      default = false;
      description = "Open the control plane port in the firewall.";
    };
  };

  config = lib.mkIf cfg.enable {
    systemd.services.nixfleet-control-plane = {
      description = "NixFleet Control Plane Server";
      wantedBy = ["multi-user.target"];
      after = ["network-online.target"];
      wants = ["network-online.target"];

      serviceConfig = {
        Type = "simple";
        ExecStart = lib.concatStringsSep " " [
          "${nixfleet-control-plane}/bin/nixfleet-control-plane"
          "--listen"
          (lib.escapeShellArg cfg.listen)
          "--db-path"
          (lib.escapeShellArg cfg.dbPath)
        ];
        Restart = "always";
        RestartSec = 10;
        StateDirectory = "nixfleet-cp";

        # Hardening
        NoNewPrivileges = true;
        ProtectHome = true;
        PrivateTmp = true;
        PrivateDevices = true;
        ProtectKernelTunables = true;
        ProtectKernelModules = true;
        ProtectControlGroups = true;
        ReadWritePaths = ["/var/lib/nixfleet-cp"];
      };
    };

    # Open firewall port if requested
    networking.firewall.allowedTCPPorts = let
      port = lib.toInt (lib.last (lib.splitString ":" cfg.listen));
    in
      lib.mkIf cfg.openFirewall [port];

    # Impermanence: persist CP state across reboots
    environment.persistence."/persist".directories =
      lib.mkIf
      (config.hostSpec.isImpermanent or false)
      ["/var/lib/nixfleet-cp"];
  };
}
