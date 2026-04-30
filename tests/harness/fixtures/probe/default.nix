# Pre-signed probe-output fixture for criterion #2 (auditor offline
# chain). Demonstrates an ed25519-signed compliance event payload that
# verifies under a host's OpenSSH-format pubkey — the contract the
# auditor follows when reconstructing the host↔probes link without CP
# access.
#
# Output: payload.canonical.json (JCS bytes), payload.sig.b64 (base64
# raw signature), pubkey.openssh (OpenSSH-format ed25519 pubkey).
#
# Determinism: every byte is a function of `seedSalt` + the embedded
# payload below. Same seed → same key → same signature.
{
  pkgs,
  nixfleet-canonicalize,
  seedSalt ? "nixfleet-harness-probe-host-seed-2026",
}: let
  seedHex = builtins.substring 0 64 (builtins.hashString "sha256" seedSalt);

  # Sample ComplianceFailureSignedPayload — shape mirrors
  # crates/nixfleet-proto/src/evidence_signing.rs. The CLI verifier
  # is shape-agnostic (it canonicalizes whatever JSON it gets and
  # matches against the signature), so this stands in for any signed
  # probe output.
  payload = {
    hostname = "agent-01";
    rollout = "stable@deadbeef";
    controlId = "auditLogging";
    status = "non-compliant";
    frameworkArticles = [];
    evidenceCollectedAt = "2026-04-01T00:00:00Z";
    evidenceSnippetSha256 = "deadbeef";
  };

  keygen = pkgs.writers.writePython3 "ed25519-pkcs8-from-seed-probe" {} ''
    import base64, sys
    seed = bytes.fromhex(sys.argv[1])
    der = bytes.fromhex("302e020100300506032b657004220420") + seed
    with open(sys.argv[2], "w") as f:
        f.write("-----BEGIN PRIVATE KEY-----\n")
        f.write(base64.b64encode(der).decode("ascii") + "\n")
        f.write("-----END PRIVATE KEY-----\n")
  '';

  # OpenSSH wire format for ed25519 pubkeys: 4-byte length-prefix
  # "ssh-ed25519" string + 4-byte length-prefix raw 32-byte pubkey,
  # then base64-wrapped, then "ssh-ed25519 <b64> <comment>".
  toOpenssh = pkgs.writers.writePython3 "raw-to-openssh-ed25519" {} ''
    import base64, struct, sys
    raw = base64.b64decode(open(sys.argv[1]).read().strip())
    wire = struct.pack(">I", 11) + b"ssh-ed25519" + struct.pack(">I", 32) + raw
    print("ssh-ed25519", base64.b64encode(wire).decode(), "harness-host")
  '';
in
  pkgs.runCommand "nixfleet-harness-probe-fixture" {
    nativeBuildInputs = [pkgs.openssl];
    payloadJson = builtins.toJSON payload;
    passAsFile = ["payloadJson"];
    inherit seedHex;
  } ''
    set -euo pipefail
    mkdir -p "$out"

    ${keygen} "$seedHex" privkey.pem

    # Raw 32-byte pubkey from PKCS#8 SPKI — strip the 12-byte header.
    openssl pkey -in privkey.pem -pubout -outform DER -out pubkey.spki.der
    tail -c 32 pubkey.spki.der | base64 -w0 > pubkey.raw.b64
    ${toOpenssh} pubkey.raw.b64 > "$out/pubkey.openssh"

    cp "$payloadJsonPath" payload.json
    ${nixfleet-canonicalize}/bin/nixfleet-canonicalize \
      < payload.json > "$out/payload.canonical.json"
    openssl pkeyutl -sign -rawin -inkey privkey.pem \
      -in "$out/payload.canonical.json" -out sig.bin
    base64 -w0 sig.bin > "$out/payload.sig.b64"
  ''
