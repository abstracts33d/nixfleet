# Per-host controlOverrides merge into the channel-scope compliance
# shorthand at probe-synthesis time. Tests:
#   - host's compliance.frameworks.<name>.controlOverrides merges with
#     the channel's controlOverrides for the same framework
#   - per-host entry wins on collision
#   - hosts without a per-host compliance attr pass through unchanged
{mkFleet, ...}:
mkFleet {
  hosts.h-default = {
    system = "x86_64-linux";
    configuration = import ./_stub-configuration.nix {};
    tags = [];
    channel = "stable";
    # No per-host compliance attrs — receives the channel's overrides
    # only.
  };
  hosts.h-legacy = {
    system = "x86_64-linux";
    configuration = import ./_stub-configuration.nix {};
    tags = [];
    channel = "stable";
    compliance.frameworks.nis2-essential.controlOverrides = {
      # Adds an override the channel doesn't declare.
      "agent-egress-exemption" = {
        mode = "observe";
        reason = "Phase-out window for legacy egress policy";
      };
      # Overrides the channel's own controlOverride for the same
      # control (per-host wins).
      "secure-boot" = {
        mode = "enforce";
        reason = "TPM available on this host";
      };
    };
  };
  channels.stable = {
    rolloutPolicy = "all";
    signingIntervalMinutes = 60;
    freshnessWindow = 180;
    compliance = {
      mode = "enforce";
      frameworks = [
        {
          name = "nis2-essential";
          mode = "enforce";
          controlOverrides = {
            "secure-boot" = {
              mode = "observe";
              reason = "Fleet default: legacy hardware";
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
