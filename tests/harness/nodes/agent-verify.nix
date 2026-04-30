# Signed-roundtrip agent microVM. At boot, fetches `canonical.json` and
# `canonical.json.sig` from the CP over mTLS, stages `test-trust.json`
# from the signed fixture, then runs `nixfleet-verify-artifact`. On
# successful verify the unit emits `harness-roundtrip-ok:
# schemaVersion=<n> hosts=<n>` — the scenario testScript greps for the
# marker.
#
# TODO: retire this module when the agent inlines the verify call
# site directly (the CLI is harness scaffolding).
{
  lib,
  pkgs,
  testCerts,
  controlPlaneHost,
  controlPlanePort,
  harnessMicrovmDefaults,
  agentHostName,
  signedFixture,
  verifyArtifactPkg,
  # Resolved by mkVerifyingAgentNode: `now` defaults to
  # `signedFixture.now` (signedAt + 1h) so the freshness gate passes
  # for any fixture variant. NixOS's module system resolves function
  # arguments through `_module.args` and does not consult
  # module-function defaults — so these are unconditionally required
  # at this layer.
  now,
  freshnessWindowSecs,
  ...
}: {
  microvm = harnessMicrovmDefaults;

  environment.etc = {
    "nixfleet-harness/ca.pem".source = "${testCerts}/ca.pem";
    "nixfleet-harness/${agentHostName}-cert.pem".source = "${testCerts}/${agentHostName}-cert.pem";
    "nixfleet-harness/${agentHostName}-key.pem".source = "${testCerts}/${agentHostName}-key.pem";
    "nixfleet-harness/test-trust.json".source = "${signedFixture}/test-trust.json";
  };

  systemd.services.harness-agent = {
    description = "Nixfleet harness agent (verifies signed artifact)";
    wantedBy = ["multi-user.target"];
    after = ["network.target"];
    path = [pkgs.curl pkgs.coreutils verifyArtifactPkg];
    serviceConfig = {
      Type = "oneshot";
      RemainAfterExit = true;
      StandardOutput = "journal+console";
      StandardError = "journal+console";
      ExecStart = pkgs.writeShellScript "harness-agent-verify" ''
        set -euo pipefail

        base="https://cp:${toString controlPlanePort}"
        workdir=$(mktemp -d)
        trap 'rm -rf "$workdir"' EXIT

        fetch() {
          local url_path="$1" out="$2"
          curl -sfS \
            --cacert /etc/nixfleet-harness/ca.pem \
            --cert /etc/nixfleet-harness/${agentHostName}-cert.pem \
            --key /etc/nixfleet-harness/${agentHostName}-key.pem \
            --resolve "cp:${toString controlPlanePort}:${controlPlaneHost}" \
            --connect-timeout 30 \
            --max-time 60 \
            "$base$url_path" -o "$out"
        }

        # Wait until the CP is reachable before starting the verify
        # flow. Microvm guest boots independently of the host's CP
        # service so the first agent attempt can race a not-yet-up
        # CP — that's a harness-only ordering artefact, not a
        # production failure mode. Budget: 60s (30 attempts × 2s).
        # Beyond that, treat as a real outage and emit FAIL.
        echo "harness-agent: waiting for CP to accept TLS" >&2
        for attempt in $(seq 1 30); do
          if curl -sfS \
            --cacert /etc/nixfleet-harness/ca.pem \
            --cert /etc/nixfleet-harness/${agentHostName}-cert.pem \
            --key /etc/nixfleet-harness/${agentHostName}-key.pem \
            --resolve "cp:${toString controlPlanePort}:${controlPlaneHost}" \
            --connect-timeout 2 --max-time 4 \
            -o /dev/null "$base/canonical.json" 2>/dev/null; then
            break
          fi
          if [ "$attempt" -eq 30 ]; then
            echo "harness-roundtrip-FAIL: CP unreachable after 60s" >&2
            exit 1
          fi
          sleep 2
        done

        echo "harness-agent: fetching signed artifact from $base" >&2
        if ! fetch /canonical.json "$workdir/artifact"; then
          echo "harness-roundtrip-FAIL: canonical.json fetch failed" >&2
          exit 1
        fi
        if ! fetch /canonical.json.sig "$workdir/signature"; then
          echo "harness-roundtrip-FAIL: canonical.json.sig fetch failed" >&2
          exit 1
        fi

        sig_len=$(stat -c %s "$workdir/signature")
        if [ "$sig_len" != 64 ]; then
          echo "harness-roundtrip-FAIL: expected 64-byte signature, got $sig_len" >&2
          exit 1
        fi

        echo "harness-agent: running nixfleet-verify-artifact" >&2
        verify_out=$(nixfleet-verify-artifact artifact \
          --artifact "$workdir/artifact" \
          --signature "$workdir/signature" \
          --trust-file /etc/nixfleet-harness/test-trust.json \
          --now ${now} \
          --freshness-window-secs ${toString freshnessWindowSecs})

        # Belt-and-suspenders: also write to /dev/console so the marker
        # reaches the host journal even if journald forwarding from the
        # guest is disabled (same pattern as nodes/agent.nix).
        msg="harness-roundtrip-ok: $verify_out"
        echo "$msg" >&2
        echo "$msg" > /dev/console || true
      '';
      Restart = "on-failure";
      RestartSec = 5;
    };
  };

  system.stateVersion = lib.mkDefault "24.11";
}
