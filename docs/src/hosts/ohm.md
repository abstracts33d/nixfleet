# ohm (framework test host)

## Purpose

Framework test host for `userName` override tests. Declared in `modules/fleet.nix` as a VM-mode host.

> **Note:** This is the framework's *test* host. The physical `ohm` laptop (with GNOME, French keyboard, etc.) is defined in the [fleet overlay](https://github.com/abstracts33d/fleet).

## Location

- `modules/fleet.nix` (host entry via `mkHost`)

## Configuration

| Property | Value |
|----------|-------|
| Platform | x86_64-linux |
| Organization | test-org |
| Constructor | `mkFleet` → `mkVmHost` (internal) |
| User | sabrina (overrides org default) |

## What it tests

- `hostSpecValues.userName` override takes precedence over org defaults
- All other org defaults (timezone, locale, SSH keys) still inherit

## Links

- [Host Overview](README.md)
