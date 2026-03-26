# install

## Purpose

Unified install script for both macOS (local nix-darwin) and NixOS (remote via nixos-anywhere). Handles SSH key verification, hostname setup, agenix key provisioning, and cross-platform builds.

## Location

- `modules/apps.nix` (the `install` app definition)

## Usage

```sh
# macOS (local)
nix run .#install -- -h <hostname> -u <username>

# NixOS (remote)
nix run .#install -- --target root@<ip> -h <hostname> -u <username>

# QEMU VM
nix run .#install -- --target root@localhost -p 2222 -h qemu
```

## Options

| Flag | Default | Description |
|------|---------|-------------|
| `-h` | (required) | Target hostname (must match flake host) |
| `-u` | <username> | Username |
| `--target` | -- | SSH target for NixOS remote install |
| `-p` | 22 | SSH port |

## NixOS Remote Flow

1. Verify SSH agent has keys
2. Test SSH connectivity (TOFU: accepts new host keys)
3. Prepare `--extra-files` with agenix decryption key from `~/.keys/id_ed25519` or `~/.ssh/id_ed25519`
4. Detect cross-platform (Darwin -> Linux) and add `--build-on-remote`
5. Run nixos-anywhere

## macOS Local Flow

1. Verify SSH key at `~/.ssh/id_ed25519`
2. Test GitHub access
3. Set hostname via `scutil`
4. Back up `/etc/` files that nix-darwin overwrites
5. Build and switch via `darwin-rebuild`

## Dependencies

- nixos-anywhere (NixOS remote install)
- git, openssh, nix, coreutils

## Links

- [Apps Overview](README.md)
- [Secrets Bootstrap](../secrets/bootstrap.md)
