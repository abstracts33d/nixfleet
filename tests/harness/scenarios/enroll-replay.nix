# Enroll-replay race scenario.
#
# Validates the token-replay race fix landed in
# `crates/nixfleet-control-plane/src/db/tokens.rs::record_token_nonce`
# (commit f03657e): dropped `INSERT OR IGNORE`, now plain INSERT
# returning `RecordTokenOutcome::AlreadyRecorded` on PK conflict; the
# enroll handler maps this to 409 CONFLICT.
#
# Pre-fix posture: a TOCTOU race between the early `token_seen()` check
# and the `INSERT OR IGNORE` write let two concurrent /v1/enroll
# requests for the same nonce both pass `token_seen()`, both hit the
# `INSERT OR IGNORE` (one inserted, one no-op'd), and both proceed to
# mint a cert. The fix closes the race at the SQL primary-key layer.
#
# Why a harness scenario when there's already a Rust integration test:
#   `crates/nixfleet-control-plane/tests/enroll.rs::enroll_rejects_replayed_nonce`
# covers the SEQUENTIAL replay (call A returns 200, call B returns 409).
# The harness adds the CONCURRENT dimension — fire two POSTs at the
# same time and assert exactly one wins. The unit test cannot reproduce
# this race because it serialises calls through a single tokio rt; the
# harness drives it from outside the process via two backgrounded
# curls + `wait`.
#
# Sequence:
#   1. Boot host VM running cp-real with a custom trust.json that
#      carries our test org-root pubkey under `orgRootKey.current`.
#      The org-root private key is mounted at /etc/harness/org-root.pem
#      so the testScript can mint tokens via `nixfleet-mint-token`.
#   2. testScript generates a CSR with openssl, computes the
#      pubkey-fingerprint the way the CP's `routes::enrollment` does
#      (sha256 of the raw 32-byte ed25519 pubkey, base64), mints a
#      bootstrap token, and assembles the EnrollRequest JSON.
#   3. Fire two POSTs to `/v1/enroll` in parallel via two backgrounded
#      curls + `wait`. Capture both http_codes.
#   4. Assert: exactly one 200, exactly one 409 (in either order — the
#      OS scheduling decides which thread wins the SQL primary-key
#      race).
#   5. Assert: state.db has exactly one row in `token_replay` for the
#      nonce.
#   6. Assert: CP journal carries the
#      "enroll: token replay detected at record" log line emitted by
#      the AlreadyRecorded branch.
#
# Edge case (silent-record-failure path):
#   The fix also closes a "fail-open on DB error" gap: a genuine SQL
#   failure (not a PK conflict) now returns 500 INSTEAD of silently
#   proceeding to mint. We exercise this by stopping CP, dropping the
#   `token_replay` table to force a "no such table" error class,
#   restarting CP, and posting a fresh-nonce enroll. Asserts NOT 200
#   (would have been pre-fix's silent fail-open).
{
  pkgs,
  harnessLib,
  testCerts,
  signedFixture,
  cpPkg,
  cliPkg,
  orgRootKeyFixture,
  ...
}: let
  # cp-real CP module. We layer an enrol-enabling override on top —
  # the default cp-real ships a trust.json wired only with
  # ciReleaseKey, which is correct for verify-artifact but leaves
  # /v1/enroll dead (handler returns 500 when orgRootKey is null).
  cpHostBase = harnessLib.mkRealCpHostModule {
    inherit testCerts signedFixture cpPkg;
  };

  # Override: drop in our org-root-key trust.json at
  # /etc/nixfleet-cp/trust.json (the path the enroll handler reads
  # from — it's `dirname(--fleet-ca-cert)/trust.json`).
  enrollEnabledModule = {
    environment.etc = {
      "nixfleet-cp/trust.json".source = "${orgRootKeyFixture}/trust.json";
      # Mount the org-root private key so the testScript can mint
      # tokens at runtime.
      "harness/org-root.pem".source = "${orgRootKeyFixture}/private.pem";
      "harness/ca.pem".source = "${testCerts}/ca.pem";
    };
    environment.systemPackages = [pkgs.openssl pkgs.sqlite pkgs.jq cliPkg];
  };

  combinedHostModule = {
    imports = [cpHostBase enrollEnabledModule];
  };
in
  harnessLib.mkFleetScenario {
    name = "fleet-harness-enroll-replay";
    cpHostModule = combinedHostModule;
    agents = {}; # wire flow driven by host-side curl
    timeout = 300;
    testScript = ''
      start_all()

      host.wait_for_unit("multi-user.target")
      host.wait_for_unit("nixfleet-control-plane.service")
      host.wait_for_open_port(8443)

      # Working directory for runtime-generated material (CSR, token,
      # request body). /tmp is fine — host VM, ephemeral.
      host.succeed("mkdir -p /tmp/enroll-test")

      # Step 1: generate an ed25519 keypair + CSR for hostname
      # 'agent-99' (a hostname not in the harness cert set, so the
      # enrolment is genuinely fresh — no conflict with pre-minted
      # client certs). openssl produces a PEM CSR.
      print("step 1: generate agent-99 CSR…")
      host.succeed(
          "openssl genpkey -algorithm ed25519 "
          "-out /tmp/enroll-test/agent-99-key.pem"
      )
      host.succeed(
          "openssl req -new -key /tmp/enroll-test/agent-99-key.pem "
          "-out /tmp/enroll-test/agent-99-csr.pem "
          "-subj '/CN=agent-99'"
      )

      # Step 2: compute the pubkey-fingerprint exactly the way the CP
      # does. The handler reads `csr_params.public_key.der_bytes()` —
      # which (per rcgen 0.13's `csr.rs::PublicKey::raw`) is the RAW
      # 32-byte ed25519 pubkey extracted from the SPKI's
      # `subject_public_key.data` field. We mirror that here:
      #   1. openssl req -pubkey  → SPKI PEM
      #   2. openssl pkey -pubin -outform DER → SPKI DER
      #   3. tail -c 32           → raw 32 pubkey bytes (the SPKI
      #      ed25519 trailer is exactly the raw key)
      #   4. sha256 + base64      → fingerprint
      print("step 2: compute pubkey fingerprint (rcgen-compatible)…")
      host.succeed(
          "openssl req -in /tmp/enroll-test/agent-99-csr.pem "
          "-noout -pubkey > /tmp/enroll-test/agent-99-pub.pem"
      )
      host.succeed(
          "openssl pkey -pubin -in /tmp/enroll-test/agent-99-pub.pem "
          "-outform DER -out /tmp/enroll-test/agent-99-pub.spki.der"
      )
      host.succeed(
          "tail -c 32 /tmp/enroll-test/agent-99-pub.spki.der "
          "> /tmp/enroll-test/agent-99-pub.raw"
      )
      fp = host.succeed(
          "openssl dgst -sha256 -binary /tmp/enroll-test/agent-99-pub.raw "
          "| base64 -w0"
      ).strip()
      print(f"step 2: fingerprint={fp}")

      # Step 3: mint a single bootstrap token using the org-root key
      # mounted at /etc/harness/org-root.pem. nixfleet-mint-token
      # writes the token JSON to stdout and the nonce to stderr; we
      # capture both. The token's nonce drives the replay defence —
      # both concurrent posts in step 4 use the same token (same
      # nonce), so exactly one is allowed to record it.
      print("step 3: mint bootstrap token…")
      host.succeed(
          "nixfleet-mint-token "
          "--hostname agent-99 "
          f"--csr-pubkey-fingerprint '{fp}' "
          "--org-root-key /etc/harness/org-root.pem "
          "--validity-hours 1 "
          "> /tmp/enroll-test/token.json "
          "2> /tmp/enroll-test/mint.stderr"
      )
      mint_stderr = host.succeed("cat /tmp/enroll-test/mint.stderr")
      # Parse the nonce out of the stderr line "nonce: <hex>".
      nonce = None
      for line in mint_stderr.splitlines():
          if line.startswith("nonce: "):
              nonce = line.split(": ", 1)[1].strip()
              break
      assert nonce is not None, f"could not parse nonce from {mint_stderr!r}"
      print(f"step 3: minted token with nonce={nonce}")

      # Step 4: assemble the EnrollRequest body (token + csrPem) and
      # fire two POSTs in parallel. Both reference the same nonce —
      # the SQL PRIMARY KEY race decides which one becomes 200 and
      # which becomes 409.
      print("step 4: build EnrollRequest, fire two parallel posts…")
      host.succeed(
          "jq -n "
          "--slurpfile token /tmp/enroll-test/token.json "
          "--rawfile csr /tmp/enroll-test/agent-99-csr.pem "
          "'{token: $token[0], csrPem: $csr}' "
          "> /tmp/enroll-test/enroll.json"
      )

      # Two backgrounded curls + wait. Each writes its http_code to a
      # separate file. The trick to maximise concurrency is to fire
      # both before either has a chance to complete — `&` + `wait`.
      # Note: /v1/enroll is non-mTLS (the host has no cert yet), so
      # we only need --cacert here, no --cert/--key.
      host.succeed(
          "set +e; "
          "(curl -sk -o /dev/null -w '%{http_code}' "
          "  --cacert /etc/harness/ca.pem "
          "  -H 'Content-Type: application/json' "
          "  -d @/tmp/enroll-test/enroll.json "
          "  https://localhost:8443/v1/enroll "
          "  > /tmp/enroll-test/code1.txt) & "
          "(curl -sk -o /dev/null -w '%{http_code}' "
          "  --cacert /etc/harness/ca.pem "
          "  -H 'Content-Type: application/json' "
          "  -d @/tmp/enroll-test/enroll.json "
          "  https://localhost:8443/v1/enroll "
          "  > /tmp/enroll-test/code2.txt) & "
          "wait; "
          "set -e"
      )
      code1 = host.succeed("cat /tmp/enroll-test/code1.txt").strip()
      code2 = host.succeed("cat /tmp/enroll-test/code2.txt").strip()
      print(f"step 4: codes = ({code1}, {code2})")

      # Step 5: assert exactly one 200 + exactly one 409 (in either
      # order — schedulers can pick either thread to win). Anything
      # else (two 200s, two 409s, a 500, etc.) is a regression.
      pair = sorted([code1, code2])
      assert pair == ["200", "409"], (
          f"expected exactly one 200 + one 409, got {pair} "
          f"(two-200 = race fix regression; other = unexpected)"
      )
      print("step 5: race outcome correct (exactly one 200, one 409)")

      # Step 6: state.db must have exactly one row for this nonce.
      row_count = host.succeed(
          "sqlite3 /var/lib/nixfleet-cp/state.db "
          f"\"SELECT COUNT(*) FROM token_replay WHERE nonce='{nonce}';\""
      ).strip()
      assert row_count == "1", (
          f"expected exactly 1 token_replay row for nonce={nonce}, got {row_count}"
      )
      print("step 6: token_replay has exactly one row for the nonce")

      # Step 7: CP journal must carry the AlreadyRecorded branch's
      # log line. The 409 path emits this from
      # routes/enrollment.rs (under the
      # `RecordTokenOutcome::AlreadyRecorded` arm).
      host.succeed(
          "journalctl -u nixfleet-control-plane.service --no-pager "
          "| grep -F "
          "'enroll: token replay detected at record (concurrent enroll race or retry)'"
      )
      print("step 7: 'token replay detected at record' log line present")

      # ─── Edge case: silent-record-failure path ────────────────────
      # The fix also closes a fail-open on DB error gap. Pre-fix, a
      # genuine SQL failure (other than PK conflict) went silently
      # ignored and enrolment proceeded. Post-fix, the handler returns
      # 500 INTERNAL_SERVER_ERROR.
      #
      # Reproduction: stop CP, drop `token_replay` table to force a
      # "no such table" error on the next enroll, restart CP, post a
      # fresh-nonce enroll. The handler's record_token_nonce call
      # bubbles `Err(_)` up through the handler's `tracing::error!(...
      # "db record_token_nonce failed; refusing enrollment"); 500`
      # arm.
      #
      # We use a fresh CSR + fresh nonce so the early `token_seen()`
      # also fails (against the missing table) — both the seen-check
      # and the record-call go through the Err arm. Either path
      # producing non-200 satisfies the "fail-closed" contract; we
      # just need NOT 200.
      print("edge case: silent-record-failure → !200 contract…")
      host.succeed("systemctl stop nixfleet-control-plane.service")
      # Drop the table outright. SQLite will surface
      # `Err::SqliteFailure("no such table: token_replay")` on the
      # next access — distinct error class from
      # ConstraintViolation, so the handler's match goes through the
      # `Err(e) => INTERNAL_SERVER_ERROR` arm.
      host.succeed(
          "sqlite3 /var/lib/nixfleet-cp/state.db "
          "'DROP TABLE token_replay;'"
      )
      host.succeed("systemctl start nixfleet-control-plane.service")
      host.wait_for_unit("nixfleet-control-plane.service")
      host.wait_for_open_port(8443)

      # Mint a fresh token so the early seen-check would normally
      # have to consult the (now-broken) table.
      host.succeed(
          "nixfleet-mint-token "
          "--hostname agent-99 "
          f"--csr-pubkey-fingerprint '{fp}' "
          "--org-root-key /etc/harness/org-root.pem "
          "--validity-hours 1 "
          "> /tmp/enroll-test/token-fresh.json "
          "2>/dev/null"
      )
      host.succeed(
          "jq -n "
          "--slurpfile token /tmp/enroll-test/token-fresh.json "
          "--rawfile csr /tmp/enroll-test/agent-99-csr.pem "
          "'{token: $token[0], csrPem: $csr}' "
          "> /tmp/enroll-test/enroll-fresh.json"
      )

      rc, fresh_code = host.execute(
          "curl -sk -o /dev/null -w '%{http_code}' "
          "--cacert /etc/harness/ca.pem "
          "-H 'Content-Type: application/json' "
          "-d @/tmp/enroll-test/enroll-fresh.json "
          "https://localhost:8443/v1/enroll"
      )
      assert rc == 0, f"fresh-nonce curl failed: {fresh_code}"
      assert fresh_code.strip() != "200", (
          f"fail-open regression: enroll returned 200 with broken "
          f"token_replay table (expected non-200). got {fresh_code!r}"
      )
      print(f"edge case: fresh-nonce enroll on broken table returned {fresh_code.strip()} (not 200, contract holds)")

      print(
          "fleet-harness-enroll-replay: race fix holds — concurrent "
          "/v1/enroll on same nonce yields exactly one 200 + one 409, "
          "exactly one token_replay row, log line present; broken "
          "token_replay table fails closed (not 200)."
      )
    '';
  }
