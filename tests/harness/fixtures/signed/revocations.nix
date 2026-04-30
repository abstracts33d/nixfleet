# Signed `revocations.json` sidecar for the teardown harness.
#
# Same JCS+ed25519 path as the main fleet fixture (shared seedSalt
# means both sidecars verify under the same `test-trust.json`). One
# entry against a synthetic revoked-host to make the post-restart
# replay assertion non-trivial; downstream consumers can override
# the entries list as needed.
{
  pkgs,
  nixfleet-canonicalize,
  signedAt ? "2026-05-01T00:00:00Z",
  seedSalt ? "nixfleet-harness-test-seed-2026",
}: let
  payload = {
    schemaVersion = 1;
    revocations = [
      {
        hostname = "decommissioned-laptop";
        notBefore = "2026-04-15T00:00:00Z";
        reason = "harness fixture: revoked-cert recovery test";
        revokedBy = "harness";
      }
    ];
    meta = {
      schemaVersion = 1;
      signedAt = signedAt;
      ciCommit = "0000000000000000000000000000000000000000";
      signatureAlgorithm = "ed25519";
    };
  };

  signed = import ./sign-bytes.nix {
    inherit pkgs nixfleet-canonicalize seedSalt;
    name = "nixfleet-harness-revocations-signed";
    jsonContent = builtins.toJSON payload;
  };
in
  pkgs.runCommand "nixfleet-harness-revocations-fixture" {} ''
    set -euo pipefail
    mkdir -p "$out"
    cp ${signed}/canonical.json "$out/revocations.json"
    cp ${signed}/canonical.json.sig "$out/revocations.json.sig"
  ''
