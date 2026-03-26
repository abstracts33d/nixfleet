# nix-index

## Purpose

Replaces the default `command-not-found` with nix-index, providing instant package suggestions when a command is missing. Includes comma (`,`) for running any package without installing: `, cowsay hello`.

## Location

- `modules/scopes/nix-index.nix`

## Configuration

**Gate:** `!isMinimal`

### NixOS module
- `programs.nix-index.enable = true`
- `programs.nix-index-database.comma.enable = true`
- `programs.command-not-found.enable = false` (replaced by nix-index)

### HM module
- `programs.nix-index.enable = true`

Uses a pre-built weekly database from nix-index-database so the index works instantly without local building.

## Dependencies

- Input: `nix-index-database` (github:Mic92/nix-index-database)
- Depends on: hostSpec `isMinimal` flag

## Links

- [Scope Overview](README.md)
