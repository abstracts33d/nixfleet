# ADR-005: Input Follows Chain — nixfleet Controls the Pin

**Date:** 2026-03-31
**Status:** Accepted
**Spec:** `superpowers/specs/2026-03-31-nixfleet-simplification-design.md`

## Context

Fleet repos consume nixfleet as a flake input. Both need nixpkgs, home-manager, disko, and other shared inputs. Two strategies: fleet repos pin their own versions independently, or fleet repos follow nixfleet's pins.

## Decision

Fleet repos use `follows` to inherit nixfleet's pins:

```nix
nixpkgs.follows = "nixfleet/nixpkgs";
home-manager.follows = "nixfleet/home-manager";
disko.follows = "nixfleet/disko";
```

nixfleet is the source of truth for dependency versions.

## Alternatives Considered

1. **Independent pins** — fleet repos pin their own nixpkgs. Rejected because framework modules are tested against nixfleet's nixpkgs pin. Using a different version risks subtle breakage (option renames, module changes, package removals).
2. **Fleet pins, nixfleet follows** — invert the chain so fleet controls nixpkgs and nixfleet adapts. Rejected because the framework should be stable — testing against an unpredictable nixpkgs pin would make nixfleet unreliable for all consumers.

## Consequences

- Fleet repos are locked to nixfleet's nixpkgs version — they cannot independently update nixpkgs without breaking the follows chain
- nixfleet must stay reasonably up-to-date with nixpkgs-unstable to avoid blocking fleet repos
- `nix flake update nixfleet` in fleet is the single command to update all shared dependencies
- Fleet-specific inputs (catppuccin, nixvim, etc.) are pinned independently — only shared infrastructure follows nixfleet
