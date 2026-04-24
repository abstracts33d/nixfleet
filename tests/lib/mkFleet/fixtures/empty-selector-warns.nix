{mkFleet, ...}:
mkFleet {
  hosts.m = {
    system = "x86_64-linux";
    configuration = import ./_stub-configuration.nix {};
    tags = ["role-a"];
    channel = "stable";
  };
  channels.stable = {
    rolloutPolicy = "emptyish";
  };
  rolloutPolicies.emptyish = {
    strategy = "canary";
    waves = [
      {
        selector.tags = ["role-b"];
        soakMinutes = 10;
      } # resolves to zero hosts — warning expected
      {
        selector.all = true;
        soakMinutes = 0;
      }
    ];
  };
}
