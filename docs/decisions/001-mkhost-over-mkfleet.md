# ADR-001: mkHost over mkFleet/mkOrg/mkRole

**Date:** 2026-03-31
**Status:** Accepted
**Spec:** `superpowers/specs/2026-03-31-nixfleet-simplification-design.md`

## Context

nixfleet had a 4-function DSL: `mkFleet` (entry point, validates orgs+hosts), `mkOrg` (org-level defaults), `mkRole` (role presets like workstation/server), `mkHost` (single host). This required learning 4 abstractions before deploying a single machine.

## Decision

Replace the DSL with a single function: `nixfleet.lib.mkHost`. It takes a host definition and returns a standard `nixosSystem` or `darwinSystem`.

- **mkFleet** removed — fleet repos define `nixosConfigurations` directly in their flake outputs
- **mkOrg** removed — org defaults are plain `let` bindings in the fleet's `flake.nix`
- **mkRole** removed — roles replaced by hostSpec flags (see ADR-002)
- **mkHost** survives as the single API function

## Alternatives Considered

1. **Keep mkFleet but simplify** — still requires learning the abstraction. Rejected because the ceremony doesn't earn its complexity for most use cases.
2. **Pure modules, no mkHost** — fleet repos use `nixpkgs.lib.nixosSystem` directly and import nixfleet modules. Rejected because mkHost provides real value: auto-wiring scopes, injecting core modules, handling Darwin vs NixOS detection. Without it, every fleet repo would duplicate this wiring.
3. **Convention-over-configuration (mkFleetFlake)** — auto-discover hosts from directory structure. Deferred as future sugar on top of mkHost (Approach C in spec).

## Consequences

- Standard NixOS commands work: `nixos-anywhere --flake .#host root@ip`, `nixos-rebuild switch --flake .#host`
- Zero learning curve beyond "call mkHost, get a nixosConfiguration"
- Fleet repos are standard Nix flakes — no framework magic in the flake structure
- Batch hosts and test matrices are handled by standard Nix (`builtins.map` over mkHost) instead of framework functions
- Future mkFleetFlake (Approach C) can be layered on top without changing the primitive
