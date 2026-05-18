# Per-host framework-level `mode = "disabled"` deactivates the
# synthesised `evidence-<framework>` probe for that host. Closes the
# Aether/Darwin case (RFC-0010 §3.5 + §4): the operator declares the
# disable inline next to the host rather than carving probe-shadow
# overrides into `healthChecks`.
{mkFleet, ...}:
mkFleet {
  hosts.h-linux = {
    system = "x86_64-linux";
    configuration = import ./_stub-configuration.nix {};
    tags = [];
    channel = "stable";
  };
  hosts.h-darwin = {
    system = "x86_64-linux";
    configuration = import ./_stub-configuration.nix {};
    tags = [];
    channel = "stable";
    compliance.frameworks.nis2-essential = {
      mode = "disabled";
      reason = "Darwin host: no compliance collector available";
    };
  };
  channels.stable = {
    rolloutPolicy = "all";
    signingIntervalMinutes = 60;
    freshnessWindow = 180;
    compliance = {
      mode = "enforce";
      frameworks = ["nis2-essential"];
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
