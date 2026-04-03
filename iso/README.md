# NixOS Installer ISO

Custom NixOS minimal ISO with SSH key pre-configured for automated installs.

## Build

```sh
nix build .#iso
```

The ISO includes:
- Our SSH public key in root's authorized_keys (no passwd needed)
- QEMU guest agents + SPICE support
- Git, parted, vim

## Usage

```sh
# Manual VM install
nix run .#spawn-qemu -- --iso result/iso/*.iso

# Fully automated (build ISO + install + verify)
nix run .#test-vm -- -h web-02
```
