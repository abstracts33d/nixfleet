# Installation

Detailed installation guide for all platforms.

## NixOS (Remote via nixos-anywhere)

Boot the target machine from any NixOS ISO (or the custom ISO with `nix build .#iso`), then:

```sh
nixos-anywhere --flake .#<hostname> root@<ip>
```

**What happens:**
1. SSH connectivity is verified
2. nixos-anywhere partitions disks (via disko) and installs NixOS
3. On reboot, the system is configured and ready

**Options:**
- `--extra-files <dir>` — provision additional files (e.g., secrets decryption keys, SSH keys)
- `--build-on-remote` — build on the target machine (useful for cross-platform installs)
- `--ssh-port <port>` — custom SSH port

## NixOS (Rebuild)

For an already-installed NixOS host:

```sh
sudo nixos-rebuild switch --flake .#<hostname>
```

## macOS (nix-darwin)

Build and activate the darwin configuration:

```sh
darwin-rebuild switch --flake .#<hostname>
```

## Custom ISO

Build an ISO with your SSH key baked in for passwordless install:

```sh
nix build .#iso
```

This ISO boots with your SSH key in `authorized_keys`, so nixos-anywhere can connect without a password.

## VM Setup

See [VM Testing](../development/vm-testing.md) for QEMU and UTM setup.

## Troubleshooting

- **SSH fails:** Ensure your key is in the agent (`ssh-add -l`)
- **Build fails:** Run `git add .` — Nix only sees git-tracked files
- **Missing state after reboot:** On impermanent hosts, check that required paths are in `environment.persistence`

For technical details on each host configuration, see the [Technical Docs](../../hosts/README.md).
