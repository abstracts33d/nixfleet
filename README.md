# NixFleet

**Declarative NixOS fleet management.** Define your organization's infrastructure as code — workstations, servers, edge devices — with reproducible builds, instant rollback, and zero config drift.

## What is NixFleet?

NixFleet is an open-core framework for managing fleets of NixOS machines. It provides:
- **Organizations** — group hosts by org with shared defaults
- **Roles** — compose workstation, server, edge, kiosk profiles from reusable building blocks
- **Batch provisioning** — deploy 3 or 300 identical machines from a template
- **Test matrix** — validate every role x platform combination in CI
- **Extension points** — plug in commercial modules (dashboard, RBAC, SSO)

## Quick Start

```nix
# flake.nix — your fleet in ~20 lines
{
  inputs.nixfleet.url = "github:abstracts33d/nixfleet";
  inputs.nixpkgs.follows = "nixfleet/nixpkgs";
  inputs.flake-parts.follows = "nixfleet/flake-parts";

  outputs = inputs:
    inputs.flake-parts.lib.mkFlake { inherit inputs; } {
      imports = [ inputs.nixfleet.flakeModules.default ./fleet.nix ];
      systems = [ "x86_64-linux" "aarch64-linux" "aarch64-darwin" ];
    };
}
```

See the [User Guide](docs/guide/README.md) for a full walkthrough.

## Layout

```
modules/
├── _shared/lib/       # Framework API: mkFleet, mkOrg, mkRole, mkHost, mkBatchHosts, mkTestMatrix
├── _shared/           # hostSpec options, disk templates, keys
├── core/              # Core deferred modules (nixos.nix, darwin.nix, home.nix, _home/)
├── scopes/            # Scope modules (auto-activate via mkIf on hostSpec flags)
│   ├── catppuccin.nix # Theming
│   ├── graphical/     # isGraphical: pipewire, fonts, browsers
│   ├── dev/           # isDev: direnv, docker, claude-code
│   ├── desktop/       # Compositors: niri, hyprland, gnome
│   ├── display/       # Display managers: gdm, greetd
│   ├── hardware/      # bluetooth
│   └── darwin/        # homebrew, karabiner, aerospace
├── wrappers/          # Portable composites (shell, terminal)
├── tests/             # Eval tests, VM tests, integration tests
├── apps.nix           # Flake apps (install, build-switch, validate, spawn-qemu, ...)
├── fleet.nix          # Test fleet for framework CI
└── flake-module.nix   # flakeModules.default for consumers
docs/
├── src/               # Technical reference (mdbook)
├── guide/             # User guide (mdbook)
└── nixfleet/          # Business docs, specs, research
```

## Scopes

Hosts declare flags in `hostSpecValues`. Scope modules auto-activate:

| Flag | Scope | What it enables |
|------|-------|-----------------|
| `!isMinimal` | `catppuccin.nix` | Catppuccin Macchiato theming |
| `isGraphical` | `graphical/` | Pipewire, fonts, XDG portals, browsers |
| `isDev` | `dev/` | Direnv, mise, docker, claude-code |
| `useNiri` | `desktop/niri.nix` | Niri compositor + Noctalia Shell |
| `useGnome` | `desktop/gnome.nix` | GNOME desktop + GDM |
| `isImpermanent` | `impermanence.nix` | Ephemeral root, btrfs wipe |
| `hasBluetooth` | `hardware/bluetooth.nix` | Bluetooth + Blueman |
| `isDarwin` | `darwin/` | Homebrew, karabiner, aerospace |

## Portable Environments

```sh
nix run github:abstracts33d/nixfleet#shell      # Full dev shell
nix run github:abstracts33d/nixfleet#terminal    # Kitty wrapping the dev shell
```

## Installing a Host

```sh
# macOS (local)
nix run .#install -- -h <hostname> -u <username>

# NixOS (remote)
nix run .#install -- --target root@<ip> -h <hostname> -u <username>
```

## Adding a Host

```nix
# In your fleet.nix
(mkHost {
  hostName = "my-host";
  org = myOrg;
  platform = "x86_64-linux";
  role = builtinRoles.workstation;
  hardwareModules = [ ./_hardware/my-host/disk-config.nix ];
})
```

Scopes auto-activate based on role flags — no feature lists needed.

## Virtual Machines

```sh
nix run .#spawn-qemu -- --iso iso/nixos-x86_64.iso   # First boot
nix run .#spawn-qemu                                   # Subsequent boots
nix run .#test-vm -- -h krach-qemu                     # Full VM test cycle
```

## Development

```sh
nix develop                        # Dev shell
nix flake check --no-build         # Eval tests
nix run .#validate                 # Full validation
nix fmt                            # Format (alejandra + shfmt)
```

## Architecture

Built on [flake-parts](https://flake.parts) + [import-tree](https://github.com/vic/import-tree):

- **`flake.nix`** — minimal: inputs + `mkFlake` + `import-tree ./modules`
- **Every `.nix`** under `modules/` is auto-imported (except `_`-prefixed dirs)
- **`fleet.nix`** defines all hosts centrally via `mkFleet`
- **Deferred modules** are auto-included by `mkHost`
- **Scope modules** self-activate with `lib.mkIf` on `hostSpec` flags

See [TECHNICAL.md](TECHNICAL.md) and [ARCHITECTURE.md](ARCHITECTURE.md) for details.

## Related Repos

| Repo | Content |
|------|---------|
| [claude-defaults](https://github.com/abstracts33d/claude-defaults) | Claude Code plugin (base agents, skills, rules) |
| [claude](https://github.com/abstracts33d/claude) | Shared knowledge (Nix, Rust, security) |
| [fleet](https://github.com/abstracts33d/fleet) | Reference fleet (abstracts33d org config) |
| [fleet-secrets](https://github.com/abstracts33d/fleet-secrets) | Encrypted secrets (agenix) |

## License

Apache-2.0
