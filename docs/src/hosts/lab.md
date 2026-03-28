# lab (framework test host)

## Purpose

Framework test host for server role tests. Declared in `modules/fleet.nix` as a VM-mode host.

> **Note:** This is the framework's *test* host. The physical `lab` server is defined in the [fleet overlay](https://github.com/abstracts33d/fleet).

## Location

- `modules/fleet.nix` (host entry via `mkHost`)

## Configuration

| Property | Value |
|----------|-------|
| Platform | x86_64-linux |
| Organization | test-org |
| Constructor | `mkFleet` → `mkVmHost` (internal) |
| User | testuser (from org defaults) |
| isServer | true |

## What it tests

- Server role flag (`isServer = true`) is set and inherited
- All org defaults (timezone, locale, SSH keys) apply to server hosts

## Links

- [Host Overview](README.md)
