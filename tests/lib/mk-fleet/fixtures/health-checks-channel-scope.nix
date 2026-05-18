# RFC-0007 §3.5 + §4 - channel scope is the fourth multi-scope target.
# Verifies host > channel > tag > fleet precedence on probe-name
# collision: fleet declares `heartbeat`, tag `web` declares `heartbeat`,
# channel `stable` declares `heartbeat`. h-web (on `stable`, tagged
# `web`, no host-scope override) resolves to the channel's version.
{mkFleet, ...}:
mkFleet {
  hosts.h-web = {
    system = "x86_64-linux";
    configuration = import ./_stub-configuration.nix {};
    tags = ["web"];
    channel = "stable";
  };
  tags.web = {
    description = "Frontend role";
    healthChecks.heartbeat = {
      kind = "http";
      url = "http://localhost/health-from-tag";
      intervalSeconds = 30;
      mode = "observe";
    };
  };
  healthChecks.heartbeat = {
    kind = "http";
    url = "http://localhost/health-from-fleet";
    intervalSeconds = 60;
    mode = "observe";
  };
  channels.stable = {
    rolloutPolicy = "all";
    signingIntervalMinutes = 60;
    freshnessWindow = 180;
    healthChecks.heartbeat = {
      kind = "http";
      url = "http://localhost/health-from-channel";
      intervalSeconds = 15;
      mode = "enforce";
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
