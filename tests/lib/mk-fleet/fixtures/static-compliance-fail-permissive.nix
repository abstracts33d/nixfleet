# tests/lib/mk-fleet/fixtures/static-compliance-fail-permissive.nix
#
# Issue #58 — when a channel's `compliance.mode = "permissive"`, the
# static gate emits `lib.warn` per failing host/control but eval
# SUCCEEDS. Mirror of negative/static-compliance-fail-strict.nix
# with mode flipped from default-strict (legacy) to explicit
# permissive.
#
# Stub a minimal nixosConfiguration that injects one failing static
# probe into `config.compliance.evidence.probes`. Channel marked
# permissive. Expected: mkFleet's `checkInvariants` does NOT throw;
# the resolved fleet is returned with a warning trace.
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
      mode = "permissive";
      # Note: legacy `strict` defaults to true here, but `mode` is
      # explicitly set, so the resolution prefers `mode`. This is
      # the "both set" case acceptance criterion — explicit mode
      # wins; warning text would mention the conflict (planned
      # follow-up; current implementation just prefers mode silently).
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
