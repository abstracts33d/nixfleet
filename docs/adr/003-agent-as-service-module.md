# ADR-003: Agent as NixOS Service Module, Not hostSpec Flag

**Date:** 2026-03-31
**Status:** Accepted
**Spec:** `superpowers/specs/2026-03-31-nixfleet-simplification-design.md`

## Context

The nixfleet agent (Rust binary that polls the control plane for desired generation, runs `nixos-rebuild switch`, reports status) needs configuration: control plane URL, machine ID, poll interval, TLS certs, auth tokens. Two options: configure via hostSpec flags or via a dedicated NixOS service module.

## Decision

The agent is a standard NixOS service module: `services.nixfleet-agent`.

```nix
services.nixfleet-agent = {
  enable = true;
  controlPlaneUrl = "https://cp.example.com";
  machineId = "web-01";  # defaults to hostname
  pollInterval = 60;
  # TLS cert paths, auth config...
};
```

mkHost auto-includes the module (disabled by default). Fleet repos enable and configure it.

## Alternatives Considered

1. **hostSpec flags** — `hostSpec.nixfleetAgent.enable = true; hostSpec.nixfleetAgent.controlPlaneUrl = "..."`. Rejected because hostSpec is for identity and capability flags (who is this machine, what does it do), not service configuration. Agent config has its own option space (URL, TLS certs, poll interval, auth) that belongs in `services.*`.

## Consequences

- Follows NixOS conventions — users expect `services.foo.enable`
- TLS certs and auth tokens wire naturally into agenix via module options
- Per-environment overrides work with standard NixOS semantics (`lib.mkForce`, per-host modules)
- Enterprise customers get full override control without framework-specific patterns
- Fleet-wide config is a single module in `fleetModules`; per-host overrides in host modules
