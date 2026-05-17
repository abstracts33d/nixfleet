{
  config,
  inputs,
  lib,
  pkgs,
  ...
}: {
  options.services.nixfleet-agent = {
    enable = lib.mkEnableOption "NixFleet fleet management agent";

    package = lib.mkOption {
      type = lib.types.package;
      default = inputs.self.packages.${pkgs.system}.nixfleet-agent;
      defaultText = lib.literalExpression "inputs.self.packages.\${pkgs.system}.nixfleet-agent";
      description = ''
        The agent package that provides `bin/nixfleet-agent`. Defaults
        to the flake's crane-built package; tests and pinned-version
        deploys override with their own derivation. Standard NixOS
        `services.<x>.package` escape hatch - accepted as-is, no
        further resolution.
      '';
    };

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

    renewalThresholdFraction = lib.mkOption {
      type = lib.types.nullOr lib.types.float;
      default = null;
      example = 0.5;
      description = ''
        Fraction of cert validity remaining below which the agent
        self-renews. When unset the agent uses its default (0.5,
        renew at half-life). Operators MAY raise this (e.g. 0.8)
        for short-cycle hardware testing of renewal flows.

        Must be strictly between 0 and 1. The agent refuses to
        start if validation fails.
      '';
    };

    trustFile = lib.mkOption {
      type = lib.types.path;
      default = "/etc/nixfleet/agent/trust.json";
      description = ''
        Path to the trust-root JSON file. The default is materialised
        by this module from config.nixfleet.trust via environment.etc;
        override only when sourcing the file from a secrets manager.
        See docs/rfcs/0005-trust-lifecycle.md §1.5 for the wiring.
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
        default = "/var/lib/nixfleet/agent-cert.pem";
        example = "/var/lib/nixfleet/agent-cert.pem";
        description = ''
          Path to the client certificate PEM file for mTLS
          authentication. Defaults to `/var/lib/nixfleet/agent-cert.pem`
          - a writable, persistent location under the agent's
          stateDir (already in `nixfleet.persistence.directories`).

          Post-RFC-0003-§2 (closed nixfleet#43): the cert is ISSUED
          by `/v1/enroll` and WRITTEN by the agent. It is not
          operator-deployed, so the path must be writable + survive
          reboots. tmpfs paths (e.g. `/run/agenix/...`) break the
          agent's enrollment loop because the bootstrap token is
          one-shot - losing the cert on reboot means the agent can't
          re-enroll on its own.
        '';
      };

      clientKey = lib.mkOption {
        type = lib.types.nullOr lib.types.str;
        default = "/etc/ssh/ssh_host_ed25519_key";
        example = "/etc/ssh/ssh_host_ed25519_key";
        description = ''
          Path to the private key the agent uses to mint CSRs at
          `/v1/enroll` and `/v1/agent/renew`. Defaults to the host's
          SSH ed25519 host key (RFC-0003 §2 binding).

          The CP rejects any CSR whose pubkey doesn't match the host's
          declared `nixfleet.fleetSchema.hosts.<hostname>.pubkey`  -
          declare it in `fleet.nix` BEFORE first enrollment. Operators
          previously deploying per-host agent keys via agenix should
          drop those entries (`agents/<host>-key.age` from
          fleet-secrets) once all hosts have rotated to host-key-bound
          certs at their next 30-day renewal cycle.
        '';
      };
    };

    bootstrapTokenFile = lib.mkOption {
      type = lib.types.nullOr lib.types.str;
      default = null;
      example = "/run/secrets/bootstrap-token-host-01";
      description = ''
        Path to a one-shot bootstrap token (operator-minted by
        `nixfleet mint-token`, signed with the org root key). Used
        by the agent's first-boot enrollment flow only - once the
        cert exists at `tls.clientCert`, the token is never read
        again. Renewal at 50% of cert validity uses the existing
        cert (mTLS-authenticated /v1/agent/renew), not this token.
      '';
    };

    stateDir = lib.mkOption {
      type = lib.types.str;
      default = "/var/lib/nixfleet-agent";
      description = ''
        Directory the agent uses for per-host persistent state.
        Currently holds `last_confirmed_at` - a two-line plaintext
        file binding the agent's most recent successful confirm
        timestamp to the closure it applies to. Pre-created with
        mode 0700 by the platform supervisor (systemd's
        `StateDirectory=` on NixOS; the preActivation script on
        darwin). Survives agent process restart.
      '';
    };

    sshHostKeyFile = lib.mkOption {
      type = lib.types.str;
      default = "/etc/ssh/ssh_host_ed25519_key";
      description = ''
        Host SSH ed25519 private key. The agent uses the matching
        PUBLIC half to verify compliance-evidence files produced by the
        local collector unit (RFC-0004 §5 + RFC-0010 §7 evidence probe).
        Default matches OpenSSH's stock path; override only if the host
        runs sshd with a non-default `HostKey` config.
      '';
    };

    tags = lib.mkOption {
      type = lib.types.listOf lib.types.str;
      default = [];
      description = ''
        Free-form tags reported with each checkin via the
        `NIXFLEET_TAGS` environment variable. Joined with commas
        before being passed to the agent. Used for operator
        observability (e.g. distinguishing build hosts from
        runners) and ignored by the dispatch decision.
      '';
    };

    effectiveHealthChecks = lib.mkOption {
      type = lib.types.attrs;
      default = {};
      description = ''
        Effective per-host probe set, resolved by `mkFleet` from
        `nixfleet.healthChecks` (fleet) + `nixfleet.tags.<>.healthChecks`
        (tag-scoped) + `nixfleet.hosts.<>.healthChecks` (host-scoped) with
        host > tag > fleet precedence (RFC-0010 §3.2). The framework's
        `mkHost` plumbs the resolved set into this option at fleet-eval
        time; `_agent.nix` renders it as
        `/etc/nixfleet/agent/health-checks.json`. Operators do NOT set
        this directly — declare probes at the appropriate scope in
        `fleet.nix`.

        Shape mirrors `healthProbeType` in `lib/mk-fleet.nix`:
        attrset keyed by probe name, each value carries
        `{ kind, mode, intervalSeconds | runOnce, ...kind-specific }`.
      '';
    };
  };
}
