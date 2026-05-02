# Future-dated artifact rejection scenario.
#
# Validates the freshness symmetric-bound fix landed in
# `crates/nixfleet-reconciler/src/verify.rs::finish_sidecar_verification`
# (commit 6fabc7c onward): a `meta.signedAt` more than `CLOCK_SKEW_SLACK_SECS`
# (60s) ahead of `now` is rejected with `VerifyError::FutureDated`.
#
# Pre-fix posture: the freshness check only enforced the *past* bound
# (`now − signed_at > window + slack` → Stale). Anything in the future
# was accepted indefinitely, masking pre-signed artifacts (a CI key
# compromise indicator) and clock-skew tampering.
#
# Pure runCommand — verify is offline by definition; no microvm. The
# `nixfleet-verify-artifact artifact` CLI accepts an explicit `--now`
# flag, so we drive every reference time off the fixture's fixed
# `signedAt` instead of wall clock — no flakes.
#
# Coverage matrix (Δ = signed_at − now):
#
#   | Δ        | Expected | Bound under test                       |
#   |----------|----------|----------------------------------------|
#   | +2 days  | reject 1 | future-dated (Δ > +60s)                |
#   | +30s     | accept 0 | future-slack-symmetric                 |
#   |   0      | accept 0 | steady state                           |
#   | −30s     | accept 0 | past-slack-symmetric (mirror)          |
#
# The +30s and −30s pair proves the slack window is symmetric around
# `now` — same 60s tolerance on either side. Pre-fix this failed
# only on the +30s side (silently accepted) and silently masked far
# larger deltas. A future regression that re-introduces an
# asymmetric bound (e.g. only-past) is caught by either the +2 day
# rejection OR the −30s acceptance flipping.
{
  pkgs,
  signedFixture,
  verifyArtifactPkg,
  ...
}: let
  # The harness signedFixture's signedAt is fixed at 2026-05-01T00:00:00Z
  # (default in tests/harness/fixtures/signed/default.nix). Driving the
  # CLI's `--now` off this constant keeps every Δ deterministic.
  signedAt = "2026-05-01T00:00:00Z";

  # Reference times around signedAt. RFC3339 in UTC; verify-artifact
  # parses these via chrono::DateTime::<Utc>::from_str.
  twoDaysBefore = "2026-04-29T00:00:00Z"; # Δ = signed_at − now = +2 days
  thirtySecondsBefore = "2026-04-30T23:59:30Z"; # Δ = +30s
  exactly = signedAt; # Δ = 0
  thirtySecondsAfter = "2026-05-01T00:00:30Z"; # Δ = −30s

  # Freshness window large enough that the past-bound never fires for
  # Δ ∈ {0, −30s, +30s}. The fixture defaults to 86400 minutes (60
  # days) so any window ≤ that is below the past-bound regardless.
  freshnessWindowSecs = 86400; # 1 day, consistent with stale-target's posture
in
  pkgs.runCommand "fleet-harness-future-dated-rejection" {} ''
    set -euo pipefail

    # Step 1 — far-future: Δ = +2 days. Must be REJECTED with the
    # FutureDated error. We capture stderr so we can grep for the
    # operator-readable parts of the message: "future-dated artifact"
    # and "clock skew tolerance is 60s".
    echo "step 1: Δ=+2d → expect reject…"
    if ${verifyArtifactPkg}/bin/nixfleet-verify-artifact artifact \
         --artifact ${signedFixture}/canonical.json \
         --signature ${signedFixture}/canonical.json.sig \
         --trust-file ${signedFixture}/test-trust.json \
         --now ${twoDaysBefore} \
         --freshness-window-secs ${toString freshnessWindowSecs} \
         2> step1.stderr; then
      echo "FAIL: Δ=+2d future-dated artifact accepted by verify-artifact" >&2
      cat step1.stderr >&2 || true
      exit 1
    fi
    if ! grep -q 'future-dated artifact' step1.stderr; then
      echo "FAIL: Δ=+2d rejection did not surface 'future-dated artifact' in stderr" >&2
      cat step1.stderr >&2
      exit 1
    fi
    if ! grep -q 'clock skew tolerance is 60s' step1.stderr; then
      echo "FAIL: Δ=+2d rejection did not surface '60s' clock-skew tolerance copy" >&2
      cat step1.stderr >&2
      exit 1
    fi
    echo "step 1: Δ=+2d rejected with future-dated error and 60s tolerance copy"

    # Step 2 — within slack: Δ = +30s. Must be ACCEPTED (slack window
    # is 60s; +30s is comfortably inside).
    echo "step 2: Δ=+30s → expect accept (within 60s slack)…"
    ${verifyArtifactPkg}/bin/nixfleet-verify-artifact artifact \
      --artifact ${signedFixture}/canonical.json \
      --signature ${signedFixture}/canonical.json.sig \
      --trust-file ${signedFixture}/test-trust.json \
      --now ${thirtySecondsBefore} \
      --freshness-window-secs ${toString freshnessWindowSecs}
    echo "step 2: Δ=+30s accepted (slack-symmetric upper bound)"

    # Step 3 — exact: Δ = 0. Steady-state acceptance.
    echo "step 3: Δ=0 → expect accept (steady state)…"
    ${verifyArtifactPkg}/bin/nixfleet-verify-artifact artifact \
      --artifact ${signedFixture}/canonical.json \
      --signature ${signedFixture}/canonical.json.sig \
      --trust-file ${signedFixture}/test-trust.json \
      --now ${exactly} \
      --freshness-window-secs ${toString freshnessWindowSecs}
    echo "step 3: Δ=0 accepted"

    # Step 4 — past-symmetric mirror: Δ = −30s. Must be ACCEPTED. This
    # is the load-bearing pair to step 2 — proves the slack tolerance
    # is symmetric around now (same 60s on either side). A future
    # regression that re-introduces an asymmetric bound (e.g. only-
    # past or only-future) will flip exactly one of {step 2, step 4}.
    echo "step 4: Δ=−30s → expect accept (past-slack mirror)…"
    ${verifyArtifactPkg}/bin/nixfleet-verify-artifact artifact \
      --artifact ${signedFixture}/canonical.json \
      --signature ${signedFixture}/canonical.json.sig \
      --trust-file ${signedFixture}/test-trust.json \
      --now ${thirtySecondsAfter} \
      --freshness-window-secs ${toString freshnessWindowSecs}
    echo "step 4: Δ=−30s accepted (slack-symmetric lower bound)"

    touch "$out"
  ''
