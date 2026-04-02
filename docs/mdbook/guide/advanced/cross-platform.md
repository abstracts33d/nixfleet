# Cross-Platform Design

How NixFleet targets both NixOS and macOS.

## Supported Platforms

- **NixOS** — full system control (x86_64-linux, aarch64-linux)
- **macOS** — nix-darwin with Home Manager (aarch64-darwin, x86_64-darwin)

Each platform has different capabilities. The framework handles this with guards and platform-aware modules.

## Platform Guards

```nix
# Darwin-only code
lib.mkIf hS.isDarwin { ... }

# NixOS impermanence paths
lib.mkIf hS.isImpermanent { ... }

# Home persistence (option doesn't exist on Darwin)
lib.optionalAttrs (!hS.isDarwin) {
  home.persistence."/persist" = { ... };
}
```

## What Is Platform-Specific

| | NixOS | macOS |
|---|-------|-------|
| System config | NixOS modules | nix-darwin modules |
| User config | Home Manager | Home Manager |
| Init system | systemd | launchd |
| Impermanence | Supported | Not applicable |
| Services | systemd units | launchd agents |

Fleet repos add platform-specific features on top (e.g., Wayland compositors on NixOS, Homebrew casks on macOS).

## Design Principle

When a cross-platform approach adds too much complexity, make it platform-specific and keep it simple. Note ambitious ideas as TODOs rather than implementing fragile workarounds.

## Further Reading

- [Scopes](../concepts/scopes.md) — how features are organized
- [Technical Architecture](../../architecture.md) — module structure
