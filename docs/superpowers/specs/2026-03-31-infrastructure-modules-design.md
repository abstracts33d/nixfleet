# Infrastructure Modules: Attic + microvm.nix

**Date:** 2026-03-31
**Roadmap phase:** Phase 3 (Framework Infrastructure)
**Repo:** nixfleet (modules + flake inputs), fleet (enablement)

## Overview

Add three optional NixOS modules to nixfleet for common fleet infrastructure: a binary cache server (Attic), a binary cache client, and a microvm host. These are thin wrappers that integrate upstream projects with nixfleet conventions (impermanence, firewall, consistent naming). They are exported as `nixfleet.nixosModules.*` — not auto-included by `mkHost`. Consuming fleets import them explicitly.

## Design Decisions

| Decision | Choice | Rationale |
|----------|--------|-----------|
| Module wrapping depth | Thin — enable + secrets + impermanence + firewall only | Consuming fleet configures upstream options directly (storage backend, GC, networking) |
| Client config source | Explicit options (cacheUrl, publicKey) per host | No cross-host config magic; server host sets both enable flags |
| microvm networking | No defaults — fleet defines bridge/TAP | Networking topology is site-specific |
| Distribution | Exported as `nixosModules`, not auto-included | Attic and microvm are optional infrastructure, unlike core agent/CP |
| Flake inputs | In nixfleet, fleet follows | Matches existing pattern (disko, impermanence, lanzaboote); enables eval tests in nixfleet |

## Module Inventory

| Module | Export path | Purpose | Upstream input |
|--------|------------|---------|----------------|
| `attic-server` | `nixfleet.nixosModules.attic-server` | Thin atticd wrapper + secrets + impermanence + firewall | `attic` |
| `attic-client` | `nixfleet.nixosModules.attic-client` | Substituters + trusted keys + CLI package | `attic` |
| `microvm-host` | `nixfleet.nixosModules.microvm-host` | microvm.host enable + impermanence for VM state | `microvm` |

## Flake Changes

### New inputs in `nixfleet/flake.nix`

```nix
attic = {
  url = "github:zhaofengli/attic";
  inputs.nixpkgs.follows = "nixpkgs";
};
microvm = {
  url = "github:astro/microvm.nix";
  inputs.nixpkgs.follows = "nixpkgs";
};
```

### New exports

```nix
nixosModules = {
  attic-server = ./modules/scopes/infra/_attic-server.nix;
  attic-client = ./modules/scopes/infra/_attic-client.nix;
  microvm-host = ./modules/scopes/infra/_microvm-host.nix;
};
```

The `_` prefix excludes these from import-tree auto-import. The `scopes/infra/` directory groups optional infrastructure modules separate from core nixfleet services (`scopes/nixfleet/`).

### Fleet-side follows

```nix
attic.follows = "nixfleet/attic";
microvm.follows = "nixfleet/microvm";
```

## Options API

### services.nixfleet-attic-server

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `enable` | bool | false | Enable the Attic binary cache server |
| `credentialFile` | str | (required) | Path to atticd credentials file (server token + signing key). Typically an agenix-managed path like `/run/agenix/attic-credentials`. |
| `openFirewall` | bool | false | Open the atticd listen port in the firewall |

The module sets `services.atticd.enable = true` and `services.atticd.credentialsFile`. All other `services.atticd.settings` (listen address, storage backend, chunking, garbage collection) pass through to the consuming fleet — they are not wrapped or defaulted.

### services.nixfleet-attic-client

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `enable` | bool | false | Enable the Attic binary cache client |
| `cacheUrl` | str | (required) | URL of the Attic binary cache |
| `publicKey` | str | (required) | Public signing key for the cache (nix format) |

The module adds `cacheUrl` to `nix.settings.substituters`, `publicKey` to `nix.settings.trusted-public-keys`, and `attic` CLI to `environment.systemPackages`.

### services.nixfleet-microvm-host

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `enable` | bool | false | Enable microvm host support |

The module sets `microvm.host.enable = true` and adds impermanence for `/var/lib/microvms`. Actual VM definitions (`microvm.vms.<name>`) and networking (bridges, TAP, NAT) are the consuming fleet's responsibility.

## Module Implementation Pattern

Each module follows the existing agent/control-plane structure:

```nix
# modules/scopes/infra/_attic-server.nix
{ config, lib, inputs, ... }: let
  cfg = config.services.nixfleet-attic-server;
in {
  imports = [ inputs.attic.nixosModules.atticd ];

  options.services.nixfleet-attic-server = {
    enable = lib.mkEnableOption "NixFleet Attic binary cache server";

    credentialFile = lib.mkOption {
      type = lib.types.str;
      example = "/run/agenix/attic-credentials";
      description = "Path to the atticd credentials file (server token + signing key).";
    };

    openFirewall = lib.mkOption {
      type = lib.types.bool;
      default = false;
      description = "Open the atticd listen port in the firewall.";
    };
  };

  config = lib.mkIf cfg.enable {
    services.atticd = {
      enable = true;
      credentialsFile = cfg.credentialFile;
    };

    # Firewall — atticd default port is 8080; fleet overrides via services.atticd.settings.listen
    # Extract port at implementation time from atticd's resolved listen address
    networking.firewall.allowedTCPPorts =
      lib.mkIf cfg.openFirewall
      [ 8080 ];

    # Impermanence
    environment.persistence."/persist".directories =
      lib.mkIf (config.hostSpec.isImpermanent or false)
      [ "/var/lib/atticd" ];
  };
}
```

```nix
# modules/scopes/infra/_attic-client.nix
{ config, lib, pkgs, inputs, ... }: let
  cfg = config.services.nixfleet-attic-client;
in {
  options.services.nixfleet-attic-client = {
    enable = lib.mkEnableOption "NixFleet Attic binary cache client";

    cacheUrl = lib.mkOption {
      type = lib.types.str;
      example = "https://cache.fleet.example.com";
      description = "URL of the Attic binary cache.";
    };

    publicKey = lib.mkOption {
      type = lib.types.str;
      example = "cache.fleet.example.com:AAAA...==";
      description = "Public signing key for the cache.";
    };
  };

  config = lib.mkIf cfg.enable {
    nix.settings = {
      substituters = [ cfg.cacheUrl ];
      trusted-public-keys = [ cfg.publicKey ];
    };

    environment.systemPackages = [
      inputs.attic.packages.${pkgs.stdenv.hostPlatform.system}.default
    ];
  };
}
```

```nix
# modules/scopes/infra/_microvm-host.nix
{ config, lib, inputs, ... }: let
  cfg = config.services.nixfleet-microvm-host;
in {
  imports = [ inputs.microvm.nixosModules.host ];

  options.services.nixfleet-microvm-host = {
    enable = lib.mkEnableOption "NixFleet microvm host support";
  };

  config = lib.mkIf cfg.enable {
    microvm.host.enable = true;

    # Impermanence
    environment.persistence."/persist".directories =
      lib.mkIf (config.hostSpec.isImpermanent or false)
      [ "/var/lib/microvms" ];
  };
}
```

Key implementation notes:
- Modules import upstream NixOS modules from inputs via `specialArgs`
- Impermanence uses the `or false` guard pattern from existing agent/CP modules
- No systemd hardening for attic-server — upstream manages its own service
- attic-client does not import an upstream module — it only configures nix settings and adds a package

## Test Fleet Additions

Two new test hosts in `modules/fleet.nix`:

```nix
# attic-host: tests attic-server + attic-client together
attic-host = mkHost {
  hostName = "attic-host";
  platform = "x86_64-linux";
  isVm = true;
  hostSpec = testDefaults // { isServer = true; };
  modules = [
    self.nixosModules.attic-server
    self.nixosModules.attic-client
  ];
};

# microvm-lab: tests microvm-host
microvm-lab = mkHost {
  hostName = "microvm-lab";
  platform = "x86_64-linux";
  isVm = true;
  hostSpec = testDefaults // { isServer = true; };
  modules = [ self.nixosModules.microvm-host ];
};
```

Test host configuration (values for required options):

```nix
# attic-host module providing test values
{
  services.nixfleet-attic-server = {
    enable = true;
    credentialFile = "/dev/null"; # placeholder for eval tests
    openFirewall = true;
  };
  services.nixfleet-attic-client = {
    enable = true;
    cacheUrl = "https://cache.test.example.com";
    publicKey = "cache.test.example.com:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";
  };
}

# microvm-lab module
{
  services.nixfleet-microvm-host.enable = true;
}
```

## Eval Tests

New assertions in `modules/tests/eval.nix`:

| Test | Host | Asserts |
|------|------|---------|
| `eval-attic-server-enabled` | `attic-host` | `services.atticd.enable == true` |
| `eval-attic-client-substituters` | `attic-host` | Cache URL in `nix.settings.substituters` |
| `eval-attic-client-trusted-keys` | `attic-host` | Public key in `nix.settings.trusted-public-keys` |
| `eval-microvm-host-enabled` | `microvm-lab` | `microvm.host.enable == true` |
| `eval-modules-not-auto-included` | `lab` | No attic or microvm config present on hosts without explicit imports |

## Documentation

New scope docs in nixfleet following existing pattern (Purpose, Location, Activation, Options, Impermanence, Links):

| File | Content |
|------|---------|
| `docs/src/scopes/attic-server.md` | Server module reference |
| `docs/src/scopes/attic-client.md` | Client module reference |
| `docs/src/scopes/microvm-host.md` | microvm host module reference |

Updates to existing nixfleet docs:

| File | Change |
|------|--------|
| `docs/src/scopes/README.md` | Add infrastructure modules table |
| `docs/src/SUMMARY.md` | Add three new scope entries |
| `docs/src/architecture.md` | Add attic and microvm to Key Integrations table |

## Fleet-Side Enablement (not part of this spec)

After the nixfleet modules are implemented, the consuming fleet enables them:

```nix
# In fleet's fleetModules or per-host modules:
inputs.nixfleet.nixosModules.attic-server
inputs.nixfleet.nixosModules.attic-client

# Per-host config (e.g., lab host):
services.nixfleet-attic-server = {
  enable = true;
  credentialFile = "/run/agenix/attic-credentials";
  openFirewall = true;
};
services.atticd.settings = {
  listen = "[::]:8080";
  storage.type = "local";
  storage.path = "/var/lib/atticd/storage";
  chunking = { nar-size-threshold = 65536; min-size = 16384; avg-size = 65536; max-size = 262144; };
};

# All hosts:
services.nixfleet-attic-client = {
  enable = true;
  cacheUrl = "https://cache.fleet.example.com";
  publicKey = "cache.fleet.example.com:...";
};
```

This is fleet-specific work tracked in the fleet roadmap, not in this spec.

## Out of Scope

- Attic S3 backend (fleet configures `services.atticd.settings.storage` directly)
- Attic garbage collection policy (fleet configures `services.atticd.settings.garbage-collection` directly)
- microvm networking (bridges, TAP, NAT — fleet-specific topology)
- microvm VM definitions (fleet defines `microvm.vms.<name>` per-host)
- Post-build hook for pushing to Attic (fleet-side automation)
- Attic server TLS termination (fleet handles via reverse proxy or direct config)
