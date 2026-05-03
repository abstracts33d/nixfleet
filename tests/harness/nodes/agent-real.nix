# Real-binary agent microVM node. Drives the framework's
# `services.nixfleet-agent` module (modules/scopes/nixfleet/_agent.nix)
# so the harness boots the same systemd unit + ExecStart shape operators
# get when consuming nixfleet.
#
# Talks to `cp-real` over mTLS. Pre-placed client cert + key + CA + trust
# mean enrollment is skipped (the harness's mkTlsCerts hands the agent a
# CA-signed cert directly); the agent comes up, polls /v1/agent/checkin
# every poll_interval seconds, and reports its currentGeneration.
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
# systemd unit listens. /etc/hosts maps the cert's CN ("cp") to
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
  imports = [
    # Trust-root + persistence option schemas the framework module
    # reads. Defaults (all keys null) are fine: the test points
    # `trustFile` at the fixture's pre-built test-trust.json so the
    # module's auto-generated trust.json is unused.
    ../../../contracts/trust.nix
    ../../../contracts/persistence.nix
    ../../../modules/scopes/nixfleet/_agent.nix
  ];

  microvm = harnessMicrovmDefaults;

  # Without explicit DHCP, qemu user-net's DHCP server is ignored by
  # the guest's stack; the guest comes up with no IP, agent's first
  # checkin returns ENETUNREACH ("Network is unreachable"). The
  # framework module's `after = network-online.target` doesn't help
  # — network-online without a DHCP client times out and fires
  # anyway, so the agent runs against an unconfigured stack.
  networking.useDHCP = lib.mkDefault true;

  environment.etc = {
    "nixfleet-agent/ca.pem".source = "${testCerts}/ca.pem";
    "nixfleet-agent/${agentHostName}-cert.pem".source = "${testCerts}/${agentHostName}-cert.pem";
    "nixfleet-agent/${agentHostName}-key.pem".source = "${testCerts}/${agentHostName}-key.pem";
    "nixfleet-agent/test-trust.json".source = "${signedFixture}/test-trust.json";
  };

  # The agent's reqwest client connects to "cp" — /etc/hosts pins it
  # to the qemu user-net gateway IP (10.0.2.2 by default). Cert SAN
  # (CN=cp, SAN includes "cp" + "localhost") accepts the hostname.
  networking.hosts."${controlPlaneHost}" = ["cp"];

  services.nixfleet-agent = {
    enable = true;
    package = agentPkg;
    controlPlaneUrl = "https://cp:${toString controlPlanePort}";
    machineId = agentHostName;
    pollInterval = pollIntervalSecs;
    trustFile = "/etc/nixfleet-agent/test-trust.json";
    stateDir = "/var/lib/nixfleet-agent";
    tls = {
      caCert = "/etc/nixfleet-agent/ca.pem";
      clientCert = "/etc/nixfleet-agent/${agentHostName}-cert.pem";
      clientKey = "/etc/nixfleet-agent/${agentHostName}-key.pem";
    };
  };

  # Surface the agent's journal in the host VM via console forwarding
  # so scenarios can grep `microvm@<name>.service` without per-VM
  # journal mounts. The framework module's serviceConfig doesn't set
  # this (production deploys go through journald → vector → rsyslog),
  # so override here for the harness.
  systemd.services.nixfleet-agent.serviceConfig = {
    StandardOutput = "journal+console";
    StandardError = "journal+console";
  };

  system.stateVersion = lib.mkDefault "24.11";
}
