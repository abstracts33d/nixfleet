# Test helper functions for eval checks.
# Usage: import from eval.nix or future vm.nix
{lib}: {
  # Build a runCommand that prints PASS/FAIL for each assertion and fails on first failure.
  mkEvalCheck = pkgs: name: assertions:
    pkgs.runCommand "eval-test-${name}" {} (
      lib.concatStringsSep "\n" (
        map (a:
          if a.check
          then ''echo "PASS: ${a.msg}"''
          else ''echo "FAIL: ${a.msg}" >&2; exit 1'')
        assertions
      )
      + "\ntouch $out\n"
    );
}
