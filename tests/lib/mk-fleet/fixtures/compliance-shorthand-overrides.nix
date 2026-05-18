# RFC-0010 §3.5 + §3.4 — per-control overrides on the compliance
# shorthand. The framework-entry attrset carries a `controlOverrides`
# map that desugars onto the synthesized `evidence-<framework>` probe
# so the agent's per-control mode resolver picks it up at runtime.
#
# Verifies:
#   - controlOverrides scoped to a single framework entry
#   - reason text round-trips through the JSON
#   - bare-string entries default to empty overrides
{mkFleet, ...}:
mkFleet {
  hosts.h-stable = {
    system = "x86_64-linux";
    configuration = import ./_stub-configuration.nix {};
    tags = [];
    channel = "stable";
  };
  channels.stable = {
    rolloutPolicy = "all";
    signingIntervalMinutes = 60;
    freshnessWindow = 180;
    compliance = {
      mode = "enforce";
      frameworks = [
        "anssi-bp028"
        {
          name = "nis2-essential";
          mode = "enforce";
          controlOverrides = {
            "agent-egress-exemption" = {
              mode = "observe";
              reason = "Phase-out window for legacy egress policy";
            };
            "synthetic" = {
              mode = "disabled";
              reason = "";
            };
          };
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
