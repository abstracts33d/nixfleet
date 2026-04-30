# NixOS service module for the NixFleet control plane.
#
# Long-running TLS server. The binary's `serve` subcommand runs
# forever, accepts mTLS-authenticated connections, and ticks an
# internal reconcile loop every 30s. The unit runs as `Type=simple`
# (no timer companion). The `tick` subcommand on the binary is
# preserved for tests and ad-hoc operator runs.
#
# Auto-included by mkHost (disabled by default). Enable on the
# coordinator host (typically lab) only.
{
  config,
  inputs,
  lib,
  pkgs,
  ...
}: let
  cfg = config.services.nixfleet-control-plane;
  nixfleet-control-plane = inputs.self.packages.${pkgs.system}.nixfleet-control-plane;

  # Shared trust.json payload — see ./_trust-json.nix for shape rationale
  # and the orgRootKey ed25519 promotion that matches proto::TrustConfig.
  trustConfig = import ./_trust-json.nix {trust = config.nixfleet.trust;};
  trustJson = pkgs.writers.writeJSON "trust.json" trustConfig;

  # First-deploy bootstrap for observed.json — laid down via
  # systemd-tmpfiles `C` (copy only if path does not exist) so the
  # reconciler's first tick has a parseable file even before the
  # operator has hand-written one. The in-memory projection from
  # agent check-ins is preferred; this stays as the offline dev/test
  # fallback.
  initialObservedJson = pkgs.writers.writeJSON "observed-initial.json" {
    channelRefs = {};
    lastRolledRefs = {};
    hostState = {};
    activeRollouts = [];
  };

  # Parse the listen address into HOST:PORT for the firewall rule.
  listenPort = lib.toInt (lib.last (lib.splitString ":" cfg.listen));
in {
  options.services.nixfleet-control-plane = {
    enable = lib.mkEnableOption "NixFleet control plane (long-running TLS server)";

    listen = lib.mkOption {
      type = lib.types.str;
      default = "0.0.0.0:8080";
      example = "0.0.0.0:8080";
      description = ''
        HOST:PORT the control plane listens on. Default 8080 per spec
        D3 — port < 1024 would require CAP_NET_BIND_SERVICE, and 443
        is typically already taken by a reverse proxy on the same host.
      '';
    };

    openFirewall = lib.mkOption {
      type = lib.types.bool;
      default = false;
      description = ''
        Open the listen port in the system firewall. Defaults to
        false because lab is on Tailscale; production deploys may
        want this true once the perimeter posture is reviewed.
      '';
    };

    tls = {
      cert = lib.mkOption {
        type = lib.types.nullOr lib.types.str;
        default = null;
        example = "/run/secrets/cp-cert";
        description = ''
          Path to the TLS server certificate PEM file. Wired by
          the fleet's secrets backend to the decrypted `cp-cert`
          path. Required when `enable = true`; the assertion at
          config-time enforces this.
        '';
      };

      key = lib.mkOption {
        type = lib.types.nullOr lib.types.str;
        default = null;
        example = "/run/secrets/cp-key";
        description = ''
          Path to the TLS server private key PEM file (decrypted
          `cp-key` path). Required when `enable = true`.
        '';
      };

      clientCa = lib.mkOption {
        type = lib.types.nullOr lib.types.str;
        default = null;
        example = "/etc/nixfleet/fleet-ca.pem";
        description = ''
          Path to the client CA PEM file. When set, the server
          requires verified client certs (mTLS). Optional — the
          server starts in TLS-only mode if unset and logs a warning.
          Standard deploys set this; production hosts should always
          have it.
        '';
      };
    };

    artifactPath = lib.mkOption {
      type = lib.types.str;
      default = "/var/lib/nixfleet-cp/fleet/releases/fleet.resolved.json";
      description = ''
        Path to the canonical fleet.resolved.json bytes (the file CI
        signed). Operator is responsible for keeping this path
        up-to-date with the fleet repo's HEAD — typically a separate
        timer that pulls the fleet repo into
        `/var/lib/nixfleet-cp/fleet/`. The CP module does not pull
        git itself; in-process Forgejo polling can refresh this
        cache automatically.
      '';
    };

    signaturePath = lib.mkOption {
      type = lib.types.str;
      default = "/var/lib/nixfleet-cp/fleet/releases/fleet.resolved.json.sig";
      description = "Path to the raw signature bytes paired with `artifactPath`.";
    };

    observedPath = lib.mkOption {
      type = lib.types.str;
      default = "/var/lib/nixfleet-cp/observed.json";
      description = ''
        Path to the JSON file holding observed fleet state — shape
        per `nixfleet_reconciler::Observed`. Hand-written by the
        operator (auto-bootstrapped to an empty skeleton on first
        deploy via systemd-tmpfiles). The live in-memory projection
        from agent check-ins is preferred; this path remains as the
        offline dev/test fallback.
      '';
    };

    trustFile = lib.mkOption {
      type = lib.types.path;
      default = "/etc/nixfleet/cp/trust.json";
      description = ''
        Path to the trust-root JSON file (see
        docs/trust-root-flow.md §3.4). Materialised by this module
        from `config.nixfleet.trust` via environment.etc.
      '';
    };

    freshnessWindowMinutes = lib.mkOption {
      type = lib.types.ints.positive;
      default = 1440;
      description = ''
        Maximum age (minutes) of `meta.signedAt` accepted by
        `verify_artifact`. Match the operator-declared channel
        `freshnessWindow` in fleet.nix when in doubt; default is 24h.
      '';
    };

    confirmDeadlineSecs = lib.mkOption {
      type = lib.types.ints.positive;
      default = 360;
      description = ''
        Seconds the dispatch loop gives an agent to fetch + activate
        + confirm a target before the magic-rollback timer marks the
        pending row as `rolled-back`.

        Default 360s: agents activate via fire-and-forget (ADR-011,
        ~300s polling `/run/current-system` after the detached
        `systemd-run` is fired) plus 60s slack. Dropping below ~310s
        creates a chaos cascade — CP rolls back while the agent is
        still polling, agent eventually polls success, posts confirm,
        CP returns 410, agent triggers local rollback.

        Tune up for slow-link channels (large closures over residential
        uplinks); avoid tuning down without first lowering the
        agent-side poll budget. Wraps the binary's `--confirm-deadline-secs`.
      '';
    };

    # Cert issuance (enroll + renew). The CP holds the fleet
    # CA private key online — see nixfleet issue #41 for the deferred
    # TPM-bound replacement. The fleet wires these to paths produced
    # by its secrets backend.
    fleetCaCert = lib.mkOption {
      type = lib.types.nullOr lib.types.str;
      default = null;
      example = "/etc/nixfleet/fleet-ca.pem";
      description = ''
        Fleet CA cert path. Used by issuance for chain assembly
        (clientAuth EKU agent certs). Typically the same path as
        `tls.clientCa`.
      '';
    };

    fleetCaKey = lib.mkOption {
      type = lib.types.nullOr lib.types.str;
      default = null;
      example = "/run/secrets/fleet-ca-key";
      description = ''
        Fleet CA private key path (decrypted by the fleet's secrets
        backend). Used to sign agent certs in /v1/enroll and
        /v1/agent/renew. **Online on the CP — see nixfleet issue
        #41.**
      '';
    };

    auditLogPath = lib.mkOption {
      type = lib.types.str;
      default = "/var/lib/nixfleet-cp/issuance.log";
      description = ''
        JSON-lines audit log of every cert issuance (enroll | renew).
        Best-effort writes; failure logs a warn but doesn't block
        issuance.
      '';
    };

    # Closure proxy upstream. Cache server (harmonia, attic, nix-serve,
    # cachix, …) the CP forwards /v1/agent/closure/<hash> requests to.
    # Typically a cache running on the same host as the CP. When null,
    # the endpoint returns 501.
    closureUpstream = lib.mkOption {
      type = lib.types.nullOr lib.types.str;
      default = null;
      example = "http://localhost:8085";
      description = ''
        Attic upstream URL for closure-proxy forwarding. Ships
        narinfo forwarding (operator can curl
        `<cp>/v1/agent/closure/<hash>` and get the upstream's
        narinfo response). Full nar streaming is a follow-up.
      '';
    };

    # SQLite path. When set, the CP opens + migrates the DB on
    # startup. Token replay + cert revocations + pending confirms +
    # rollouts persist across CP restarts. When null, in-memory state
    # only — fine for dev, not production.
    dbPath = lib.mkOption {
      type = lib.types.nullOr lib.types.str;
      default = "/var/lib/nixfleet-cp/state.db";
      description = ''
        Path to the SQLite database. Default lives under
        StateDirectory so impermanent hosts can persist via
        environment.persistence (already declared below). Set to
        `null` to disable persistence — e.g. for dev/test or until
        the operator is ready for the full stateful CP.
      '';
    };

    # Channel-refs poll. When set, the CP polls a configured pair of
    # URLs every 60s for the signed `fleet.resolved.json` + `.sig`,
    # verifies, and refreshes the in-memory verified-fleet snapshot.
    # Implementation-agnostic — the framework only knows how to issue
    # `GET <url>` with an optional Bearer token. URL templates for
    # specific git forges (Forgejo `/raw/branch/...`, GitHub
    # `raw.githubusercontent.com/...`, GitLab `/-/raw/...`) live in
    # `impls/gitops/` and are exposed at `flake.scopes.gitops.<forge>`;
    # consumers either use those helpers or build the URLs by hand.
    channelRefsSource = {
      artifactUrl = lib.mkOption {
        type = lib.types.nullOr lib.types.str;
        default = null;
        example = "https://git.example.com/myorg/myfleet/raw/branch/main/releases/fleet.resolved.json";
        description = ''
          Fully-formed URL that yields the raw bytes of the canonical
          signed fleet.resolved.json. When null, channel-refs polling
          is disabled and the CP falls back to the file-backed
          observed.json. Must be set together with `signatureUrl`.
        '';
      };

      signatureUrl = lib.mkOption {
        type = lib.types.nullOr lib.types.str;
        default = null;
        example = "https://git.example.com/myorg/myfleet/raw/branch/main/releases/fleet.resolved.json.sig";
        description = ''
          Fully-formed URL that yields the raw bytes of the matching
          signature. The poll task fetches both files together and
          runs verify_artifact — this is what closes the GitOps loop
          (push → CI re-sign → poll picks up new closureHashes within
          ~60s, no CP redeploy).
        '';
      };

      tokenFile = lib.mkOption {
        type = lib.types.nullOr lib.types.str;
        default = null;
        example = "/run/secrets/cp-channel-refs-token";
        description = ''
          Path to a file containing the upstream API token (sent as
          `Authorization: Bearer <token>`). Optional — leave null for
          public sources (e.g. unauthenticated raw URLs on a public
          forge or a plain HTTPS file server). Read on each poll so
          token rotation propagates without restart.
        '';
      };
    };

    # Gap C (#48): signed `revocations.json` sidecar. Mirrors
    # `channelRefsSource` —
    # same `(artifact, signature, token)` shape, same Bearer auth,
    # same trust class (ciReleaseKey signs both artifacts). When
    # configured, the CP polls the upstream every 60s and replays
    # entries into `cert_revocations` so a CP rebuilt from empty
    # state re-establishes the revocation set within one reconcile
    # tick. When all three are null, the legacy "operator runs CLI
    # against the CP" path stays in effect (which has no recovery
    # source — see DISASTER-RECOVERY.md).
    revocationsSource = {
      artifactUrl = lib.mkOption {
        type = lib.types.nullOr lib.types.str;
        default = null;
        example = "https://git.example.com/myorg/myfleet/raw/branch/main/releases/revocations.json";
        description = ''
          Fully-formed URL that yields the raw bytes of the canonical
          signed `revocations.json`. When null, revocations polling
          is disabled. Must be set together with `signatureUrl`.
        '';
      };

      signatureUrl = lib.mkOption {
        type = lib.types.nullOr lib.types.str;
        default = null;
        example = "https://git.example.com/myorg/myfleet/raw/branch/main/releases/revocations.json.sig";
        description = ''
          Fully-formed URL that yields the raw bytes of the matching
          signature. Verified against the same `ciReleaseKey` trust
          roots as `fleet.resolved.json`.
        '';
      };

      tokenFile = lib.mkOption {
        type = lib.types.nullOr lib.types.str;
        default = null;
        example = "/run/secrets/cp-revocations-token";
        description = ''
          Path to a file containing the upstream API token. Defaults
          to falling back on `channelRefsSource.tokenFile` when null
          since both artifacts typically live in the same upstream
          repo with the same auth scope. Set explicitly only if the
          two artifacts ship from different sources.
        '';
      };
    };
  };

  config = lib.mkMerge [
    (lib.mkIf cfg.enable {
      assertions = [
        {
          assertion = builtins.match ".*:[0-9]+" cfg.listen != null;
          message = ''
            services.nixfleet-control-plane.listen must be in HOST:PORT format
            (e.g. "0.0.0.0:8080"), got: "${cfg.listen}"
          '';
        }
        {
          assertion = (cfg.tls.cert != null) && (cfg.tls.key != null);
          message = ''
            services.nixfleet-control-plane requires both tls.cert and tls.key
            to be set when enabled. Wire them through your secrets backend.
          '';
        }
      ];

      environment.etc."nixfleet/cp/trust.json".source = trustJson;

      systemd.services.nixfleet-control-plane = {
        description = "NixFleet control plane (long-running TLS server)";
        wantedBy = ["multi-user.target"];
        after = ["network-online.target"];
        wants = ["network-online.target"];
        unitConfig.ConditionPathExists = cfg.artifactPath;

        serviceConfig = {
          Type = "simple";
          ExecStart = lib.concatStringsSep " " (
            [
              "${nixfleet-control-plane}/bin/nixfleet-control-plane"
              "serve"
              "--listen"
              (lib.escapeShellArg cfg.listen)
              "--tls-cert"
              (lib.escapeShellArg cfg.tls.cert)
              "--tls-key"
              (lib.escapeShellArg cfg.tls.key)
              "--artifact"
              (lib.escapeShellArg cfg.artifactPath)
              "--signature"
              (lib.escapeShellArg cfg.signaturePath)
              "--trust-file"
              (lib.escapeShellArg (toString cfg.trustFile))
              "--observed"
              (lib.escapeShellArg cfg.observedPath)
              "--freshness-window-secs"
              (toString (cfg.freshnessWindowMinutes * 60))
              "--confirm-deadline-secs"
              (toString cfg.confirmDeadlineSecs)
            ]
            ++ lib.optionals (cfg.tls.clientCa != null) [
              "--client-ca"
              (lib.escapeShellArg cfg.tls.clientCa)
            ]
            ++ lib.optionals (cfg.fleetCaCert != null) [
              "--fleet-ca-cert"
              (lib.escapeShellArg cfg.fleetCaCert)
            ]
            ++ lib.optionals (cfg.fleetCaKey != null) [
              "--fleet-ca-key"
              (lib.escapeShellArg cfg.fleetCaKey)
            ]
            ++ [
              "--audit-log"
              (lib.escapeShellArg cfg.auditLogPath)
            ]
            ++ lib.optionals (cfg.dbPath != null) [
              "--db-path"
              (lib.escapeShellArg cfg.dbPath)
            ]
            ++ lib.optionals (cfg.closureUpstream != null) [
              "--closure-upstream"
              (lib.escapeShellArg cfg.closureUpstream)
            ]
            ++ lib.optionals
            (
              cfg.channelRefsSource.artifactUrl
              != null
              && cfg.channelRefsSource.signatureUrl != null
            ) (
              [
                "--channel-refs-artifact-url"
                (lib.escapeShellArg cfg.channelRefsSource.artifactUrl)
                "--channel-refs-signature-url"
                (lib.escapeShellArg cfg.channelRefsSource.signatureUrl)
              ]
              ++ lib.optionals (cfg.channelRefsSource.tokenFile != null) [
                "--channel-refs-token-file"
                (lib.escapeShellArg cfg.channelRefsSource.tokenFile)
              ]
            )
            ++ lib.optionals
            (
              cfg.revocationsSource.artifactUrl
              != null
              && cfg.revocationsSource.signatureUrl != null
            ) (
              [
                "--revocations-artifact-url"
                (lib.escapeShellArg cfg.revocationsSource.artifactUrl)
                "--revocations-signature-url"
                (lib.escapeShellArg cfg.revocationsSource.signatureUrl)
              ]
              ++ lib.optionals
              (
                cfg.revocationsSource.tokenFile
                != null
                || cfg.channelRefsSource.tokenFile != null
              ) [
                "--revocations-token-file"
                (lib.escapeShellArg
                  (
                    if cfg.revocationsSource.tokenFile != null
                    then cfg.revocationsSource.tokenFile
                    else cfg.channelRefsSource.tokenFile
                  ))
              ]
            )
          );
          Restart = "always";
          RestartSec = 10;
          StateDirectory = "nixfleet-cp";

          # Hardening. Network access is required (TLS listener), so
          # PrivateNetwork is not set. ProtectSystem=strict is fine
          # since the server reads from /etc + /var/lib + the
          # secrets-backend mountpoint, and only writes to its
          # StateDirectory.
          ProtectSystem = "strict";
          ProtectHome = true;
          PrivateTmp = true;
          PrivateDevices = true;
          ProtectKernelTunables = true;
          ProtectKernelModules = true;
          ProtectControlGroups = true;
          NoNewPrivileges = true;
          ReadWritePaths = ["/var/lib/nixfleet-cp"];
        };
      };

      # First-deploy auto-bootstrap of observed.json. tmpfiles type `C`
      # (without the `+` modifier) copies from the seed path only if
      # the target does not already exist — operator edits to
      # observed.json survive rebuilds.
      systemd.tmpfiles.rules = [
        "d /var/lib/nixfleet-cp 0755 root root -"
        "C ${cfg.observedPath} 0644 root root - ${initialObservedJson}"
      ];

      networking.firewall.allowedTCPPorts = lib.mkIf cfg.openFirewall [listenPort];
    })

    # Persistence: contribute the CP state dir to the framework
    # persistence list. The active implementation reads the list.
    (lib.mkIf cfg.enable {
      nixfleet.persistence.directories = ["/var/lib/nixfleet-cp"];
    })
  ];
}
