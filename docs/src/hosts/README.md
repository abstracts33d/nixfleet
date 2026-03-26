# Host System

## Purpose

All hosts are declared in a single file, `modules/fleet.nix`, via the `mkFleet` API. Scope modules auto-activate based on each host's `hostSpec` flags -- hosts never list features manually.

## Location

- `modules/fleet.nix` -- all host definitions (the single source of truth)
- `modules/_shared/lib/` -- `mkFleet`, `mkHost`, `mkOrg`, `mkRole`, `mkBatchHosts`, `mkTestMatrix`
- `modules/_shared/mk-host.nix` -- low-level constructors (called internally by mkFleet)
- `modules/_shared/host-spec-module.nix` -- hostSpec option definitions
- `modules/_hardware/<name>/` -- per-host disk-config and hardware-configuration

## Fleet Table

### Physical hosts

| Host | Platform | Flags | Description |
|------|----------|-------|-------------|
| [krach](krach.md) | x86_64-linux | Niri, impermanent, dev, graphical | Main workstation |
| [ohm](ohm.md) | x86_64-linux | GNOME, impermanent, !dev | Secondary laptop |
| [lab](lab.md) | x86_64-linux | server, impermanent, !dev, !graphical | Headless server |
| [aether](aether.md) | aarch64-darwin | Darwin, dev, graphical | Apple Silicon Mac |

### VM hosts

| Host | Platform | Flags | Description |
|------|----------|-------|-------------|
| [krach-qemu](vm/krach-qemu.md) | x86_64-linux | Niri, impermanent, !dev | QEMU mirror of krach |
| [krach-utm](vm/krach-utm.md) | aarch64-linux | Niri, impermanent, !dev | UTM mirror of krach |
| [qemu](vm/qemu.md) | x86_64-linux | minimal | Bare QEMU test VM |
| [utm](vm/utm.md) | aarch64-linux | minimal | Bare UTM test VM |

### Batch hosts (simulated edge fleet)

| Hosts | Platform | Role | Description |
|-------|----------|------|-------------|
| `edge-01`, `edge-02`, `edge-03` | x86_64-linux | `edge` | Stamped from a batch template via `mkBatchHosts` |

### Test matrix (CI)

| Hosts | Platform | Description |
|-------|----------|-------------|
| `test-workstation-x86_64-linux`, `test-server-x86_64-linux`, `test-minimal-x86_64-linux` | x86_64-linux | Role × platform eval hosts via `mkTestMatrix` |

## hostSpec Smart Defaults

Compositor flags auto-propagate via `lib.mkDefault`:

- `useNiri` implies `isGraphical = true`, `useGreetd = true`
- `useHyprland` implies `isGraphical = true`, `useGreetd = true`
- `useGnome` implies `isGraphical = true`, `useGdm = true`
- `isMinimal` implies `isGraphical = false`, `isDev = false`

## Adding a New Host

See the [new host guide](../../guide/advanced/new-host.md) for step-by-step instructions. The short version: add an `mkHost` entry to `fleet.nix`, add hardware config in `_hardware/`, and run the installer.

## Links

- [Architecture](../architecture.md)
- [VM Hosts](vm/README.md)
- [hostSpec flags](../architecture.md)
