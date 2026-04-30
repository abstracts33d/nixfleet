# Corruption-rejection demonstration: verify-artifact `artifact`
# mode rejects a tampered fleet.resolved (canonical bytes mutated)
# AND a tampered signature (sig bytes mutated). The load-bearing
# property is that an attacker swapping the signed artifact for a
# fake cannot pass verify; this scenario asserts the CLI surface
# honors that.
#
# Pure runCommand — verify is offline by definition; no microvm
# required. Mirrors the auditor-chain pattern.
{
  pkgs,
  signedFixture,
  verifyArtifactPkg,
  ...
}: let
  # The signed fixture's frozen `meta.signedAt` is 2026-05-01T00:00Z;
  # use 1h later as `now` so the freshness gate doesn't trip.
  now = "2026-05-01T01:00:00Z";
  freshnessWindowSecs = 604800;
in
  pkgs.runCommand "fleet-harness-corruption-rejection" {} ''
    set -euo pipefail

    # Control: well-formed inputs verify (exit 0). Confirms the
    # invocation shape is right before we test rejection.
    ${verifyArtifactPkg}/bin/nixfleet-verify-artifact artifact \
      --artifact ${signedFixture}/canonical.json \
      --signature ${signedFixture}/canonical.json.sig \
      --trust-file ${signedFixture}/test-trust.json \
      --now ${now} \
      --freshness-window-secs ${toString freshnessWindowSecs}

    # Tamper one byte deep inside canonical.json. Offset 50 sits
    # inside the JSON body (well past the opening `{"meta"`); XORing
    # 0x01 keeps the byte printable and the JSON loosely parseable
    # so the verifier reaches the signature step rather than failing
    # at parse. The signed bytes diverge from the canonical, so
    # verify must return BadSignature.
    cp ${signedFixture}/canonical.json tampered-canonical.json
    chmod +w tampered-canonical.json
    printf '\x01' | dd of=tampered-canonical.json bs=1 count=1 seek=50 \
      conv=notrunc 2>/dev/null
    if ${verifyArtifactPkg}/bin/nixfleet-verify-artifact artifact \
      --artifact tampered-canonical.json \
      --signature ${signedFixture}/canonical.json.sig \
      --trust-file ${signedFixture}/test-trust.json \
      --now ${now} \
      --freshness-window-secs ${toString freshnessWindowSecs}; then
      echo "FAIL: tampered canonical.json was accepted by verify-artifact" >&2
      exit 1
    fi

    # Tamper one byte of the 64-byte raw signature. The s-component
    # (bytes 32..64) is the canonical scalar; mutating any byte
    # there breaks both the strict-form check and the verify check.
    cp ${signedFixture}/canonical.json.sig tampered.sig
    chmod +w tampered.sig
    printf '\x01' | dd of=tampered.sig bs=1 count=1 seek=10 \
      conv=notrunc 2>/dev/null
    if ${verifyArtifactPkg}/bin/nixfleet-verify-artifact artifact \
      --artifact ${signedFixture}/canonical.json \
      --signature tampered.sig \
      --trust-file ${signedFixture}/test-trust.json \
      --now ${now} \
      --freshness-window-secs ${toString freshnessWindowSecs}; then
      echo "FAIL: tampered signature was accepted by verify-artifact" >&2
      exit 1
    fi

    touch "$out"
  ''
