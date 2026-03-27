# Hardware & VMs

Knowledge about hardware configuration, QEMU, UTM, and virtualization.

## Host Constructors

| Constructor | Use | What it adds |
|-------------|-----|-------------|
| `mkNixosHost` | Physical NixOS machines | Base NixOS + HM |
| `mkVmHost` | QEMU/UTM VMs | Wraps `mkNixosHost` + virtio hardware, SPICE, software rendering, global DHCP |
| `mkDarwinHost` | macOS machines | nix-darwin + HM |

VM hosts need no hardware/extra modules (defaults in `mkVmHost`). UTM can override `hardwareModules` and `platform`.

## QEMU VM Management

```bash
# First boot with ISO
nix run .#spawn-qemu -- --iso <path-to-nixos-iso>

# Subsequent boots (uses existing disk)
nix run .#spawn-qemu

# Headless mode (serial console)
nix run .#spawn-qemu -- --console
```

### Key QEMU flags

- **Display**: SPICE on port 5900 (`-spice port=5900,disable-ticketing=on`), VGA virtio
- **Graphics**: virgl for GPU acceleration (`-device virtio-vga-gl`)
- **Storage**: virtio-blk for disk I/O
- **Networking**: user-mode with SSH port forward (2222->22)
- **Memory/CPU**: configurable via flags, defaults 4G/4 cores

### Gotchas

- QEMU nixpkgs hardcodes `/run/opengl-driver` -- needs sudo shim on non-NixOS
- SPICE on port 5900 with `disable-ticketing=on` -- no auth, acceptable for local dev
- Always use named flags (`--iso`, `--disk`) -- positional args had parsing bugs

## UTM VM Management (macOS)

```bash
# Create VM and boot from ISO
nix run .#spawn-utm -- --iso <path-to-aarch64-iso>

# Then install via nixos-anywhere
nix run .#install -- --target root@<vm-ip> -h krach-utm
```

UTM uses Apple Virtualization.framework -- no QEMU dependency on macOS.

## Hardware Configuration

Per-host hardware files live in `_hardware/<host>/`:
- `disk-config.nix` -- disko declarative disk layout
- `hardware-configuration.nix` -- generated hardware scan

## Disko Disk Templates

`_shared/disk-templates/` provides reusable disk layouts:
- Standard btrfs with subvolumes (for impermanence)
- LUKS encryption support
- ESP partition management
