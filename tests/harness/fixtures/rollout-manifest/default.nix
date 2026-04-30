# Pre-signed rollout-manifest fixture for the manifest-tamper-rejection
# scenario. Produces the artifacts a single test rollout needs:
#
#   - manifest.canonical.json  — JCS-canonical RolloutManifest bytes
#   - manifest.canonical.json.sig — raw ed25519 signature
#   - pubkey.b64               — 32-byte raw verify key, base64 (SPKI-stripped)
#   - rollout-id               — hex sha256 of canonical bytes (the rolloutId)
#   - trust.json               — TrustConfig with the pubkey wired as ciReleaseKey.current
#   - signed-at                — RFC3339 string used in the manifest's meta (test passes as --now)
#
# All bytes are a pure function of `seedSalt` + the embedded payload below.
# Same seed → same key → same signature → same rolloutId. Reuses the
# shared sign-bytes primitive so this fixture verifies under any
# trust.json minted with the same seed.
{
  lib,
  pkgs,
  nixfleet-canonicalize,
  seedSalt ? "nixfleet-harness-test-seed-2026",
  signedAt ? "2026-05-01T00:00:00Z",
  derivationName ? "nixfleet-harness-rollout-manifest-fixture",
}: let
  signBytes = import ../signed/sign-bytes.nix;

  # Sample RolloutManifest. Two hosts in distinct waves, deterministic
  # closure hashes. The fleet_resolved_hash is opaque to the verifier
  # (it just has to be present); pick a stable test value.
  manifestPayload = {
    schemaVersion = 1;
    displayName = "stable@def4567";
    channel = "stable";
    channelRef = "def4567abc123def4567abc123def4567abc123d";
    fleetResolvedHash =
      "1111111111111111111111111111111111111111111111111111111111111111";
    hostSet = [
      {
        hostname = "agent-01";
        waveIndex = 0;
        targetClosure = "0000000000000000000000000000000000000000-host-a";
      }
      {
        hostname = "agent-02";
        waveIndex = 1;
        targetClosure = "1111111111111111111111111111111111111111-host-b";
      }
    ];
    healthGate = {};
    complianceFrameworks = ["anssi-bp028"];
    meta = {
      schemaVersion = 1;
      signedAt = signedAt;
      ciCommit = "def45678";
      signatureAlgorithm = "ed25519";
    };
  };

  signed = signBytes {
    inherit pkgs nixfleet-canonicalize seedSalt;
    name = "${derivationName}-signed";
    jsonContent = builtins.toJSON manifestPayload;
  };
in
  pkgs.runCommand derivationName {
    nativeBuildInputs = [pkgs.coreutils];
  } ''
    set -euo pipefail
    mkdir -p "$out"

    cp "${signed}/canonical.json"      "$out/manifest.canonical.json"
    cp "${signed}/canonical.json.sig"  "$out/manifest.canonical.json.sig"
    cp "${signed}/pubkey.b64"          "$out/pubkey.b64"

    # rolloutId is sha256(canonical bytes), hex lowercase.
    sha256sum "$out/manifest.canonical.json" \
      | cut -d' ' -f1 > "$out/rollout-id"

    # Minimal trust.json: pubkey wired as ciReleaseKey.current. Same
    # shape as the production trust roots an agent loads via
    # --trust-file. Cache + org-root keys absent (manifest verify
    # only needs ciReleaseKey).
    pubkey=$(cat "$out/pubkey.b64")
    cat > "$out/trust.json" <<EOF
    {
      "schemaVersion": 1,
      "ciReleaseKey": {
        "current": { "algorithm": "ed25519", "public": "$pubkey" },
        "previous": null,
        "rejectBefore": null
      },
      "cacheKeys": [],
      "orgRootKey": null
    }
    EOF

    # Surface the embedded signed-at as a discoverable file the test
    # can `cat` and pass to --now without parsing the manifest JSON.
    printf '%s' '${signedAt}' > "$out/signed-at"
  ''
