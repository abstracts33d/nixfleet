# Operator workstation scope.
#
# Wires the operator-side tooling into a NixOS host:
# - `nixfleet-mint-token` — signs bootstrap tokens for /v1/enroll.
# - `nixfleet-derive-pubkey` — derives base64 ed25519 pubkey from a
#   raw private key file (one-shot, used when initialising the org
#   root key).
#
# The org root **private** key is intentionally NOT a fleet-wide
# secret. It lives in fleet-secrets agenix-encrypted to the operator
# user + the operator workstation's host key only — lab CP and other
# fleet hosts never decrypt it. The CP only verifies token signatures
# with the public half (declared in `config.nixfleet.trust.orgRootKey`).
#
# Per the design property in `docs/CONTRACTS.md §II #3` and
# nixfleet#10's "control plane holds no secrets, forges no trust",
# the org root key compromise scenario is a multi-host operator-
# workstation event — not a CP-side breach. Sovereignty preserved.
#
# Auto-included by mkHost (disabled by default). Enable on the
# operator's workstation only.
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
      operator-workstation tooling: installs `nixfleet-mint-token`
      and `nixfleet-derive-pubkey` system-wide.
    '';

    orgRootKeyFile = lib.mkOption {
      type = lib.types.nullOr lib.types.str;
      default = null;
      example = "/run/agenix/org-root-key";
      description = ''
        Path to the agenix-decrypted org root ed25519 private key
        (raw 32 bytes). Used by `nixfleet-mint-token --org-root-key`
        when the operator runs the tool interactively. The path is
        not consumed by any systemd service; it's only read when the
        operator invokes the tool.

        Wired by `fleet/modules/nixfleet/operator.nix` to
        `config.age.secrets.org-root-key.path` on the operator's
        workstation. `null` on every other host.
      '';
    };
  };

  config = lib.mkIf cfg.enable {
    # nixfleet-cli's bin/ contains both nixfleet (operator CLI) and
    # nixfleet-mint-token + nixfleet-derive-pubkey. Adding the whole
    # package puts all three in PATH; matches the convention for the
    # other scopes (full package, not selective binaries).
    environment.systemPackages = [nixfleet-cli];

    # Surface the configured key path via shell env so the operator
    # can run `nixfleet-mint-token` without remembering the agenix
    # path (or muscle-memorise an alias). When `orgRootKeyFile` is
    # null this stays unset.
    environment.variables = lib.mkIf (cfg.orgRootKeyFile != null) {
      NIXFLEET_OPERATOR_ORG_ROOT_KEY = cfg.orgRootKeyFile;
    };
  };
}
