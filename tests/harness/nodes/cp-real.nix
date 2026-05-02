# Real-binary CP node for harness scenarios. Drives the framework's
# `services.nixfleet-control-plane` module against the harness's signed
# fixture so the harness boots the same systemd unit + ExecStart shape
# operators get when consuming nixfleet (modules/scopes/nixfleet/_control-plane.nix).
#
# State lives in `/var/lib/nixfleet-cp/` so the teardown scenario can
# wipe it mid-run and observe recovery.
#
# Why the framework module rather than an inline systemd unit: the only
# scenario that boots the operator-facing module today is module-rollouts-wire;
# every other CP-binary scenario (teardown, deadline-expiry, stale-target,
# rollback-policy, secret-hygiene, boot-recovery, concurrent-checkin,
# enroll-replay) re-implemented the unit body, drifting from the real
# ExecStart. Migrating cp-real onto the module folds those re-impls
# back into the operator-facing surface.
#
# Mounting: same host-VM placement as the stub nodes (qemu user-net
# isolates microVM gateways; CP-on-host lets every agent reach it
# via 10.0.2.2). Listens on :8443 with mTLS.
#
# Fleet-CA wiring: the CP's `--fleet-ca-cert` / `--fleet-ca-key`
# flags drive `/v1/enroll` + `/v1/agent/renew`. The harness pre-
# mints client certs via `mkTlsCerts` so agents skip enrollment;
# we still pass the test CA's cert + key here so the CP boots
# clean and renew flows could be exercised by future scenarios.
#
# Optional revocations sidecar: when `revocationsFixture` is non-
# null, a static HTTP server runs alongside on :9090 serving the
# signed `revocations.json` + sig pair, and the CP gets the matching
# `revocationsSource` URLs. This is the harness path for proving CP
# rebuild restores `cert_revocations` from the signed sidecar.
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
    # Trust-root + persistence option schemas the framework module
    # reads. Defaults (all keys null) are fine: the test points
    # `trustFile` at the fixture's pre-built test-trust.json so the
    # module's auto-generated trust.json is unused.
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

  # Static HTTP server for the revocations sidecar. Python's stdlib
  # http.server is enough — same pattern cp-signed.nix uses for the
  # signed-roundtrip flow.
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

  # Sequence the revocations sidecar before the CP unit when present
  # so the CP's poll loop has a reachable upstream on first tick.
  # Module-system list-merge appends to the framework module's
  # `after = ["network-online.target"]`.
  systemd.services.nixfleet-control-plane.after =
    lib.mkIf hasRevocations ["harness-revocations-server.service"];

  networking.firewall.allowedTCPPorts = lib.optional hasRevocations 9090;

  services.nixfleet-control-plane =
    {
      enable = true;
      package = cpPkg;
      listen = "0.0.0.0:8443";
      openFirewall = true;

      # Point at /nix/store paths so the module's ExecStart accepts
      # them as-is and `unitConfig.ConditionPathExists = artifactPath`
      # is satisfied at unit start.
      artifactPath = "${signedFixture}/canonical.json";
      signaturePath = "${signedFixture}/canonical.json.sig";
      trustFile = "${signedFixture}/test-trust.json";

      # Observed lives under StateDirectory so it survives a CP wipe
      # via the systemd-tmpfiles `C` seed. Default value, kept explicit.
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

      # 7 days = 10080 minutes. Matches the original cp-real
      # `--freshness-window-secs 604800` default.
      freshnessWindowMinutes = 10080;
    }
    // lib.optionalAttrs hasRevocations {
      revocationsSource = {
        artifactUrl = "http://127.0.0.1:9090/revocations.json";
        signatureUrl = "http://127.0.0.1:9090/revocations.json.sig";
      };
    };

  system.stateVersion = lib.mkDefault "24.11";
}
