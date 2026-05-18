# RFC-0007 §3.2 + §4 — verifies the multi-scope healthChecks resolver
# produces non-empty `effectiveHealthChecks` per host.
#
# Coverage:
#   h-web carries tag `web`; receives:
#     - fleet-scope `heartbeat`
#     - tag-scope    `nginx-version`
#     - host-scope   `cache-disk-space`  (overrides + adds)
#   h-infra carries tag `infra`; receives:
#     - fleet-scope `heartbeat`
#     - tag-scope    `cp-tls-self-check`
#
# Pins the precedence host > tag > fleet by giving `h-web` a host-scope
# probe that the tag scope does not duplicate (so we see additive merge)
# AND a fleet-scope `heartbeat` mode-override at tag scope on tag `web`
# (so we see override-merge — see expected .resolved.json: h-web's
# heartbeat.mode = "enforce" from tag, NOT "observe" from fleet).
{mkFleet, ...}:
mkFleet {
  hosts.h-web = {
    system = "x86_64-linux";
    configuration = import ./_stub-configuration.nix {};
    tags = ["web"];
    channel = "stable";
    healthChecks.cache-disk-space = {
      kind = "exec";
      command = ["/usr/bin/df" "-h" "/"];
      intervalSeconds = 300;
      mode = "enforce";
    };
  };
  hosts.h-infra = {
    system = "x86_64-linux";
    configuration = import ./_stub-configuration.nix {};
    tags = ["infra"];
    channel = "stable";
  };
  tags.web = {
    description = "Frontend role";
    healthChecks.nginx-version = {
      kind = "http";
      url = "http://localhost/version";
      expectStatus = 200;
      intervalSeconds = 15;
      mode = "enforce";
    };
    healthChecks.heartbeat = {
      kind = "http";
      url = "http://localhost/health";
      intervalSeconds = 30;
      mode = "enforce";
    };
  };
  tags.infra = {
    description = "Infra role";
    healthChecks.cp-tls-self-check = {
      kind = "exec";
      command = ["/usr/bin/openssl" "s_client" "-connect" "cp:8443"];
      intervalSeconds = 600;
      mode = "observe";
    };
  };
  healthChecks.heartbeat = {
    kind = "http";
    url = "http://localhost/health";
    intervalSeconds = 30;
    mode = "observe";
  };
  channels.stable = {
    rolloutPolicy = "all";
    signingIntervalMinutes = 60;
    freshnessWindow = 180;
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
