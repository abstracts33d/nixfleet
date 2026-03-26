# homebrew

## Purpose

Declarative Homebrew management on macOS via nix-homebrew. Immutable taps, automatic cleanup (`zap`), and auto-update on activation.

## Location

- `modules/scopes/darwin/homebrew.nix`

## Configuration

**Platform:** Darwin only (registered as `flake.modules.darwin.homebrew`)

### nix-homebrew settings
- Immutable taps (`mutableTaps = false`)
- Auto-migrate enabled
- Taps: homebrew-core, homebrew-cask, homebrew-bundle

### Brews
Empty in the framework. Org-specific brews are added via org darwinModules in `fleet.nix`.

### Casks
Empty in the framework. Org-specific casks (e.g. docker-desktop, visual-studio-code, discord, slack, etc.) are added via org darwinModules in `fleet.nix`.

### Activation
- `autoUpdate = true`
- `cleanup = "zap"` (removes unmanaged casks/brews)
- `upgrade = true`

## Dependencies

- Inputs: `nix-homebrew`, `homebrew-core`, `homebrew-cask`, `homebrew-bundle`
- Used by: [aether](../../hosts/aether.md)

## Links

- [Scope Overview](../README.md)
- [aether host](../../hosts/aether.md)
