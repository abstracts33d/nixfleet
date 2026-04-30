# Manifest-tamper-rejection scenario.
#
# Validates the offline auditor side of the rollout-manifest contract
# (RFC-0002 §4.4 / RFC-0003 §4.6 / CONTRACTS.md §I #8): given a signed
# manifest + signature + trust.json + an expected rolloutId, the
# `nixfleet-verify-artifact rollout-manifest` CLI:
#
#   1. Accepts a well-formed pair.
#   2. Rejects a byte-tampered manifest (signature breaks).
#   3. Rejects a byte-tampered signature (signature breaks).
#   4. Rejects a content-address mismatch (operator passes wrong
#      `--rollout-id`).
#
# Pure runCommand — same shape as fleet-harness-auditor-chain. The
# verify path is offline by definition; no microvm or networking.
# Agent-side end-to-end (fetch from CP + cache + emit on failure)
# is covered by the unit tests in nixfleet_agent::manifest_cache and
# the integration test path in tests/checkin.rs (workspace `cargo
# test`). This scenario adds the auditor-chain coverage: any operator
# with the trust roots can reproduce verify outside the fleet's
# running infrastructure.
{
  pkgs,
  rolloutManifestFixture,
  verifyArtifactPkg,
  ...
}:
pkgs.runCommand "fleet-harness-manifest-tamper-rejection" {} ''
  set -euo pipefail

  rid=$(cat ${rolloutManifestFixture}/rollout-id)
  signedAt=$(cat ${rolloutManifestFixture}/signed-at)

  # Step 1: well-formed pair must verify.
  ${verifyArtifactPkg}/bin/nixfleet-verify-artifact rollout-manifest \
    --manifest ${rolloutManifestFixture}/manifest.canonical.json \
    --signature ${rolloutManifestFixture}/manifest.canonical.json.sig \
    --trust-file ${rolloutManifestFixture}/trust.json \
    --now "$signedAt" \
    --freshness-window-secs 86400 \
    --rollout-id "$rid"

  # Step 2: byte-flipped manifest. Pick offset 50 (well past `{"meta`
  # opener; lands in JSON body). Same recipe as
  # tests/harness/scenarios/corruption-rejection.nix.
  cp ${rolloutManifestFixture}/manifest.canonical.json tampered-manifest.json
  chmod +w tampered-manifest.json
  printf '\x01' | dd of=tampered-manifest.json bs=1 count=1 seek=50 \
    conv=notrunc 2>/dev/null
  if ${verifyArtifactPkg}/bin/nixfleet-verify-artifact rollout-manifest \
       --manifest tampered-manifest.json \
       --signature ${rolloutManifestFixture}/manifest.canonical.json.sig \
       --trust-file ${rolloutManifestFixture}/trust.json \
       --now "$signedAt" \
       --freshness-window-secs 86400 \
       --rollout-id "$rid" \
       2>/dev/null; then
    echo "FAIL: tampered manifest accepted by verify-artifact rollout-manifest" >&2
    exit 1
  fi

  # Step 3: byte-flipped signature.
  cp ${rolloutManifestFixture}/manifest.canonical.json.sig tampered.sig
  chmod +w tampered.sig
  printf '\xff' | dd of=tampered.sig bs=1 count=1 seek=10 \
    conv=notrunc 2>/dev/null
  if ${verifyArtifactPkg}/bin/nixfleet-verify-artifact rollout-manifest \
       --manifest ${rolloutManifestFixture}/manifest.canonical.json \
       --signature tampered.sig \
       --trust-file ${rolloutManifestFixture}/trust.json \
       --now "$signedAt" \
       --freshness-window-secs 86400 \
       --rollout-id "$rid" \
       2>/dev/null; then
    echo "FAIL: tampered signature accepted" >&2
    exit 1
  fi

  # Step 4: content-address mismatch — operator passes a rolloutId
  # that doesn't match the manifest's content hash. This is the
  # rename/swap attack the content-addressing closes. The signature
  # itself is valid; the verifier must still reject.
  wrong_rid="9999999999999999999999999999999999999999999999999999999999999999"
  if ${verifyArtifactPkg}/bin/nixfleet-verify-artifact rollout-manifest \
       --manifest ${rolloutManifestFixture}/manifest.canonical.json \
       --signature ${rolloutManifestFixture}/manifest.canonical.json.sig \
       --trust-file ${rolloutManifestFixture}/trust.json \
       --now "$signedAt" \
       --freshness-window-secs 86400 \
       --rollout-id "$wrong_rid" \
       2>/dev/null; then
    echo "FAIL: rolloutId mismatch accepted (rename/swap attack not detected)" >&2
    exit 1
  fi

  touch "$out"
''
