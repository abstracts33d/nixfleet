# Platform & Virtualization

## Cross-Platform Guards

| Guard | When to use |
|-------|-------------|
| `hS.isDarwin` | Darwin-only code paths |
| `!hS.isDarwin` | NixOS-only features |
| `hS.isImpermanent` | Impermanence persistence paths |
| `lib.optionalAttrs (!hS.isDarwin)` | For `home.persistence` blocks (option type doesn't exist on Darwin) |

## Key Platform Differences

| Feature | NixOS | Darwin |
|---------|-------|--------|
| Secrets backend | agenix (org-level) | agenix (org-level) |
| Display manager | greetd / GDM | N/A |
| Compositor | Niri / Hyprland / GNOME | AeroSpace |
| Package manager | nix only | nix + Homebrew (GUI apps) |
| Impermanence | btrfs wipe on boot | N/A |
| catppuccin | nixosModules + homeModules | homeModules only |

## Host Constructors

| Constructor | Use | Extras |
|-------------|-----|--------|
| `mkNixosHost` | Physical NixOS | Base NixOS + HM |
| `mkVmHost` | QEMU/UTM VMs | + virtio, SPICE, software rendering, DHCP |
| `mkDarwinHost` | macOS | nix-darwin + HM |

## QEMU VMs

```bash
nix run .#spawn-qemu -- --iso <path>   # First boot with ISO
nix run .#spawn-qemu                    # Subsequent boots
nix run .#spawn-qemu -- --console       # Headless (serial)
nix run .#launch-vm                     # Graphical SPICE
```

SPICE on port 5900 (disable-ticketing -- local dev only). SSH forward 2222->22.

## UTM (macOS)

```bash
nix run .#spawn-utm -- --iso <path>     # Create + boot from ISO
nix run .#install -- --target root@<ip> -h <host>  # Install via nixos-anywhere
```

## Hardware

Per-host files in `_hardware/<host>/`: `disk-config.nix` (disko) + `hardware-configuration.nix`.
Disk templates in `_shared/disk-templates/` (btrfs subvolumes, LUKS, ESP).
