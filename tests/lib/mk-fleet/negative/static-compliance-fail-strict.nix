# tests/lib/mk-fleet/negative/static-compliance-fail-strict.nix
#
# Issue #4 static compliance gate: when `channels.X.compliance.strict
# = true`, any host on that channel whose evaluated NixOS config has
# a `compliance.evidence.probes.<n>` with `type ∈ {static, both}`
# and `staticEvidence.passed = false` must fail mkFleet eval.
#
# Stub a minimal nixosConfiguration that injects one failing static
# probe into `config.compliance.evidence.probes`. Channel marked
# strict. Expected: mkFleet's `checkInvariants` throws on the
# `staticComplianceErrors` accumulator.
{mkFleet, ...}:
mkFleet {
  hosts.m = {
    system = "x86_64-linux";
    configuration = {
      config = {
        system.build.toplevel = {
          outPath = "/nix/store/0000000000000000000000000000000000000000-stub";
          drvPath = "/nix/store/0000000000000000000000000000000000000000-stub.drv";
        };
        compliance.evidence.probes = {
          accessControl = {
            type = "static";
            staticEvidence = {
              passed = false;
              evidence = {
                sshPasswordAuthDisabled = false;
              };
            };
          };
        };
      };
    };
    tags = [];
    channel = "prod";
  };
  channels.prod = {
    rolloutPolicy = "all-at-once";
    signingIntervalMinutes = 60;
    freshnessWindow = 180;
    compliance = {
      mode = "enforce";
      frameworks = [];
    };
  };
  rolloutPolicies.all-at-once = {
    strategy = "all-at-once";
    waves = [
      {
        selector.all = true;
        soakMinutes = 0;
      }
    ];
  };
}
