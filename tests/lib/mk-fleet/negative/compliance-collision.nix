# RFC-0010 §3.5: declaring both `compliance.frameworks = ["anssi-bp028"]`
# AND an explicit `healthChecks.evidence-anssi-bp028 = {...}` is
# ambiguous - the shorthand's synthesized probe name collides with the
# operator's explicit declaration. Eval must throw with a clear
# message naming the collision so the operator picks one form.
{mkFleet, ...}:
mkFleet {
  hosts.h = {
    system = "x86_64-linux";
    configuration = import ../fixtures/_stub-configuration.nix {};
    tags = [];
    channel = "stable";
  };
  channels.stable = {
    rolloutPolicy = "all";
    signingIntervalMinutes = 60;
    freshnessWindow = 180;
    compliance.frameworks = ["anssi-bp028"];
    healthChecks.evidence-anssi-bp028 = {
      kind = "evidence";
      framework = "anssi-bp028";
      mode = "observe";
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
