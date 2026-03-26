# Quick Start

Get up and running in minutes.

## Prerequisites

- A machine with [Nix](https://nixos.org/download) installed (NixOS, macOS, or any Linux)
- An ed25519 SSH key (for secrets decryption)
- Access to the private `nix-secrets` repository

## Try It Without Installing

The portable shell works on any machine with Nix:

```sh
nix run github:abstracts33d/nixos-config#shell
```

This gives you a fully configured zsh with 20+ CLI tools, starship prompt, and git config — no installation required.

## First-Time Setup

### macOS

```sh
git clone git@github.com:abstracts33d/nixos-config.git ~/nixos-config
cd ~/nixos-config
nix run .#install -- -h aether -u s33d
```

### NixOS (Remote)

Boot the target machine from a NixOS ISO, then from your workstation:

```sh
nix run .#install -- --target root@<ip> -h krach -u s33d
```

The install script handles everything: disk partitioning, system configuration, secret provisioning, and first boot.

### VM (Testing)

For a fully automated test cycle (build ISO, install, verify):

```sh
nix run .#test-vm -- -h krach-qemu
```

## After Installation

Day-to-day, you only need one command:

```sh
nix run .#build-switch
```

This rebuilds and switches to the latest configuration. See [Day-to-Day Usage](daily-usage.md) for the full workflow.

## Next Steps

- [Installation](installation.md) — detailed install guide with options
- [Why NixOS?](../concepts/why-nixos.md) — understand the philosophy
- [The Scope System](../concepts/scopes.md) — how features are organized
