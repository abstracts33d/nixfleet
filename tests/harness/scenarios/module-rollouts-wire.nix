# End-to-end NixOS-module test for the rollout-manifest distributor
# wired through `services.nixfleet-control-plane`.
#
# Boots the actual NixOS service module
# (modules/scopes/nixfleet/_control-plane.nix) — NOT the harness's
# hand-rolled cp-real.nix unit — points `rolloutsDir` at a directory
# containing the rollout-manifest fixture, and asserts the running CP
# serves the manifest pair at:
#
#   GET /v1/rollouts/<rolloutId>      → manifest bytes byte-for-byte
#   GET /v1/rollouts/<rolloutId>/sig  → signature bytes byte-for-byte
#   GET /v1/rollouts/<bogus 64-hex>   → 404
#
# Why this exists separately from manifest-tamper-rejection (auditor
# verify-only) and the agent unit tests in
# `nixfleet_agent::manifest_cache` (fetch + cache + signed-event):
# both bypass the NixOS service module. ExecStart-construction bugs
# in `_control-plane.nix` (a flag dropped, a path threaded wrong, a
# new option not added to the systemd unit) cannot fail those tests
# but DO break the production service. This is the only scenario
# that boots the real module's `services.nixfleet-control-plane =
# {...}` declaration.
{
  lib,
  pkgs,
  inputs,
  rolloutManifestFixture,
  signedFixture,
  testCerts,
  cpPkg,
  ...
}: let
  # Stage the fixture's manifest pair under the filename layout the
  # CP serves: `<rolloutId>.json{,.sig}`. The fixture stores them as
  # `manifest.canonical.json{,.sig}`; this is just the rename. The
  # rolloutId itself is sha256 of the canonical bytes — a build-time
  # output of the fixture (not knowable at eval without IFD), so the
  # test script reads it from a staged file inside the host VM.
  rolloutsDir =
    pkgs.runCommand "harness-rollouts-dir" {
      nativeBuildInputs = [pkgs.coreutils];
    } ''
      mkdir -p "$out"
      rid=$(cat ${rolloutManifestFixture}/rollout-id)
      cp ${rolloutManifestFixture}/manifest.canonical.json     "$out/$rid.json"
      cp ${rolloutManifestFixture}/manifest.canonical.json.sig "$out/$rid.json.sig"
    '';
in
  pkgs.testers.runNixOSTest {
    name = "fleet-harness-module-rollouts-wire";
    meta.timeout = 300;

    # The NixOS service module reads
    # `inputs.self.packages.${pkgs.system}.nixfleet-control-plane` to
    # locate the binary. Pass the flake's inputs through so that path
    # resolves to the crane-built `cpPkg`. Same convention as the
    # other harness scenarios (mkFleetScenario in lib.nix).
    node.specialArgs = {inherit inputs;};

    nodes.host = {
      lib,
      pkgs,
      ...
    }: {
      imports = [
        # Declares the `nixfleet.trust.*` option schema that
        # `_control-plane.nix` reads to materialise its trust.json.
        # The schema's defaults (all keys null) are fine here — this
        # test does not exercise the materialisation path; it points
        # `cfg.trustFile` at the fixture's pre-built test-trust.json.
        # The trust.json materialisation path is covered by the
        # eval-only `modules/tests/_agent-v2-trust.nix`.
        ../../../contracts/trust.nix
        # `_control-plane.nix` writes
        # `nixfleet.persistence.directories = [...]` so the host's
        # persistence implementation picks up the CP state dir.
        # No persistence impl is wired here, so this declaration is
        # inert — but the option must exist for the assignment to
        # type-check.
        ../../../contracts/persistence.nix
        ../../../modules/scopes/nixfleet/_control-plane.nix
      ];

      services.nixfleet-control-plane = {
        enable = true;
        # Match the harness convention (cp-real.nix, agent-real.nix
        # both use 8443) so the test certs' SAN posture lines up.
        listen = "0.0.0.0:8443";
        openFirewall = true;

        # Point at /nix/store paths so the service's
        # `ConditionPathExists = cfg.artifactPath` is always true and
        # we don't need tmpfiles to bootstrap a writable path.
        artifactPath = "${signedFixture}/canonical.json";
        signaturePath = "${signedFixture}/canonical.json.sig";
        trustFile = "${signedFixture}/test-trust.json";

        tls = {
          cert = "${testCerts}/cp-cert.pem";
          key = "${testCerts}/cp-key.pem";
          clientCa = "${testCerts}/ca.pem";
        };

        # The flag this scenario exists to exercise. A regression in
        # `_control-plane.nix`'s ExecStart construction (e.g. dropping
        # `--rollouts-dir`, like the gap that landed before commit
        # 8f02cbd) makes every step below fail.
        rolloutsDir = "${rolloutsDir}";
      };

      # Test client materials + expected bytes staged where the
      # testScript can curl + diff against them. Avoids IFD on
      # rolloutId by staging the file the runCommand emits.
      environment.etc = {
        "nixfleet-test/rollout-id".source = "${rolloutManifestFixture}/rollout-id";
        "nixfleet-test/expected.json".source = "${rolloutManifestFixture}/manifest.canonical.json";
        "nixfleet-test/expected.sig".source = "${rolloutManifestFixture}/manifest.canonical.json.sig";
        "nixfleet-test/agent-cert.pem".source = "${testCerts}/agent-01-cert.pem";
        "nixfleet-test/agent-key.pem".source = "${testCerts}/agent-01-key.pem";
        "nixfleet-test/ca.pem".source = "${testCerts}/ca.pem";
      };

      environment.systemPackages = [pkgs.curl];
    };

    testScript = ''
      host.start()
      host.wait_for_unit("multi-user.target")
      host.wait_for_unit("nixfleet-control-plane.service")
      host.wait_for_open_port(8443)

      rid = host.succeed("cat /etc/nixfleet-test/rollout-id").strip()

      curl_base = (
          "curl -sS "
          "--cacert /etc/nixfleet-test/ca.pem "
          "--cert /etc/nixfleet-test/agent-cert.pem "
          "--key /etc/nixfleet-test/agent-key.pem "
          "--resolve cp:8443:127.0.0.1 "
      )

      # Step 1: GET /v1/rollouts/<rid> — byte-for-byte match against
      # the fixture's canonical manifest.
      host.succeed(
          f"{curl_base} --fail "
          f"https://cp:8443/v1/rollouts/{rid} -o /tmp/got.json"
      )
      host.succeed("diff -u /etc/nixfleet-test/expected.json /tmp/got.json")

      # Step 2: GET /v1/rollouts/<rid>/sig — byte-for-byte match.
      host.succeed(
          f"{curl_base} --fail "
          f"https://cp:8443/v1/rollouts/{rid}/sig -o /tmp/got.sig"
      )
      host.succeed("cmp /etc/nixfleet-test/expected.sig /tmp/got.sig")

      # Step 3: 404 on a well-formed (64-char hex) but unknown rid.
      # `looks_like_rollout_id` accepts the shape; the loader misses
      # both filesystem AND http-source (latter is unset here), so
      # `load_pair` falls through to NOT_FOUND.
      bogus = "0" * 64
      code = host.succeed(
          f"{curl_base} -o /dev/null -w '%{{http_code}}' "
          f"https://cp:8443/v1/rollouts/{bogus}"
      ).strip()
      assert code == "404", f"expected 404 for unknown rolloutId, got {code}"

      print(
          "fleet-harness-module-rollouts-wire: services.nixfleet-control-plane "
          "module threaded `rolloutsDir` through ExecStart to the running CP; "
          "GET /v1/rollouts/<id>{,/sig} served fixture bytes byte-for-byte; "
          "unknown rid returned 404."
      )
    '';
  }
