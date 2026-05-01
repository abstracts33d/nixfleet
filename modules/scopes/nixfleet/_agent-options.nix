# Shared option declarations for the NixFleet fleet agent.
#
# Imported by both `_agent.nix` (NixOS / systemd supervisor) and
# `_agent-darwin.nix` (nix-darwin / launchd supervisor) so a fleet's
# `services.nixfleet-agent.<...>` settings work unchanged regardless
# of host platform. Options-only — the supervisor modules carry the
# platform-specific `config = mkIf cfg.enable { ... }` blocks.
#
# Auto-included transitively via the supervisor module that mkHost
# selects per platform; no separate import needed at the call site.
{
  config,
  lib,
  ...
}: {
  options.services.nixfleet-agent = {
    enable = lib.mkEnableOption "NixFleet fleet management agent";

    controlPlaneUrl = lib.mkOption {
      type = lib.types.str;
      example = "https://fleet.example.com";
      description = "URL of the NixFleet control plane.";
    };

    machineId = lib.mkOption {
      type = lib.types.str;
      default = config.hostSpec.hostName or config.networking.hostName;
      defaultText = lib.literalExpression "config.hostSpec.hostName or config.networking.hostName";
      description = "Machine identifier reported to the control plane.";
    };

    pollInterval = lib.mkOption {
      type = lib.types.int;
      default = 60;
      description = "Poll interval in seconds (steady-state).";
    };

    trustFile = lib.mkOption {
      type = lib.types.path;
      default = "/etc/nixfleet/agent/trust.json";
      description = ''
        Path to the trust-root JSON file (see docs/trust-root-flow.md §3.4).
        The default is materialised by this module from config.nixfleet.trust
        via environment.etc; override only when sourcing the file from a
        secrets manager.
      '';
    };

    tls = {
      caCert = lib.mkOption {
        type = lib.types.nullOr lib.types.str;
        default = null;
        example = "/etc/nixfleet/fleet-ca.pem";
        description = "Path to CA certificate PEM file for verifying the control plane. Trusted alongside system roots.";
      };

      clientCert = lib.mkOption {
        type = lib.types.nullOr lib.types.str;
        default = null;
        example = "/run/secrets/agent-cert.pem";
        description = "Path to client certificate PEM file for mTLS authentication.";
      };

      clientKey = lib.mkOption {
        type = lib.types.nullOr lib.types.str;
        default = null;
        example = "/run/secrets/agent-key.pem";
        description = "Path to client private key PEM file for mTLS authentication.";
      };
    };

    # Bootstrap token for first-boot enrollment. When set, and
    # `tls.clientCert` doesn't exist yet on disk, the agent reads
    # this token, generates a CSR, POSTs /v1/enroll, and writes the
    # issued cert + key to the configured paths before entering its
    # poll loop. The fleet wires this to a secrets-backend-decrypted
    # `bootstrap-token-${hostname}` path (agenix, sops, systemd-creds,
    # …).
    bootstrapTokenFile = lib.mkOption {
      type = lib.types.nullOr lib.types.str;
      default = null;
      example = "/run/secrets/bootstrap-token-host-01";
      description = ''
        Path to a one-shot bootstrap token (operator-minted by
        `nixfleet-mint-token`, signed with the org root key). Used
        by the agent's first-boot enrollment flow only — once the
        cert exists at `tls.clientCert`, the token is never read
        again. Renewal at 50% of cert validity uses the existing
        cert (mTLS-authenticated /v1/agent/renew), not this token.
      '';
    };

    # Per-host state directory. The agent persists
    # `last_confirmed_at` here after every successful confirm so
    # subsequent checkins can attest the timestamp; the CP-side
    # `recover_soak_state_from_attestation` repopulates
    # `host_rollout_state.last_healthy_since` after a CP rebuild.
    # Aligned with `StateDirectory=nixfleet-agent` so the systemd
    # unit creates the directory with the right owner + perms.
    # Agent-side population of last_confirmed_at folds into the
    # magic-rollback work.
    stateDir = lib.mkOption {
      type = lib.types.str;
      default = "/var/lib/nixfleet-agent";
      description = ''
        Directory the agent uses for per-host persistent state.
        Currently holds `last_confirmed_at` — a two-line plaintext
        file binding the agent's most recent successful confirm
        timestamp to the closure it applies to. Created with mode
        0700 by the systemd unit's StateDirectory. Survives agent
        process restart but NOT `systemd-tmpfiles --remove` style
        wipes.
      '';
    };

    # Runtime compliance gate policy.
    # `auto` (default) auto-detects from collector unit presence:
    # Permissive when present, Disabled when absent. Operators
    # introducing compliance to an existing fleet typically: deploy
    # `permissive` while observing what would fail; flip to `enforce`
    # once the fleet's compliance posture is healthy. The CP can
    # relay a per-channel override via `EvaluatedTarget.compliance_mode`
    # — when present, the relay wins over this local default.
    complianceGate.mode = lib.mkOption {
      type = lib.types.enum ["auto" "disabled" "permissive" "enforce"];
      default = "auto";
      description = ''
        Local default for the runtime compliance gate.

        - `auto` (default): permissive when the
          `compliance-evidence-collector.service` unit is present on
          this host, disabled when absent. Safe for fleets that
          haven't deployed `nixfleet-compliance` — no events posted,
          no rollouts blocked.
        - `permissive`: the gate runs and posts `RuntimeGateError`
          and `ComplianceFailure` events on failure, but does NOT
          block the activation confirm. Use during incremental
          rollout to observe what would fail without breaking
          deploys.
        - `enforce`: same events posted; additionally a
          `RuntimeGateError` (collector failed / stale evidence)
          triggers a local rollback and skips confirm. Same severity
          class as a SwitchFailed.
        - `disabled`: gate skipped entirely. No events, no journal
          warnings.

        The CP can relay a per-channel
        `EvaluatedTarget.compliance_mode` to override this; when
        absent (or set to `auto`), this value is used.
      '';
    };
  };
}
