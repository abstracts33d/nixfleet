# Installation

Detailed installation guide for all platforms.

## macOS (nix-darwin)

The install script sets your hostname, backs up conflicting `/etc/` files, builds the configuration, and switches to it.

```sh
nix run .#install -- -h <hostname> -u <username>
```

**What happens:**
1. SSH key is verified (needed for secrets repo)
2. Hostname is set via `scutil`
3. nix-darwin configuration is built and activated
4. Home Manager configures your user environment

**After install:** Open a new terminal. Your shell, prompt, and tools are ready.

## NixOS (Remote via nixos-anywhere)

Boot the target machine from any NixOS ISO (or the custom ISO with `nix build .#iso`), then:

```sh
nix run .#install -- --target root@<ip> -h <hostname> -u <username>
```

**What happens:**
1. SSH connectivity is verified
2. Your agenix decryption key is provisioned via `--extra-files`
3. nixos-anywhere partitions disks (via disko) and installs NixOS
4. On reboot, secrets are decrypted, WiFi connects, everything works

**Options:**
- `-p <port>` — custom SSH port (default: 22)
- Cross-platform installs (macOS to NixOS) automatically use `--build-on-remote`

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
- **Secrets missing:** Check `~/.keys/id_ed25519` exists (the agenix decryption key)
- **Build fails:** Run `git add .` — Nix only sees git-tracked files

For technical details on each host configuration, see the [Technical Docs](../../src/hosts/README.md).
