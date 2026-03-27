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

See the [User Guide](docs/src/guide/README.md) for a full walkthrough.

## Layout

```
modules/
├── _shared/lib/       # Framework API: mkFleet, mkOrg, mkRole, mkHost, mkBatchHosts, mkTestMatrix
├── _shared/           # hostSpec options, disk templates
├── core/              # Core deferred modules (nixos.nix, darwin.nix)
├── scopes/            # Scope modules (base, impermanence, nixfleet/agent, nixfleet/control-plane)
├── tests/             # Eval tests, VM tests, integration tests
├── apps.nix           # Flake apps (install, validate, docs, spawn-qemu, ...)
├── fleet.nix          # Test fleet for framework CI
└── flake-module.nix   # flakeModules.default for consumers
docs/
├── src/               # Technical reference + user guide (mdbook)
│   └── guide/         # User guide section
└── nixfleet/          # Business docs, specs, research
```

> **Note:** Opinionated modules (scopes, wrappers, HM programs, config files) live in your fleet repo, not in the framework. NixFleet provides the lib + base NixOS/Darwin core. Your fleet adds the opinions.

## Scope Pattern

Hosts declare flags in `hostSpecValues` (e.g., `useNiri`, `isDev`, `isGraphical`). Scope modules in your fleet auto-activate based on these flags using `lib.mkIf`. The framework defines the hostSpec options; your fleet provides the scope implementations.

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

- **`flake.nix`** — minimal: inputs + `mkFleet` + `import-tree ./modules`
- **Every `.nix`** under `modules/` is auto-imported (except `_`-prefixed dirs)
- **`fleet.nix`** defines all hosts centrally via `mkFleet`
- **Deferred modules** are auto-included by `mkHost`
- **Scope modules** self-activate with `lib.mkIf` on `hostSpec` flags

See [docs/src/architecture.md](docs/src/architecture.md) and [CLAUDE.md](CLAUDE.md) for details.

## Related Repos

| Repo | Content |
|------|---------|
| [fleet](https://github.com/abstracts33d/fleet) | Reference fleet (abstracts33d org config) |
| [fleet-secrets](https://github.com/abstracts33d/fleet-secrets) | Encrypted secrets (agenix) |
| [claude-defaults](https://github.com/abstracts33d/claude-defaults) | Standalone Claude Code plugin for other Nix projects |

## License

Apache-2.0
