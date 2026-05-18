{
  lib,
  pkgs,
  testCerts,
  signedFixture,
  cpPkg,
  revocationsFixture ? null,
  ...
}: let
  hasRevocations = revocationsFixture != null;
in {
  imports = [
    ../../../contracts/trust.nix
    ../../../contracts/persistence.nix
    ../../../modules/scopes/nixfleet/_control-plane.nix
  ];

  environment.etc =
    {
      "nixfleet-cp/ca.pem".source = "${testCerts}/ca.pem";
      "nixfleet-cp/cp-cert.pem".source = "${testCerts}/cp-cert.pem";
      "nixfleet-cp/cp-key.pem".source = "${testCerts}/cp-key.pem";
      "nixfleet-cp/fleet-ca-cert.pem".source = "${testCerts}/ca.pem";
      "nixfleet-cp/fleet-ca-key.pem".source = "${testCerts}/ca-key.pem";
    }
    // lib.optionalAttrs hasRevocations {
      "nixfleet-cp-static/revocations.json".source = "${revocationsFixture}/revocations.json";
      "nixfleet-cp-static/revocations.json.sig".source = "${revocationsFixture}/revocations.json.sig";
    };

  systemd.services.harness-revocations-server = lib.mkIf hasRevocations {
    description = "Static HTTP server for the harness revocations sidecar";
    wantedBy = ["multi-user.target"];
    after = ["network.target"];
    serviceConfig = {
      Type = "simple";
      ExecStart = "${pkgs.python3}/bin/python3 -m http.server 9090 --directory /etc/nixfleet-cp-static --bind 127.0.0.1";
      Restart = "on-failure";
      RestartSec = 2;
    };
  };

  # Serves the entire signedFixture (canonical.json + signatures +
  # rollouts/) via a single local HTTP endpoint so the CP's poll
  # workers can fetch + verify them at runtime instead of relying on
  # static `--artifact`/`--signature` flags (which prime route caches
  # but NOT the reducer's `manifests` cache — the manifest_poll worker
  # idles when channelRefsSource is null, so the planner never opens
  # any rollout). Production CP fetches from a CI artifact server; the
  # harness mirrors that flow against a localhost server backed by the
  # signedFixture derivation.
  systemd.services.harness-fleet-server = {
    description = "Static HTTP server for the harness signed fleet artifacts";
    wantedBy = ["multi-user.target"];
    after = ["network.target"];
    serviceConfig = {
      Type = "simple";
      ExecStart = "${pkgs.python3}/bin/python3 -m http.server 9091 --directory ${signedFixture} --bind 127.0.0.1";
      Restart = "on-failure";
      RestartSec = 2;
    };
  };

  # LOADBEARING: CP poll loop needs a reachable upstream on first tick
  # (revocations + fleet sidecars must start before CP).
  systemd.services.nixfleet-control-plane.after =
    (lib.optional hasRevocations "harness-revocations-server.service")
    ++ ["harness-fleet-server.service"];

  networking.firewall.allowedTCPPorts =
    (lib.optional hasRevocations 9090) ++ [9091];

  services.nixfleet-control-plane =
    {
      enable = true;
      package = cpPkg;
      listen = "0.0.0.0:8443";
      openFirewall = true;
      agentCnSuffix = "fleet.example.com";

      artifactPath = "${signedFixture}/canonical.json";
      signaturePath = "${signedFixture}/canonical.json.sig";
      trustFile = "${signedFixture}/test-trust.json";

      observedPath = "/var/lib/nixfleet-cp/observed.json";

      tls = {
        cert = "/etc/nixfleet-cp/cp-cert.pem";
        key = "/etc/nixfleet-cp/cp-key.pem";
        clientCa = "/etc/nixfleet-cp/ca.pem";
      };

      fleetCaCert = "/etc/nixfleet-cp/fleet-ca-cert.pem";
      fleetCaKey = "/etc/nixfleet-cp/fleet-ca-key.pem";
      auditLogPath = "/var/lib/nixfleet-cp/audit.log";
      dbPath = "/var/lib/nixfleet-cp/state.db";

      freshnessWindowMinutes = 43200;
    }
    // lib.optionalAttrs hasRevocations {
      revocationsSource = {
        artifactUrl = "http://127.0.0.1:9090/revocations.json";
        signatureUrl = "http://127.0.0.1:9090/revocations.json.sig";
      };
    }
    // {
      # Drive manifest_poll against the localhost fleet server so the
      # reducer's `manifests` cache populates via the same code path
      # production uses (rather than only priming route state through
      # the `--artifact` flags). Without this, plan_next never sees a
      # SignedManifestSet and the planner can't open any rollout.
      channelRefsSource = {
        artifactUrl = "http://127.0.0.1:9091/canonical.json";
        signatureUrl = "http://127.0.0.1:9091/canonical.json.sig";
      };

      # Per-rollout manifest fetch. CP's manifest_poll substitutes
      # `{rolloutId}` for the canonical RFC-0012 §6.3 composite
      # `{channel}@{channel_ref}` when fetching.
      rolloutsSource = {
        artifactUrlTemplate = "http://127.0.0.1:9091/rollouts/{rolloutId}.json";
        signatureUrlTemplate = "http://127.0.0.1:9091/rollouts/{rolloutId}.json.sig";
      };
    };

  system.stateVersion = lib.mkDefault "24.11";
}
