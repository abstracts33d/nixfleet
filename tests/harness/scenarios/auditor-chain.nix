# §8 done-criterion #2: auditor traces host↔closure↔commit↔probes
# offline. Demonstrates the verify-artifact `probe` mode rejects a
# tampered payload and accepts a well-formed one — the load-bearing
# property an auditor relies on when reconstructing the chain
# without CP access.
#
# This is intentionally a pure runCommand check, not a microvm
# scenario: the verify path is offline by definition, no nodes or
# networking required. Faster to build, cheaper to run, same
# coverage of the contract.
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
