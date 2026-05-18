# Fleet- and tag-scope compliance overrides flow into a host's
# synthesised `evidence-<framework>` probe with fleet < tag < channel
# < host precedence:
#   - h-untagged inherits the fleet-scope observe-mode default
#   - h-audit-pinned has the `audit` tag, whose tag-scope override
#     bumps the framework back to enforce with an audit reason
#   - h-strict declares a per-host enforce override and a per-control
#     observe exemption on top of the tag-scope enforce
#   - controlOverrides deep-merge: fleet < tag < channel < host
{mkFleet, ...}:
mkFleet {
  compliance.frameworks.nis2-essential = {
    mode = "observe";
    reason = "Fleet rollout window: observe-mode default";
    controlOverrides = {
      "fleet-only-control" = {
        mode = "observe";
        reason = "Fleet-scope exemption";
      };
    };
  };
  tags.audit = {
    compliance.frameworks.nis2-essential = {
      mode = "enforce";
      reason = "Audit tag: must enforce";
      controlOverrides = {
        "tag-only-control" = {
          mode = "enforce";
          reason = "Tag-scope strict";
        };
      };
    };
  };
  hosts.h-untagged = {
    system = "x86_64-linux";
    configuration = import ./_stub-configuration.nix {};
    tags = [];
    channel = "stable";
  };
  hosts.h-audit-pinned = {
    system = "x86_64-linux";
    configuration = import ./_stub-configuration.nix {};
    tags = ["audit"];
    channel = "stable";
  };
  hosts.h-strict = {
    system = "x86_64-linux";
    configuration = import ./_stub-configuration.nix {};
    tags = ["audit"];
    channel = "stable";
    compliance.frameworks.nis2-essential = {
      reason = "Host-pinned: production critical";
      controlOverrides = {
        "host-only-control" = {
          mode = "observe";
          reason = "Host-scope exemption";
        };
      };
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
