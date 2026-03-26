# The Scope System

How features are organized and self-activate.

## What Are Scopes?

Scopes are feature groups that activate based on host flags. Instead of manually listing packages and services for each host, you set a flag and the scope handles everything.

| Flag | Scope | What it provides |
|------|-------|-----------------|
| `!isMinimal` | base | Universal packages, catppuccin theming, nix-index |
| `isDev` | dev | direnv, mise, docker, dev languages, Claude Code |
| `isGraphical` | graphical | pipewire, fonts, browsers, editors |
| `useNiri` | desktop/niri | Niri compositor + Noctalia shell |
| `isImpermanent` | impermanence | Ephemeral root, persist paths |

## Self-Activation

Each scope module checks its flag and activates itself:

```nix
# Simplified: scopes/dev/nixos.nix
config = lib.mkIf hS.isDev {
  virtualisation.docker.enable = true;
  environment.systemPackages = [ ... ];
};
```

When you add `isDev = true` to a host, it gets Docker, dev tools, and everything else in the dev scope — without listing any of it explicitly.

## Adding a Feature

To add a new feature to an existing scope:
1. Edit the scope's module file
2. The feature appears on every host with that flag
3. No host files need changing

To add a new scope, see [Adding a New Scope](../advanced/new-scope.md).

## Scope-Aware Persistence

Each scope manages its own persist paths. When a scope adds a program that needs persistent state, the persist path lives in the same scope module — not in a central file.

## Roles: Composing Scopes

Roles are named bundles of hostSpec flags that pre-configure which scopes activate. Instead of repeating flags across hosts, assign a role:

| Role | Flags set |
|------|-----------|
| `workstation` | isDev, isGraphical, isImpermanent, useNiri |
| `server` | isServer, !isDev, !isGraphical |
| `minimal` | isMinimal |
| `vm-test` | !isDev, isGraphical, isImpermanent, useNiri |
| `edge` | isServer, isMinimal |
| `darwin-workstation` | isDarwin, isDev, isGraphical |

Roles are defined in `modules/_shared/lib/roles.nix` and assigned via `mkHost` or `mkBatchHosts` in `fleet.nix`. Individual hosts can still override any flag set by a role.

## Platform Awareness

Scopes handle platform differences internally. The dev scope installs Docker on NixOS and skips it on macOS. The graphical scope only applies to NixOS (macOS handles graphics differently).

## Further Reading

- [Technical Scope Details](../../scopes/README.md) — every scope module documented
- [Adding a New Scope](../advanced/new-scope.md) — step-by-step guide
