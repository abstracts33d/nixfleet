# Auditor offline-chain demonstration: verify-artifact `probe` mode
# accepts a well-formed signed compliance payload and rejects a
# byte-flipped copy. The load-bearing property is that an auditor
# can reconstruct the host↔probes link without CP access; this
# scenario asserts the CLI surface honors that contract.
#
# Pure runCommand — the verify path is offline by definition; no
# microvm or networking required.
{
  pkgs,
  probeFixture,
  verifyArtifactPkg,
  ...
}:
pkgs.runCommand "fleet-harness-auditor-chain" {} ''
  set -euo pipefail

  # Happy path: signature + pubkey + canonical payload all match.
  ${verifyArtifactPkg}/bin/nixfleet-verify-artifact probe \
    --payload ${probeFixture}/payload.canonical.json \
    --signature ${probeFixture}/payload.sig.b64 \
    --pubkey ${probeFixture}/pubkey.openssh

  # Tamper one byte of the canonical payload; the verifier must
  # reject. Exit-code 1 is the "verify failed" contract per the
  # CLI's spec §6 — we invert it so a *non-zero* exit is the pass.
  cp ${probeFixture}/payload.canonical.json tampered.json
  chmod +w tampered.json
  printf '\x00' | dd of=tampered.json bs=1 count=1 conv=notrunc 2>/dev/null
  if ${verifyArtifactPkg}/bin/nixfleet-verify-artifact probe \
    --payload tampered.json \
    --signature ${probeFixture}/payload.sig.b64 \
    --pubkey ${probeFixture}/pubkey.openssh; then
    echo "FAIL: tampered payload was accepted by verify-artifact probe" >&2
    exit 1
  fi

  touch "$out"
''
