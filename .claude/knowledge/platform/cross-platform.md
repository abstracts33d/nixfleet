# Cross-Platform Design

Knowledge about targeting NixOS, macOS (Darwin), and portable environments.

## Environments

This config targets three environments:
- **NixOS** (krach, ohm, lab) -- full system management
- **macOS** (aether) -- nix-darwin + HM
- **Portable** (any machine with Nix) -- wrappers only

## Platform Guards

| Guard | When to use |
|-------|-------------|
| `hS.isDarwin` | Darwin-only code paths |
| `!hS.isDarwin` | NixOS-only features |
| `hS.isImpermanent` | Impermanence persistence paths |
| `lib.optionalAttrs (!hS.isDarwin)` | For `home.persistence` blocks (option doesn't exist on Darwin) |
| `lib.mkIf (hS.networking ? interface)` | Hosts without a named network interface |

## Key Differences

| Feature | NixOS | Darwin |
|---------|-------|--------|
| System config | `nixos.nix` | `darwin.nix` |
| Secrets backend | agenix (org-level) | agenix (org-level) |
| Display manager | greetd / GDM | N/A |
| Compositor | Niri / Hyprland / GNOME | AeroSpace |
| Package manager | nix only | nix + Homebrew (for GUI apps) |
| Impermanence | btrfs wipe on boot | N/A |
| Trusted users | Gated with `!isServer` | Not gated (Darwin unlikely to be server) |
| catppuccin | nixosModules + homeModules | homeModules only (no darwinModules) |

## Wrapper Boundary

| Layer | Managed by | Why |
|-------|-----------|-----|
| Individual tools (kitty, git, starship, bat) | HM `programs.*` | Catppuccin auto-themes, shell integrations, no conflicts |
| Portable dev shell (`nix run .#shell`) | Wrapper | Self-contained zsh + tools for remote machines |
| Portable terminal (`nix run .#terminal`) | Wrapper | Kitty wrapping the portable shell |
| Desktop session (niri + noctalia) | NixOS scope + HM | Needs host GPU drivers |

## Design Principle

When adding features, consider cross-platform compatibility. If a cross-platform approach adds too much complexity (bwrap GL hacks, AppleScript automation), make it platform-specific and keep it simple. Note ambitious ideas as TODOs instead of implementing fragile workarounds.
