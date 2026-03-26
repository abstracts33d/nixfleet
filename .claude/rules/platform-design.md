# Platform-Agnostic Design

The config targets multiple environments: NixOS (krach, ohm), macOS (aether), and portable (any machine with Nix).

## Principle
When adding features, consider cross-platform compatibility. If a cross-platform approach adds too much complexity (e.g. bwrap GL hacks, AppleScript automation), make it platform-specific and keep it simple.

## Guards
- `isDarwin` -- Darwin-only code
- `isImpermanent` -- NixOS impermanence paths
- `lib.optionalAttrs (!hS.isDarwin)` -- for `home.persistence` (option doesn't exist on Darwin)
- `lib.mkIf (hS.networking ? interface)` -- hosts without a named network interface

## Don't over-engineer
Note ambitious ideas as TODOs (Lima, remote builder) instead of implementing fragile workarounds for edge cases.
