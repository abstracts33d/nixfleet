# tests/harness/lib.nix
#
# Helpers for the microvm.nix-based fleet simulation harness.
#
# Builds lightweight microVM guests (cloud-hypervisor/qemu) that share the
# host's /nix/store over virtiofs, for cheap fleet-scale scenarios.
#
# Public attrs:
#   mkCpHostModule - NixOS module for the host VM that runs the CP stub
#   mkAgentNode    - build an agent microVM NixOS module (curls CP at boot)
#   mkFleetScenario- wrap CP-on-host + N agent microVMs into a runNixOSTest
#   mkHarnessCerts - builds a fleet CA + CP server cert + one client cert per hostname
#
# Node mix: `cp-real.nix` / `agent-real.nix` use the framework's
# `services.nixfleet-control-plane` / `services.nixfleet-agent`
# modules and are the path for any scenario that needs real binary
# semantics. `cp.nix` / `agent.nix` / `cp-signed.nix` /
# `agent-verify.nix` stay as stubs because the real CP doesn't expose
# `GET /` or `GET /canonical.json{,.sig}` (see those files' headers
# for the per-stub rationale).
{
  lib,
  pkgs,
  inputs,
}: let
  # Build a fleet CA + CP server cert + one client cert per hostname.
  # Deterministic and cached — the same `hostnames` list yields the same
  # derivation across scenarios. Inlined here (was previously in the now-
  # retired modules/tests/_lib/helpers.nix that served v0.1 VM scenarios).
  #
  # X.509 extension posture (load-bearing for cp-real.nix): the CP's
  # `WebPkiClientVerifier` strictly checks the chain's extensions —
  # the CA needs basicConstraints CA:TRUE + keyCertSign; client certs
  # need extendedKeyUsage clientAuth; server cert needs
  # extendedKeyUsage serverAuth. The earlier smoke / signed-roundtrip
  # scenarios used socat / Python http.server which did NOT enforce
  # any of this; cp-real.nix is the first consumer that does.
  # Explicit extensions below avoid relying on OpenSSL's default
  # behaviour (which has shifted across versions).
  mkTlsCerts = {hostnames ? ["cp" "agent-01" "agent-02"]}:
    pkgs.runCommand "nixfleet-harness-test-certs" {
      nativeBuildInputs = [pkgs.openssl];
    } ''
      mkdir -p $out

      # OpenSSL config files. Single combined config for the CA
      # (req section + extensions section, both required by
      # `openssl req -x509`). Per-cert-type extension files for the
      # `openssl x509 -req` step (uses `-extfile`, no config needed).
      cat > $out/ca.cnf <<'EOF'
      [req]
      distinguished_name = dn
      prompt = no
      x509_extensions = ca_ext

      [dn]
      CN = nixfleet-test-ca

      [ca_ext]
      basicConstraints = critical, CA:TRUE
      keyUsage = critical, keyCertSign, cRLSign, digitalSignature
      subjectKeyIdentifier = hash
      EOF
      cat > $out/server-ext.cnf <<'EOF'
      basicConstraints = critical, CA:FALSE
      keyUsage = critical, digitalSignature, keyEncipherment
      extendedKeyUsage = serverAuth
      subjectAltName = DNS:cp, DNS:localhost
      authorityKeyIdentifier = keyid
      EOF
      cat > $out/client-ext.cnf <<'EOF'
      basicConstraints = critical, CA:FALSE
      keyUsage = critical, digitalSignature, keyEncipherment
      extendedKeyUsage = clientAuth
      authorityKeyIdentifier = keyid
      EOF

      # Fleet CA (self-signed, EC P-256, explicit CA:TRUE).
      # `x509_extensions = ca_ext` in the [req] section is what
      # `openssl req -x509` reads to apply the CA-cert extensions
      # (basicConstraints CA:TRUE etc.) — no `-extensions` flag
      # needed when it's wired into the config.
      openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:prime256v1 \
        -keyout $out/ca-key.pem -out $out/ca.pem -days 365 -nodes \
        -config $out/ca.cnf

      # CP server cert (CN=cp, SAN includes cp + localhost for test curl).
      openssl req -newkey ec -pkeyopt ec_paramgen_curve:prime256v1 \
        -keyout $out/cp-key.pem -out $out/cp-csr.pem -nodes \
        -subj '/CN=cp'
      openssl x509 -req -in $out/cp-csr.pem -CA $out/ca.pem -CAkey $out/ca-key.pem \
        -CAcreateserial -out $out/cp-cert.pem -days 365 \
        -extfile $out/server-ext.cnf

      # Agent client certs (CN = hostname). Filter "cp" — its
      # cert is the server cert generated above; iterating "cp"
      # here would overwrite the server's cp-key.pem +
      # cp-cert.pem with a client cert that has no SANs, breaking
      # rustls's RFC 6125 SAN-only hostname validation. The
      # earlier smoke / signed-roundtrip scenarios survived this
      # because curl falls back to CN matching when SANs are
      # absent; reqwest + rustls (used by cp-real.nix's agents)
      # is stricter and requires SANs.
      ${lib.concatMapStringsSep "\n" (h: ''
          openssl req -newkey ec -pkeyopt ec_paramgen_curve:prime256v1 \
            -keyout $out/${h}-key.pem -out $out/${h}-csr.pem -nodes \
            -subj "/CN=${h}"
          openssl x509 -req -in $out/${h}-csr.pem -CA $out/ca.pem -CAkey $out/ca-key.pem \
            -CAcreateserial -out $out/${h}-cert.pem -days 365 \
            -extfile $out/client-ext.cnf
        '')
        (lib.filter (h: h != "cp") hostnames)}

      rm -f $out/*-csr.pem $out/*.srl $out/*-ext.cnf $out/ca.cnf
    '';

  # One cert set covering the harness hostnames. Additional hostnames get
  # added here as new scenarios land.
  mkHarnessCerts = {hostnames ? ["cp" "agent-01" "agent-02"]}:
    mkTlsCerts {inherit hostnames;};

  # Common microvm guest settings. Cloud-hypervisor is the default because
  # it has the lowest cold-start cost and supports virtiofs /nix/store sharing.
  # mem defaults to 256 MB per guest to fit the 16GB-dev-machine budget
  # (≤512MB per VM allows fleet-20 on a 16GB host).
  microvmGuestDefaults = {
    hypervisor = "qemu";
    mem = 256;
    vcpu = 1;
    # virtiofs share of the host /nix/store keeps cold-start nearly free;
    # the guest mounts it read-only and writes stateful paths elsewhere.
    shares = [
      {
        source = "/nix/store";
        mountPoint = "/nix/.ro-store";
        tag = "ro-store";
        proto = "virtiofs";
      }
    ];
    # Bridge-less user-mode networking; every guest sees the host via
    # qemu user-net's 10.0.2.2. Scenarios that need guest-to-guest
    # networking (future: canary rollback) will switch to tap/bridge.
    interfaces = [
      {
        type = "user";
        id = "vm-net";
        mac = "02:00:00:00:00:01";
      }
    ];
  };

  # CP runs on the host VM, not inside a microVM.
  #
  # Rationale: qemu user-mode networking isolates every microVM's
  # gateway (10.0.2.2) to the host VM itself — two user-net microVMs
  # cannot reach each other directly. Running the CP on the host VM
  # lets every agent microVM reach it via the shared user-net gateway
  # without bridge/NAT plumbing. The same placement applies whether
  # the CP is the smoke stub (cp.nix), the verify-artifact stub
  # (cp-signed.nix), or the real binary via the framework module
  # (cp-real.nix).
  mkCpHostModule = {
    testCerts,
    resolvedJsonPath,
  }: {
    imports = [./nodes/cp.nix];
    _module.args = {inherit testCerts resolvedJsonPath;};
  };

  # Signed-fixture CP: routes GET /canonical.json + /canonical.json.sig
  # from `signedFixture` (the derivation output at
  # tests/harness/fixtures/signed/default.nix). Used only by the
  # signed-roundtrip scenario; the smoke scenario keeps `mkCpHostModule`.
  mkSignedCpHostModule = {
    testCerts,
    signedFixture,
  }: {
    imports = [./nodes/cp-signed.nix];
    _module.args = {inherit testCerts signedFixture;};
  };

  # Real-binary CP host module. Runs the crane-built
  # `nixfleet-control-plane serve` binary against the signed fixture
  # with persistent SQLite state. Used by the teardown scenario;
  # future scenarios that need real CP semantics import this instead
  # of mkCpHostModule.
  mkRealCpHostModule = {
    testCerts,
    signedFixture,
    cpPkg,
    # Optional signed `revocations.json` sidecar fixture. When set,
    # cp-real.nix runs a static HTTP server alongside the CP and
    # configures `--revocations-*` to poll it. Scenarios that don't
    # exercise revocations recovery pass `null`.
    revocationsFixture ? null,
  }: {
    imports = [./nodes/cp-real.nix];
    _module.args = {inherit testCerts signedFixture cpPkg revocationsFixture;};
  };

  mkAgentNode = {
    testCerts,
    hostName,
    controlPlaneHost ? "10.0.2.2",
    controlPlanePort ? 8443,
    extraModules ? [],
  }: {
    imports =
      [
        ./nodes/agent.nix
      ]
      ++ extraModules;

    _module.args = {
      inherit testCerts controlPlaneHost controlPlanePort;
      harnessMicrovmDefaults = microvmGuestDefaults;
      agentHostName = hostName;
    };

    networking.hostName = hostName;
    system.stateVersion = lib.mkDefault "24.11";
  };

  # Verifying agent: fetches canonical.json + .sig from the signed CP,
  # loads /etc/nixfleet-harness/test-trust.json from `signedFixture`,
  # invokes the `nixfleet-verify-artifact` binary from
  # `verifyArtifactPkg` (crane-built) and logs the OK marker on success.
  #
  # `now` defaults to the fixture's `passthru.now` (signedAt + 1h) so
  # the freshness-window check passes regardless of how the fixture's
  # signedAt is parameterised. Stale-refusal scenarios override `now`
  # to a value past `signedAt + freshnessWindowSecs`.
  mkVerifyingAgentNode = {
    testCerts,
    hostName,
    signedFixture,
    verifyArtifactPkg,
    controlPlaneHost ? "10.0.2.2",
    controlPlanePort ? 8443,
    now ? signedFixture.now,
    freshnessWindowSecs ? 604800,
    extraModules ? [],
  }: {
    imports =
      [
        ./nodes/agent-verify.nix
      ]
      ++ extraModules;

    _module.args = {
      inherit
        testCerts
        controlPlaneHost
        controlPlanePort
        signedFixture
        verifyArtifactPkg
        now
        freshnessWindowSecs
        ;
      harnessMicrovmDefaults = microvmGuestDefaults;
      agentHostName = hostName;
    };

    networking.hostName = hostName;
    system.stateVersion = lib.mkDefault "24.11";
  };

  # Real-binary agent microVM. Runs the crane-built
  # `nixfleet-agent` against `cp-real`. Pre-placed certs (no
  # enrollment); poll loop ticks at `pollIntervalSecs` (default 10s
  # in the harness so scenarios don't wait the full 60s production
  # cadence between checkins).
  mkRealAgentNode = {
    testCerts,
    signedFixture,
    agentPkg,
    hostName,
    controlPlaneHost ? "10.0.2.2",
    controlPlanePort ? 8443,
    pollIntervalSecs ? 10,
    extraModules ? [],
  }: {
    imports =
      [
        ./nodes/agent-real.nix
      ]
      ++ extraModules;

    _module.args = {
      inherit inputs testCerts controlPlaneHost controlPlanePort agentPkg signedFixture pollIntervalSecs;
      harnessMicrovmDefaults = microvmGuestDefaults;
      agentHostName = hostName;
    };

    networking.hostName = hostName;
    system.stateVersion = lib.mkDefault "24.11";
  };

  # Convergence preseed: a NixOS module that overrides
  # /run/current-system to a path whose basename equals `closureHash`,
  # and writes `<state-dir>/last_confirmed_at` with that hash + the
  # given `attestedAt` RFC3339 timestamp.
  #
  # Together those two pieces let the agent's reported
  # `current_generation.closure_hash` match the fleet's declared
  # `hosts[*].closureHash` (set via the signed fixture's
  # `hostClosureHashes`) AND echo a known last_confirmed_at on every
  # checkin. Required for any scenario that asserts CP-side soak-state
  # attestation recovery; harmless for scenarios that don't, so it's
  # safe to apply pre-emptively to any agent that uses the converged
  # signed fixture.
  #
  # Returns a NixOS module; callers add it to `extraModules`.
  convergencePreseedModule = {
    closureHash,
    attestedAt ? "2026-04-01T00:00:00Z",
  }: {pkgs, ...}: {
    systemd.services.harness-agent-preseed = {
      description = "Pre-seed agent state-dir + override /run/current-system for convergence";
      wantedBy = ["multi-user.target"];
      before = ["nixfleet-agent.service"];
      after = ["local-fs.target"];
      # `requiredBy` makes the agent unit fail loudly if preseed
      # fails. Without it, read_last_confirmed silently returns
      # Ok(None) on a missing file and convergence-dependent
      # assertions would false-pass.
      requiredBy = ["nixfleet-agent.service"];
      serviceConfig = {
        Type = "oneshot";
        RemainAfterExit = true;
      };
      script = ''
        set -euo pipefail

        # Override /run/current-system. The symlink target doesn't
        # need to exist — the agent reads the symlink string via
        # fs::read_link and reports its basename.
        ${pkgs.coreutils}/bin/ln -sfn \
          /tmp/${closureHash} /run/current-system

        # Seed last_confirmed_at in the agent's state dir. Format
        # per crates/nixfleet-agent/src/checkin_state.rs:
        # <closure_hash>\n<rfc3339>\n. Two-line plaintext.
        ${pkgs.coreutils}/bin/mkdir -p /var/lib/nixfleet-agent
        ${pkgs.coreutils}/bin/chmod 0700 /var/lib/nixfleet-agent
        ${pkgs.coreutils}/bin/printf '%s\n%s\n' \
          '${closureHash}' '${attestedAt}' \
          > /var/lib/nixfleet-agent/last_confirmed_at
        ${pkgs.coreutils}/bin/chmod 0600 \
          /var/lib/nixfleet-agent/last_confirmed_at
      '';
    };
  };

  # Python helpers prepended to every scenario's `testScript` by
  # `mkFleetScenario`. Scenarios call `wait_for_journal_match` instead
  # of reimplementing the deadline-grep-sleep loop.
  #
  # On boot timing: agent service modules carry an `ExecStartPre` that
  # blocks on `ip route show default` until the guest has a default
  # route, so checkin attempts only fire once networking is genuinely
  # ready. This is the right place for the wait — guest-side, in
  # systemd's ordering — and removes the need for a test-driver-side
  # readiness gate. Scenarios use generous activity budgets that
  # cover boot + agent activity end-to-end.
  testScriptPrelude = ''
    import time

    def wait_for_journal_match(
        host,
        *,
        since_cursor,
        unit,
        pattern,
        timeout=60,
        sleep_secs=2,
        label=None,
    ):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            rc, _ = host.execute(
                "journalctl -u " + unit
                + " --since='" + since_cursor + "' --no-pager "
                + "| grep -E " + repr(pattern)
            )
            if rc == 0:
                return
            time.sleep(sleep_secs)
        dump = host.succeed(
            "journalctl -u " + unit
            + " --since='" + since_cursor + "' --no-pager"
        )
        msg = label if label is not None else pattern
        raise Exception(
            msg + " did not appear in " + unit + " within "
            + str(timeout) + "s\n=== " + unit + " journal ===\n"
            + dump + "\n=== end ==="
        )
  '';

  # Wrap a CP host-module + a list of agent microVM modules into a
  # runNixOSTest that boots the host and the agent microVMs. The CP stub
  # runs directly on the host VM (see mkCpHostModule for rationale);
  # agents run as microVMs sharing the host's /nix/store via virtiofs.
  #
  # Extension path: `agents` is an attrset of name -> <nix module>. For
  # fleet-N, the scenario file generates agent-01..agent-N programmatically
  # and passes them here. The CP host module is a single entry.
  # Default budget per microVM guest. Mirrors microvmGuestDefaults.mem
  # so callers can size the host RAM correctly without re-deriving
  # the per-guest figure.
  guestMemMB = 256;

  mkFleetScenario = {
    name,
    cpHostModule,
    agents,
    testScript,
    timeout ? 600,
    # Optional host-VM memory override. Default sizes per
    # `agents`-count: 4GB host + (count × guestMemMB), with a
    # 1GB minimum host overhead reservation. Fleet-2 lands at
    # 4GB; fleet-10 lands at ~7GB; fleet-50 (if attempted) would
    # land at ~17GB which hits the 16GB-dev-machine cap.
    hostMemoryMB ? null,
  }: let
    agentCount = builtins.length (builtins.attrNames agents);
    autoHostMemoryMB = lib.max 4096 (1024 + agentCount * guestMemMB + 2048);
    resolvedHostMemoryMB =
      if hostMemoryMB != null
      then hostMemoryMB
      else autoHostMemoryMB;
  in
    pkgs.testers.runNixOSTest {
      inherit name;
      node.specialArgs = {inherit inputs;};

      nodes.host = {pkgs, ...}: {
        imports = [
          inputs.microvm.nixosModules.host
          cpHostModule
        ];

        # The host VM needs KVM nested + enough disk for the microvm state
        # dirs + enough RAM to cover every guest's declared mem budget.
        virtualisation = {
          cores = 2;
          memorySize = resolvedHostMemoryMB;
          diskSize = 8192;
          qemu.options = [
            "-cpu"
            "kvm64,+svm,+vmx"
          ];
        };

        microvm.vms = lib.mapAttrs (_: mod: {config = mod;}) agents;

        environment.systemPackages = [pkgs.jq pkgs.curl];
      };

      testScript = testScriptPrelude + "\n" + testScript;
      meta.timeout = timeout;
    };
in {
  inherit
    convergencePreseedModule
    mkAgentNode
    mkCpHostModule
    mkFleetScenario
    mkHarnessCerts
    mkRealAgentNode
    mkRealCpHostModule
    mkSignedCpHostModule
    mkVerifyingAgentNode
    microvmGuestDefaults
    ;
}
