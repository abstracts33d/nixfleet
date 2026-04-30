# Factored JCS+ed25519 signing path for harness fixtures.
#
# Output: a derivation containing `canonical.json`, `canonical.json.sig`,
# and `pubkey.b64` (the 32-byte raw verify key, base64). All bytes are a
# pure function of `jsonContent` + `seedSalt`; same seed → same key, so
# multiple sidecars (fleet.resolved + revocations.json + ...) signed with
# the default seed verify under one shared `test-trust.json`.
{
  pkgs,
  nixfleet-canonicalize,
  jsonContent,
  name,
  seedSalt ? "nixfleet-harness-test-seed-2026",
}: let
  seedHex = builtins.substring 0 64 (builtins.hashString "sha256" seedSalt);

  # RFC 8410 §7 PKCS#8 wrap of an ed25519 seed. OpenSSL 3 won't take a
  # caller-supplied seed for `genpkey` (openssl/openssl#18333), so we
  # hand-build the 48-byte DER (16-byte prefix + 32-byte seed).
  keygen = pkgs.writers.writePython3 "ed25519-pkcs8-from-seed" {} ''
    import base64, sys
    seed = bytes.fromhex(sys.argv[1])
    assert len(seed) == 32
    der = bytes.fromhex("302e020100300506032b657004220420") + seed
    with open(sys.argv[2], "w") as f:
        f.write("-----BEGIN PRIVATE KEY-----\n")
        f.write(base64.b64encode(der).decode("ascii") + "\n")
        f.write("-----END PRIVATE KEY-----\n")
  '';
in
  pkgs.runCommand name {
    nativeBuildInputs = [pkgs.openssl];
    passAsFile = ["jsonContent"];
    inherit jsonContent seedHex;
  } ''
    set -euo pipefail
    mkdir -p "$out"
    ${keygen} "$seedHex" privkey.pem

    # JCS canonicalize via the pinned shell tool, then ed25519-sign the
    # raw bytes. Sig is exactly 64 bytes; sanity-check before exit.
    cp "$jsonContentPath" stamped.json
    ${nixfleet-canonicalize}/bin/nixfleet-canonicalize \
      < stamped.json > "$out/canonical.json"
    openssl pkeyutl -sign -rawin -inkey privkey.pem \
      -in "$out/canonical.json" -out "$out/canonical.json.sig"
    siglen=$(stat -c %s "$out/canonical.json.sig")
    [ "$siglen" -eq 64 ] || { echo "bad sig length: $siglen" >&2; exit 1; }

    # Strip SPKI header to get the raw 32-byte pubkey, base64.
    openssl pkey -in privkey.pem -pubout -outform DER -out pubkey.spki.der
    tail -c 32 pubkey.spki.der | base64 -w0 > "$out/pubkey.b64"
  ''
