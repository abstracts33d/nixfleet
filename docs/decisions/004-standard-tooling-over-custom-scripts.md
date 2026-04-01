# ADR-004: Standard NixOS Tooling over Custom Scripts

**Date:** 2026-03-31
**Status:** Accepted
**Spec:** `superpowers/specs/2026-03-31-nixfleet-simplification-design.md`

## Context

nixfleet had ~500 lines of custom shell scripts in `apps.nix` wrapping standard tools: `install` (wrapping nixos-anywhere), `build-switch` (wrapping nixos-rebuild), `docs` (wrapping mdbook), plus VM helpers. The custom scripts added flags, validation, and UX polish but required users to learn `nix run .#install -- --target root@ip -h hostname -u username` instead of the standard commands.

## Decision

Remove custom wrappers for operations that have standard equivalents:

| Operation | Before (custom) | After (standard) |
|-----------|-----------------|-------------------|
| Fresh install | `nix run .#install -- --target root@ip -h hostname -u username` | `nixos-anywhere --flake .#hostname root@ip` |
| Local rebuild | `nix run .#build-switch` | `sudo nixos-rebuild switch --flake .#hostname` |
| Remote rebuild | (not directly supported) | `nixos-rebuild switch --flake .#hostname --target-host root@ip` |
| macOS rebuild | `nix run .#install -- -h hostname -u username` | `darwin-rebuild switch --flake .#hostname` |

Keep custom scripts only for operations without standard equivalents: VM helpers (QEMU/UTM orchestration), custom ISO.

## Alternatives Considered

1. **Keep wrappers as UX sugar** — provide both custom and standard commands. Rejected because maintaining ~500 lines of shell scripts for marginal UX improvement isn't worth it, and it creates confusion about which command to use.
2. **Replace with `nh`** — use `nh os switch .` as the nicer UX layer. Accepted as complementary (see fleet enhancements spec) — `nh` is a community tool, not a custom script to maintain.

## Consequences

- apps.nix shrinks from ~500 to ~150 lines (VM helpers only)
- Any NixOS tutorial or documentation applies directly — no translation needed
- nixos-anywhere integration is automatic (disko config is part of nixosConfiguration)
- VM helpers exported as `nixfleet.lib.mkVmApps` — fleet repos wire them into their own apps output
- ISO is a nixfleet package, built via `nix build github:abstracts33d/nixfleet#packages.x86_64-linux.iso`
