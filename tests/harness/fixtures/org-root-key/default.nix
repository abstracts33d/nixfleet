# Deterministic org-root ed25519 keypair fixture for harness scenarios
# that exercise `/v1/enroll` (bootstrap-token-signed enrolment).
#
# Output:
#   - private.pem        — PKCS#8-wrapped ed25519 seed, the input
#                          `nixfleet-mint-token --org-root-key` reads
#   - pubkey.b64         — 32-byte raw verify key, base64. Wired into
#                          `trust.json::orgRootKey.current.public`
#
# All bytes are a pure function of `seedSalt`. Same seed → same key,
# so different scenarios can mint tokens against the same trust file
# without coordinating private-material distribution.
#
# This fixture deliberately does NOT bake a trust.json — callers
# typically need to splice both `orgRootKey.current` (this fixture's
# pubkey) AND `ciReleaseKey.current` (the signedFixture's pubkey)
# into a single trust.json so the CP can boot against the harness
# signed fleet AND accept enrolment tokens. That stitching happens
# in the calling default.nix at runCommand time.
{
  pkgs,
  seedSalt ? "nixfleet-harness-org-root-seed-2026",
  derivationName ? "nixfleet-harness-org-root-key",
}: let
  seedHex = builtins.substring 0 64 (builtins.hashString "sha256" seedSalt);

  # Same RFC 8410 §7 PKCS#8 wrap as sign-bytes.nix. Hand-built DER
  # because openssl 3 won't accept a caller-supplied seed for
  # `genpkey` (openssl/openssl#18333).
  keygen = pkgs.writers.writePython3 "ed25519-pkcs8-from-seed-org-root" {} ''
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
in
  pkgs.runCommand derivationName {
    nativeBuildInputs = [pkgs.openssl pkgs.coreutils];
    inherit seedHex;
  } ''
    set -euo pipefail
    mkdir -p "$out"
    ${keygen} "$seedHex" "$out/private.pem"

    # Strip SPKI header to surface the raw 32-byte pubkey, base64.
    openssl pkey -in "$out/private.pem" -pubout -outform DER -out pubkey.spki.der
    tail -c 32 pubkey.spki.der | base64 -w0 > "$out/pubkey.b64"
  ''
