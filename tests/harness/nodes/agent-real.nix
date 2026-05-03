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
  pkgs,
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

  # microvm.nix uses systemd-networkd by default but doesn't
  # auto-configure DHCP on the user-net interface; without explicit
  # config the guest's stack ignores qemu's DHCP offers and the
  # agent's first checkin returns ENETUNREACH (os error 101).
  # `networking.useDHCP` doesn't take effect under networkd; we
  # have to give networkd an explicit .network unit.
  #
  # `RequiredForOnline = "routable"` is load-bearing: the default
  # ("degraded") makes network-online.target fire even when no IP
  # is assigned, masking the DHCP failure as an agent-level bug.
  # "routable" requires an actual default route, gating the agent
  # service correctly via its `wants = network-online.target`.
  networking.useNetworkd = lib.mkDefault true;
  systemd.network.networks."10-vm-net" = {
    matchConfig.Name = "en* eth*";
    networkConfig.DHCP = "yes";
    linkConfig.RequiredForOnline = "routable";
  };

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
  #
  # `ExecStartPre` waits for an actual default route. systemd's
  # `network-online.target` and `RequiredForOnline=routable` both
  # fire prematurely in the qemu user-net + microvm.nix combo —
  # the agent otherwise hits ENETUNREACH on its first checkin and
  # spends its retry budget waiting for DHCP that already fired. The
  # explicit gate makes networking-up the precondition for the
  # agent's main process actually starting, so the test budget
  # measures agent activity rather than DHCP timing.
  systemd.services.nixfleet-agent.serviceConfig = {
    StandardOutput = "journal+console";
    StandardError = "journal+console";
    ExecStartPre = "${pkgs.bash}/bin/bash -c 'for i in $(seq 1 60); do ${pkgs.iproute2}/bin/ip route show default | grep -q . && exit 0; sleep 1; done; echo \"agent: no default route after 60s\" >&2; exit 1'";
  };

  system.stateVersion = lib.mkDefault "24.11";
}
