# Tier C - Eval tests: assert config properties at evaluation time.
# Runs via `nix flake check` (--no-build skips VM tests, eval checks are instant).
#
# Currently restricted to the mkFleet harness — all per-host eval
# assertions previously here (web-01, cache-test, agent-test, …)
# were defined against the example fleet in `modules/fleet.nix`,
# which was removed during the framework decoupling. Real per-host
# coverage now lives in the consuming fleet repo's CI; the framework
# keeps only the lib-level mkFleet fixtures that don't depend on a
# concrete fleet instance.
{self, ...}: {
  perSystem = {
    pkgs,
    system,
    lib,
    ...
  }:
    lib.optionalAttrs (system == "x86_64-linux") {
      checks = {
        # --- lib/mkFleet: eval-only harness (positive + negative fixtures) ---
        # Evaluates every fixture under tests/lib/mkFleet/{fixtures,negative}.
        # Positive fixtures compare against golden .resolved.json files;
        # negative fixtures are expected to throw. Each entry in `results`
        # must be the literal string "ok" - anything else fails the check.
        mkFleet-eval-tests = let
          harness = import ../../tests/lib/mkFleet {inherit lib;};
          results = harness.results;
          allOk = lib.all (r: r == "ok") results;
        in
          pkgs.runCommand "mkFleet-eval-tests" {} (
            if allOk
            then ''
              echo "PASS: mkFleet harness — ${toString (builtins.length results)} fixtures ok"
              printf '%s\n' ${lib.concatMapStringsSep " " (r: ''"${r}"'') results} > $out
            ''
            else ''
              echo "FAIL: mkFleet harness produced non-ok results: ${builtins.toJSON results}" >&2
              exit 1
            ''
          );
      };
    };
}
