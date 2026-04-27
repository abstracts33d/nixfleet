# `modules/scopes/nixfleet/_agent_darwin.nix`

Darwin stub for `services.nixfleet-agent`.

Auto-included by `mk-host.nix` for all Darwin hosts. The Rust agent
binary's activation path uses `nixos-rebuild`, which doesn't exist on
Darwin — running the agent on Darwin is deferred until a `darwin-
rebuild` backend lands. Until then this module exists purely to
**declare the option tree** so consumer fleets can wire Darwin hosts
without eval errors:

    # fleet/modules/nixfleet/agent-darwin.nix
    services.nixfleet-agent = {
      enable = true;
      controlPlaneUrl = "https://lab:8080";
    };

All option declarations match the NixOS module (`_agent.nix`) so the
wire types are the same shape regardless of platform — when the real
Darwin implementation lands, only the `config` block changes.

`config` is intentionally empty: enabling this on Darwin compiles +
materialises nothing. Operators wire Darwin hosts the same way as
NixOS — `enable = true; controlPlaneUrl = …; tls = …;` — and the
wiring no-ops on Darwin until a real backend lands.

