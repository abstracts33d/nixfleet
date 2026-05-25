# Platform-derived compliance disable: a host with `system` not ending
# in `-linux` cannot honestly run the NixOS compliance collector, so
# `mkFleet` auto-disables every channel-declared framework on it at the
# lowest precedence layer. Operator declarations at any other scope
# still win - see `compliance-host-framework-disable.nix` for the
# operator-override path.
{mkFleet, ...}:
mkFleet {
  hosts.h-darwin = {
    system = "aarch64-darwin";
    configuration = import ./_stub-configuration.nix {};
    tags = [];
    channel = "stable";
  };
  hosts.h-linux = {
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
