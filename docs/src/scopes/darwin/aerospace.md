# aerospace

## Purpose

AeroSpace tiling window manager for macOS. Currently disabled on all hosts (`useAerospace = false` on aether).

## Location

- `modules/scopes/darwin/aerospace.nix`

## Configuration

**Gate:** `useAerospace`

### Darwin module
- `services.aerospace.enable = true`

## Dependencies

- Depends on: hostSpec `useAerospace` flag

## Links

- [Scope Overview](../README.md)
- [aether host](../../hosts/aether.md)
