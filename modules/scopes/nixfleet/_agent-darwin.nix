# Darwin stub for `services.nixfleet-agent`.
#
# Auto-included by `mk-host.nix` for all Darwin hosts. The Rust agent
# binary's activation path uses `nixos-rebuild`, which doesn't exist on
# Darwin — running the agent on Darwin is deferred until a `darwin-
# rebuild` backend lands. Until then this module exists purely to
# **declare the option tree** so consumer fleets can wire Darwin hosts
# without eval errors:
#
#     # fleet/modules/nixfleet/agent-darwin.nix
#     services.nixfleet-agent = {
#       enable = true;
#       controlPlaneUrl = "https://lab:8080";
#     };
#
# All option declarations match the NixOS module (`_agent.nix`) so the
# wire types are the same shape regardless of platform — when the real
# Darwin implementation lands, only the `config` block changes.
#
# `config` is intentionally empty: enabling this on Darwin compiles +
# materialises nothing. Operators wire Darwin hosts the same way as
# NixOS — `enable = true; controlPlaneUrl = …; tls = …;` — and the
# wiring no-ops on Darwin until a real backend lands.
{
  config,
  lib,
  ...
}: {
  options.services.nixfleet-agent = {
    enable = lib.mkEnableOption "NixFleet fleet management agent (Darwin: deferred)";

    controlPlaneUrl = lib.mkOption {
      type = lib.types.str;
      example = "https://fleet.example.com";
      description = "URL of the NixFleet control plane.";
    };

    machineId = lib.mkOption {
      type = lib.types.str;
      default = config.hostSpec.hostName or config.networking.hostName or "";
      defaultText = lib.literalExpression "config.hostSpec.hostName";
      description = "Machine identifier reported to the control plane.";
    };

    pollInterval = lib.mkOption {
      type = lib.types.int;
      default = 60;
      description = "Poll interval in seconds (steady-state).";
    };

    trustFile = lib.mkOption {
      type = lib.types.path;
      default = "/etc/nixfleet/agent/trust.json";
      description = "Path to the trust-root JSON file.";
    };

    tls = {
      caCert = lib.mkOption {
        type = lib.types.nullOr lib.types.str;
        default = null;
      };
      clientCert = lib.mkOption {
        type = lib.types.nullOr lib.types.str;
        default = null;
      };
      clientKey = lib.mkOption {
        type = lib.types.nullOr lib.types.str;
        default = null;
      };
    };

    bootstrapTokenFile = lib.mkOption {
      type = lib.types.nullOr lib.types.str;
      default = null;
    };

    tags = lib.mkOption {
      type = lib.types.listOf lib.types.str;
      default = [];
      description = "Tags reported with each checkin.";
    };
  };

  # Intentionally no `config` block. Until a darwin-rebuild backend
  # lands, the Darwin agent is a paper option tree — fleet wiring
  # evaluates, no service runs.
}
