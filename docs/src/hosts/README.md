# Host System

## Purpose

All hosts are declared in a single file, `modules/fleet.nix`, via the `mkFleet` API. Scope modules auto-activate based on each host's `hostSpec` flags — hosts never list features manually.

## Location

- `modules/fleet.nix` -- all host definitions (the single source of truth)
- `modules/_shared/lib/` -- `mkFleet`, `mkHost`, `mkOrg`, `mkRole`, `mkBatchHosts`, `mkTestMatrix`
- `modules/_shared/mk-host.nix` -- low-level constructors (called internally by mkFleet)
- `modules/_shared/host-spec-module.nix` -- hostSpec option definitions
- `modules/_hardware/<name>/` -- per-host disk-config and hardware-configuration

## Framework Test Fleet

The framework ships a minimal test fleet in `modules/fleet.nix`. These hosts exist to make eval tests and VM tests pass — they are **not** a real org fleet. Fleet-specific `hostSpec` options (isDev, isGraphical, useNiri, etc.) are declared by consuming fleet repos, not the framework.

### Test hosts (individual)

| Host | Platform | Flags | Purpose |
|------|----------|-------|---------|
| [krach](krach.md) | x86_64-linux | `isImpermanent` | Org defaults / SSH / impermanence tests |
| [krach-qemu](vm/krach-qemu.md) | x86_64-linux | `isImpermanent` | Scope activation tests |
| [ohm](ohm.md) | x86_64-linux | `userName=sabrina` | userName override tests |
| [qemu](vm/qemu.md) | x86_64-linux | `isMinimal` | Minimal host tests |
| [lab](lab.md) | x86_64-linux | `isServer` | Server role tests |

### Batch hosts (simulated edge fleet)

| Hosts | Platform | Role | Description |
|-------|----------|------|-------------|
| `edge-01`, `edge-02`, `edge-03` | x86_64-linux | `edge` | Stamped from a batch template via `mkBatchHosts` |

### Test matrix (CI)

| Hosts | Platform | Description |
|-------|----------|-------------|
| `test-workstation-x86_64`, `test-server-x86_64`, `test-minimal-x86_64` | x86_64-linux | Role × platform eval hosts via `mkTestMatrix` |

> Fleet overlay hosts (krach physical, ohm physical, lab physical, aether, utm, krach-utm) are defined in the [fleet repo](https://github.com/abstracts33d/fleet), not in the framework.

## hostSpec Framework Options

The framework defines these options in `host-spec-module.nix`:

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `userName` | str | — | Primary username |
| `hostName` | str | — | Machine hostname |
| `organization` | str | — | Set by `mkFleet` |
| `role` | str or null | null | Named role |
| `timeZone` | str | `"UTC"` | IANA timezone |
| `locale` | str | `"en_US.UTF-8"` | System locale |
| `keyboardLayout` | str | `"us"` | XKB layout |
| `sshAuthorizedKeys` | list of str | `[]` | SSH public keys |
| `secretsPath` | str or null | null | Secrets repo path hint |
| `isMinimal` | bool | false | Suppress base packages |
| `isDarwin` | bool | false | macOS host |
| `isImpermanent` | bool | false | Enable impermanence |
| `isServer` | bool | false | Headless server |
| `hashedPasswordFile` | str or null | null | Primary user password file |
| `rootHashedPasswordFile` | str or null | null | Root password file |

Additional flags (`isDev`, `isGraphical`, `useNiri`, etc.) are declared by consuming fleet repos.

## Adding a New Host

See the [new host guide](../guide/advanced/new-host.md) for step-by-step instructions. The short version: add an `mkHost` entry to `fleet.nix`, add hardware config in `_hardware/`, and run the installer.

## Links

- [Architecture](../architecture.md)
- [VM Hosts](vm/README.md)
