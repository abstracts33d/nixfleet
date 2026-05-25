{mkFleet, ...}:
mkFleet {
  hosts.bad = {
    platform = "x86_64-linux";
    configuration = "not-a-nixos-config";
    tags = [];
    channel = "stable";
  };
  channels.stable = {
    rolloutPolicy = "all-at-once";
    signingIntervalMinutes = 60;
    freshnessWindow = 180;
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
