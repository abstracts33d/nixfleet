# Core Modules

## Purpose

Core modules are always active on every host. They provide the foundational NixOS and Darwin configuration that all hosts share: boot, networking, user management, SSH hardening, security, Nix settings, and agenix secrets integration.

## Location

- `modules/core/nixos.nix` -- NixOS system config
- `modules/core/darwin.nix` -- Darwin system config

> **Note:** Home Manager tool configuration (`core/home.nix`, `core/_home/`) is fleet-overlay territory — it is not shipped by the framework. Consuming fleet repos add their own HM tool configs via deferred modules.

## Module Table

| Module | Scope | Key Responsibilities |
|--------|-------|---------------------|
| [nixos](nixos.md) | `flake.modules.nixos.core` | Boot, networking, users, SSH hardening, firewall, Nix settings, nixpkgs config |
| [darwin](darwin.md) | `flake.modules.darwin.core` | Nix settings, TouchID sudo, dock management, system defaults |

## What core/nixos.nix provides

- `nixpkgs.config` — allowUnfree, allowBroken=false, allowInsecure=false
- Nix settings — substituters, trusted-users, auto-optimise-store, weekly GC
- SSH hardening — PermitRootLogin=prohibit-password, PasswordAuthentication=false
- Firewall — enabled by default
- User accounts — primary user + root wired from `hostSpec`
- SSH authorized keys — from `hostSpec.sshAuthorizedKeys`
- Password files — `hashedPasswordFile` / `rootHashedPasswordFile` wired from `hostSpec`
- Locale, timezone, keyboard — from `hostSpec`
- disko — imported for declarative disk partitioning

## Links

- [Architecture](../architecture.md)
- [Scope System](../scopes/README.md)
