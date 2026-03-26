# spawn-qemu

## Purpose

QEMU/KVM virtual machine launcher with GPU-accelerated graphics via SPICE (virgl) or headless serial console mode.

## Location

- `modules/apps.nix` (the `spawn-qemu` app definition)
- **Platform:** Linux only

## Usage

```sh
nix run .#spawn-qemu -- --iso iso/nixos-x86_64.iso   # boot from ISO
nix run .#spawn-qemu                                    # boot from disk
nix run .#spawn-qemu -- --console                       # headless mode
```

## Options

| Flag | Default | Description |
|------|---------|-------------|
| `--iso PATH` | -- | Boot from ISO |
| `--disk PATH` | qemu-disk.qcow2 | Disk image path |
| `--ram MB` | 4096 | RAM |
| `--cpus N` | 2 | CPU count |
| `--ssh-port N` | 2222 | SSH port forwarding |
| `--disk-size S` | 20G | New disk image size |
| `--console` | -- | Headless serial console |
| `--graphical` | (default) | GPU-accelerated SPICE |

## Graphical Mode

Uses EGL headless rendering with virtio-vga-gl and SPICE on port 5900. Auto-launches `remote-viewer`. On non-NixOS, requires sudo to create `/run/opengl-driver` symlink for GBM drivers.

## Dependencies

- Packages: qemu, openssh, virt-viewer, mesa, OVMF
- SPICE on localhost:5900 (no auth -- acceptable for local dev)

## Links

- [Apps Overview](README.md)
- [VM hosts](../hosts/vm/README.md)
