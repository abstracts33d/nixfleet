# tests/harness/nodes/agent-real.nix
#
# Real-binary agent microVM node. Runs the
# `nixfleet-agent` binary against `cp-real` over mTLS. Pre-placed
# client cert + key + CA + trust mean enrollment is skipped (the
# harness's mkTlsCerts hands the agent a CA-signed cert directly);
# the agent comes up, polls /v1/agent/checkin every poll_interval
# seconds, and reports its currentGeneration.
#
# Activation IS wired in the binary (activation.rs realises +
# switches + verifies + confirms); the harness avoids triggering
# it by giving the fleet fixture a `closureHash: null` per host —
# dispatch returns Decision::NoDeclaration and the agent never
# receives a target. Future scenarios that exercise activation
# fork this node and inject a closureHash that matches the
# microVM's actual /run/current-system.
#
# Network: same qemu user-net pattern as nodes/agent.nix —
# 10.0.2.2 from inside the microVM is the host VM where cp-real's
# systemd unit listens. --resolve maps the cert's CN ("cp") to
# that gateway IP.
{
  lib,
  testCerts,
  controlPlaneHost,
  controlPlanePort,
  harnessMicrovmDefaults,
  agentHostName,
  agentPkg,
  signedFixture,
  pollIntervalSecs ? 10,
  ...
}: {
  microvm = harnessMicrovmDefaults;

  environment.etc = {
    "nixfleet-agent/ca.pem".source = "${testCerts}/ca.pem";
    "nixfleet-agent/${agentHostName}-cert.pem".source = "${testCerts}/${agentHostName}-cert.pem";
    "nixfleet-agent/${agentHostName}-key.pem".source = "${testCerts}/${agentHostName}-key.pem";
    "nixfleet-agent/test-trust.json".source = "${signedFixture}/test-trust.json";
  };

  # The agent's `--resolve cp:8443:<gateway-ip>` flag-equivalent is
  # /etc/hosts: map "cp" → 10.0.2.2 so the binary's reqwest client
  # connects to the right address while satisfying the cert SAN
  # (CN=cp, SAN includes "cp" + "localhost").
  networking.hosts."${controlPlaneHost}" = ["cp"];

  systemd.services.nixfleet-agent = {
    description = "NixFleet agent (real binary, harness teardown scenario)";
    wantedBy = ["multi-user.target"];
    after = ["network.target"];
    serviceConfig = {
      Type = "simple";
      Restart = "on-failure";
      RestartSec = 5;
      # StateDirectory creates /var/lib/nixfleet-agent at unit
      # start with mode 0700; the agent writes `last_confirmed_at`
      # (the soak-recovery attestation echoed on every checkin) here.
      StateDirectory = "nixfleet-agent";
      StateDirectoryMode = "0700";
      ExecStart = lib.concatStringsSep " " [
        "${agentPkg}/bin/nixfleet-agent"
        "--control-plane-url https://cp:${toString controlPlanePort}"
        "--machine-id ${agentHostName}"
        "--poll-interval ${toString pollIntervalSecs}"
        "--trust-file /etc/nixfleet-agent/test-trust.json"
        "--ca-cert /etc/nixfleet-agent/ca.pem"
        "--client-cert /etc/nixfleet-agent/${agentHostName}-cert.pem"
        "--client-key /etc/nixfleet-agent/${agentHostName}-key.pem"
        "--state-dir /var/lib/nixfleet-agent"
      ];
      # Surface in host journal via microvm@<name>.service so the
      # scenario can grep without per-VM journal mounts.
      StandardOutput = "journal+console";
      StandardError = "journal+console";
    };
  };

  system.stateVersion = lib.mkDefault "24.11";
}
