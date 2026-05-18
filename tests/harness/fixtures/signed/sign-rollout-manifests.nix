{
  pkgs,
  lib,
  nixfleet-canonicalize,
  # Path to the signed fleet.resolved canonical bytes — used to compute
  # `fleetResolvedHash` for each manifest at build time.
  fleetCanonicalJson,
  # Attrset { channelName = manifestPayload; } where each payload is the
  # camelCase RolloutManifest struct (RFC-0012 §6.3 shape). The
  # `fleetResolvedHash` field MUST contain the sentinel literal
  # "__FLEET_RESOLVED_HASH__"; this derivation substitutes the actual
  # sha256 of `fleetCanonicalJson` at build time.
  manifestPayloads,
  # Attrset { channelName = "channel@channel_ref"; } — RolloutId per
  # RFC-0012 §6.3. The derivation writes `<rolloutId>.json` + `.sig`.
  rolloutIds,
  seedSalt ? "nixfleet-harness-test-seed-2026",
  name ? "nixfleet-harness-rollout-manifests-signed",
}: let
  seedHex = builtins.substring 0 64 (builtins.hashString "sha256" seedSalt);

  # Same hand-built PKCS#8 DER as sign-bytes.nix so the rollout
  # manifests are signed by the same ed25519 key the fleet.resolved
  # uses. test-trust.json's ciReleaseKey verifies both.
  keygen = pkgs.writers.writePython3 "ed25519-pkcs8-from-seed" {} ''
    import base64
    import sys

    seed = bytes.fromhex(sys.argv[1])
    assert len(seed) == 32
    der = bytes.fromhex("302e020100300506032b657004220420") + seed
    with open(sys.argv[2], "w") as f:
        f.write("-----BEGIN PRIVATE KEY-----\n")
        f.write(base64.b64encode(der).decode("ascii") + "\n")
        f.write("-----END PRIVATE KEY-----\n")
  '';

  channelNames = lib.attrNames manifestPayloads;
in
  pkgs.runCommand name {
    nativeBuildInputs = [pkgs.openssl pkgs.coreutils];
    inherit seedHex;
  } ''
    set -euo pipefail
    mkdir -p "$out"
    ${keygen} "$seedHex" privkey.pem

    fleet_hash=$(sha256sum < ${fleetCanonicalJson} | cut -d' ' -f1)

    ${lib.concatMapStringsSep "\n" (channel: let
        payload = manifestPayloads.${channel};
        rolloutId = rolloutIds.${channel};
      in ''
        cat > "${rolloutId}.raw.json" <<'PAYLOAD_EOF'
        ${builtins.toJSON payload}
        PAYLOAD_EOF
        sed -i "s|__FLEET_RESOLVED_HASH__|$fleet_hash|g" "${rolloutId}.raw.json"
        ${nixfleet-canonicalize}/bin/nixfleet-canonicalize \
          < "${rolloutId}.raw.json" > "$out/${rolloutId}.json"
        openssl pkeyutl -sign -rawin -inkey privkey.pem \
          -in "$out/${rolloutId}.json" \
          -out "$out/${rolloutId}.json.sig"
        siglen=$(stat -c %s "$out/${rolloutId}.json.sig")
        [ "$siglen" -eq 64 ] || { echo "bad sig length: $siglen" >&2; exit 1; }
      '')
      channelNames}
  ''
