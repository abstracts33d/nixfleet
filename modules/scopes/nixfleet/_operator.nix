{
  config,
  inputs,
  lib,
  pkgs,
  ...
}: let
  cfg = config.nixfleet.operator;
  nixfleet-cli = inputs.self.packages.${pkgs.system}.nixfleet-cli;
in {
  options.nixfleet.operator = {
    enable = lib.mkEnableOption ''
      operator-workstation tooling: installs `nixfleet` (status),
      `nixfleet-mint-token`, and `nixfleet-derive-pubkey` system-wide.
    '';

    orgRootKeyFile = lib.mkOption {
      type = lib.types.nullOr lib.types.str;
      default = null;
      example = "/run/secrets/org-root-key";
      description = ''
        Path to the org root ed25519 private key (raw 32 bytes),
        decrypted by the fleet's secrets backend. Used by
        `nixfleet-mint-token --org-root-key` when the operator runs
        the tool interactively. The path is not consumed by any
        systemd service; it's only read when the operator invokes
        the tool.

        Set on the operator's workstation only — `null` on every
        other host.
      '';
    };
  };

  config = lib.mkIf cfg.enable {
    environment.systemPackages = [nixfleet-cli];

    environment.variables = lib.mkIf (cfg.orgRootKeyFile != null) {
      NIXFLEET_OPERATOR_ORG_ROOT_KEY = cfg.orgRootKeyFile;
    };
  };
}
