# Real-binary CP node for the teardown scenario. Runs
# `nixfleet-control-plane serve` from the crane-built package against
# the harness's signed fixture. State lives in `/var/lib/nixfleet-cp/`
# so the teardown scenario can wipe it mid-run and observe recovery.
#
# Why a separate node from cp.nix / cp-signed.nix: those serve a
# static fixture from socat / Python http.server — no SQLite, no
# in-memory checkin map, nothing to wipe. The teardown test needs
# a CP that actually keeps state. Once this node is exercised in
# CI, it's also the path for any future scenario that wants real
# CP semantics (rollouts, dispatch, magic rollback).
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
# `--revocations-*` flags. This is the harness path for proving CP
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
  environment.etc =
    {
      "nixfleet-cp/ca.pem".source = "${testCerts}/ca.pem";
      "nixfleet-cp/cp-cert.pem".source = "${testCerts}/cp-cert.pem";
      "nixfleet-cp/cp-key.pem".source = "${testCerts}/cp-key.pem";
      "nixfleet-cp/canonical.json".source = "${signedFixture}/canonical.json";
      "nixfleet-cp/canonical.json.sig".source = "${signedFixture}/canonical.json.sig";
      "nixfleet-cp/test-trust.json".source = "${signedFixture}/test-trust.json";
      "nixfleet-cp/observed.json".text = builtins.toJSON {
        channelRefs = {};
        lastRolledRefs = {};
        hostState = {};
        activeRollouts = [];
      };
      "nixfleet-cp/fleet-ca-cert.pem".source = "${testCerts}/ca.pem";
      "nixfleet-cp/fleet-ca-key.pem".source = "${testCerts}/ca-key.pem";
    }
    // lib.optionalAttrs hasRevocations {
      "nixfleet-cp-static/revocations.json".source = "${revocationsFixture}/revocations.json";
      "nixfleet-cp-static/revocations.json.sig".source = "${revocationsFixture}/revocations.json.sig";
    };

  systemd.tmpfiles.rules = [
    "d /var/lib/nixfleet-cp 0700 root root -"
  ];

  networking.firewall.allowedTCPPorts = [8443] ++ lib.optional hasRevocations 9090;

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

  systemd.services.nixfleet-control-plane = {
    description = "NixFleet control plane (real binary, harness teardown scenario)";
    wantedBy = ["multi-user.target"];
    after = ["network.target"] ++ lib.optional hasRevocations "harness-revocations-server.service";
    serviceConfig = {
      Type = "simple";
      Restart = "on-failure";
      RestartSec = 2;
      ExecStart = lib.concatStringsSep " " (
        [
          "${cpPkg}/bin/nixfleet-control-plane"
          "serve"
          "--listen 0.0.0.0:8443"
          "--tls-cert /etc/nixfleet-cp/cp-cert.pem"
          "--tls-key /etc/nixfleet-cp/cp-key.pem"
          "--client-ca /etc/nixfleet-cp/ca.pem"
          "--fleet-ca-cert /etc/nixfleet-cp/fleet-ca-cert.pem"
          "--fleet-ca-key /etc/nixfleet-cp/fleet-ca-key.pem"
          "--audit-log /var/lib/nixfleet-cp/audit.log"
          "--artifact /etc/nixfleet-cp/canonical.json"
          "--signature /etc/nixfleet-cp/canonical.json.sig"
          "--trust-file /etc/nixfleet-cp/test-trust.json"
          "--observed /etc/nixfleet-cp/observed.json"
          "--db-path /var/lib/nixfleet-cp/state.db"
          "--freshness-window-secs 604800"
        ]
        ++ lib.optionals hasRevocations [
          "--revocations-artifact-url http://127.0.0.1:9090/revocations.json"
          "--revocations-signature-url http://127.0.0.1:9090/revocations.json.sig"
        ]
      );
      StandardOutput = "journal+console";
      StandardError = "journal+console";
    };
  };

  system.stateVersion = lib.mkDefault "24.11";
}
