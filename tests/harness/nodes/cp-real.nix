# tests/harness/nodes/cp-real.nix
#
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
{
  lib,
  testCerts,
  signedFixture,
  cpPkg,
  ...
}: {
  # mTLS cert + signed-artifact + trust file + observed fallback at
  # stable paths. The CP reads them on each tick (trust + signed
  # artifact) so an operator-side rotation or commit propagates
  # without restart.
  environment.etc = {
    "nixfleet-cp/ca.pem".source = "${testCerts}/ca.pem";
    "nixfleet-cp/cp-cert.pem".source = "${testCerts}/cp-cert.pem";
    "nixfleet-cp/cp-key.pem".source = "${testCerts}/cp-key.pem";
    "nixfleet-cp/canonical.json".source = "${signedFixture}/canonical.json";
    "nixfleet-cp/canonical.json.sig".source = "${signedFixture}/canonical.json.sig";
    "nixfleet-cp/test-trust.json".source = "${signedFixture}/test-trust.json";
    # File-backed observed fallback (only consulted when both the
    # in-memory projection is empty AND no GitOps poll is wired).
    # Empty rollouts is the harness's steady state.
    "nixfleet-cp/observed.json".text = builtins.toJSON {
      channelRefs = {};
      lastRolledRefs = {};
      hostState = {};
      activeRollouts = [];
    };
    # Fleet CA cert + key for /v1/enroll (the harness's testCerts
    # CA doubles as the fleet CA). Symlinks to the same files as
    # ca.pem; the CP loads them via separate paths so an operator
    # can split CAs in production.
    "nixfleet-cp/fleet-ca-cert.pem".source = "${testCerts}/ca.pem";
    "nixfleet-cp/fleet-ca-key.pem".source = "${testCerts}/ca-key.pem";
  };

  # State directory survives systemctl restart but is wiped by the
  # teardown scenario via `rm -rf` on the journal-trigger path.
  systemd.tmpfiles.rules = [
    "d /var/lib/nixfleet-cp 0700 root root -"
  ];

  networking.firewall.allowedTCPPorts = [8443];

  systemd.services.nixfleet-control-plane = {
    description = "NixFleet control plane (real binary, harness teardown scenario)";
    wantedBy = ["multi-user.target"];
    after = ["network.target"];
    serviceConfig = {
      Type = "simple";
      # The CP is single-process; restart on failure so the
      # teardown scenario's `systemctl stop` + DB wipe + `start`
      # sequence behaves as if the operator manually restarted.
      Restart = "on-failure";
      RestartSec = 2;
      ExecStart = lib.concatStringsSep " " [
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
        # 7-day freshness window — same as the
        # mkVerifyingAgentNode default and the signedFixture's
        # 1-hour-stale tolerance. Big enough that future-dated
        # signedAt (the fixture's 2026-05-01) doesn't trip
        # against the harness's frozen clock.
        "--freshness-window-secs 604800"
      ];
      # Forward to console so the teardown scenario can grep the
      # host journal directly without parsing per-VM logs.
      StandardOutput = "journal+console";
      StandardError = "journal+console";
    };
  };

  system.stateVersion = lib.mkDefault "24.11";
}
