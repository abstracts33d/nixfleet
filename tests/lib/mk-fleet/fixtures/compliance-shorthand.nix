# RFC-0007 §3.5 channel-scope compliance shorthand. Verifies the three
# accepted forms:
#   - bare string list (uses compliance.mode default)
#   - per-entry attrset with mode override
#   - mixed list
# All three expand to evidence-<framework> entries in the channel's
# effective healthChecks; the resolved JSON's
# effectiveHealthChecks.<host> carries them.
{mkFleet, ...}:
mkFleet {
  hosts.h-stable = {
    system = "x86_64-linux";
    configuration = import ./_stub-configuration.nix {};
    tags = [];
    channel = "stable";
  };
  hosts.h-edge = {
    system = "x86_64-linux";
    configuration = import ./_stub-configuration.nix {};
    tags = [];
    channel = "edge";
  };
  channels.stable = {
    rolloutPolicy = "all";
    signingIntervalMinutes = 60;
    freshnessWindow = 180;
    # Bare-string list - all entries inherit compliance.mode = "enforce".
    compliance.frameworks = ["anssi-bp028" "nis2"];
  };
  channels.edge = {
    rolloutPolicy = "all";
    signingIntervalMinutes = 60;
    freshnessWindow = 180;
    # Mixed list: explicit per-entry mode override coexists with bare
    # string. Top-level compliance.mode = "enforce" is the default for
    # bare entries; the iso27001 attrset overrides to "observe".
    compliance = {
      mode = "enforce";
      frameworks = [
        "anssi-bp028"
        {
          name = "iso27001";
          mode = "observe";
        }
      ];
    };
  };
  rolloutPolicies.all = {
    strategy = "all-at-once";
    waves = [
      {
        selector.all = true;
        soakMinutes = 0;
      }
    ];
  };
}
