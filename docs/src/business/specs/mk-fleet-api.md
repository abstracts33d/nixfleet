# mkFleet API Reference

## Overview

The NixFleet framework library lives at `modules/_shared/lib/`. It exposes a public API for composing NixOS and Darwin fleets from organizations, roles, and host descriptors.

## Functions

### `mkFleet`

Fleet entry point. Takes organizations, hosts, and extensions. Returns `{ nixosConfigurations; darwinConfigurations; }`.

```nix
mkFleet {
  organizations = [ org1 org2 ];    # list of mkOrg outputs
  hosts = [ host1 host2 ... ];      # list of mkHost outputs
  extensions = [];                   # list of nixfleet-platform modules (optional)
}
```

**Behavior:**
- Validates all hosts reference an org in the `organizations` list
- Injects org/role `hostSpec` defaults via `lib.mkDefault` (host values always win)
- Wires `options.nixfleet.extensions` namespace on every host
- Partitions hosts into `nixosConfigurations` and `darwinConfigurations`

### `mkOrg`

Organization factory. Returns a typed attrset consumed by `mkFleet`.

```nix
mkOrg {
  name = "acme";                          # required, unique in fleet
  description = "ACME Corp fleet";        # optional
  hostSpecDefaults = {                    # optional, applied via mkDefault
    userName = "deploy";
    githubUser = "acme-ops";
  };
  secretsPath = "/path/to/secrets";       # optional, framework-agnostic hint
  nixosModules = [ myAgenixNixosModule ]; # optional, extra NixOS modules for all org hosts
  darwinModules = [ myAgenixDarwinModule ]; # optional, extra Darwin modules for all org hosts
  hmModules = [ myOrgHmModule ];          # optional, extra HM modules for all org hosts
  roles = {                               # optional, org-specific custom roles
    kiosk = mkRole { name = "kiosk"; hostSpecDefaults = { isGraphical = true; }; };
  };
}
```

### `mkRole`

Role factory. Returns a typed attrset of `hostSpec` defaults. Roles are decoupled from orgs — reusable across organizations.

```nix
mkRole {
  name = "webserver";                     # required
  hostSpecDefaults = {                    # optional, applied via mkDefault
    isServer = true;
    isMinimal = true;
  };
  modules = [ myWebModule ];             # optional, extra NixOS modules for role members
}
```

**Built-in roles** (in `lib/roles.nix`):

| Role | Flags set |
|------|-----------|
| `workstation` | isDev, isGraphical, isImpermanent, useNiri |
| `server` | isServer, !isDev, !isGraphical |
| `minimal` | isMinimal |
| `vm-test` | !isDev, isGraphical, isImpermanent, useNiri |
| `edge` | isServer, isMinimal |
| `darwin-workstation` | isDarwin, isDev, isGraphical |

### `mkHost`

Host descriptor. Builds the attrset that `mkFleet` consumes. Does NOT build the NixOS/Darwin system.

```nix
mkHost {
  hostName = "web-01";                    # required
  platform = "x86_64-linux";             # required
  org = acmeOrg;                          # required, mkOrg output
  role = builtinRoles.server;             # optional, mkRole output
  hostSpecValues = { ... };               # optional, host-level overrides
  hardwareModules = [ ... ];              # optional, for physical hosts
  extraModules = [ ... ];                 # optional, extra NixOS/Darwin modules
  extraHmModules = [ ... ];               # optional, extra Home Manager modules
  stateVersion = "24.11";                 # optional, default "24.11"
  isVm = false;                           # optional, use mkVmHost internally
  vmHardwareModules = null;               # optional, custom VM hardware
}
```

### `mkBatchHosts`

Batch host generator. Takes a template + instances, returns a list of `mkHost` outputs.

```nix
mkBatchHosts {
  template = {
    org = acmeOrg;
    role = builtinRoles.edge;
    platform = "x86_64-linux";
    isVm = true;
  };
  instances = [
    { hostName = "edge-01"; }
    { hostName = "edge-02"; }
    { hostName = "edge-03"; hostSpecValues = { isMinimal = false; }; }  # override
  ];
}
```

### `mkTestMatrix`

Test matrix generator. Creates one VM host per role × platform combination for CI validation.

```nix
mkTestMatrix {
  org = acmeOrg;                          # required
  roles = with builtinRoles; [            # required
    workstation server minimal
  ];
  platforms = [ "x86_64-linux" ];         # optional, default ["x86_64-linux"]
  namePrefix = "test";                    # optional, default "test"
}
# Generates: test-workstation-x86_64, test-server-x86_64, test-minimal-x86_64
```

## Priority Order

Defaults compose via `lib.mkDefault` (priority 1000). Explicit host values (no mkDefault) always win.

```
org hostSpecDefaults (mkDefault)
  ↓ overridden by
role hostSpecDefaults (mkDefault, later in mkMerge)
  ↓ overridden by
hostSpec smart defaults (mkDefault, in host-spec-module.nix)
  ↓ overridden by
host hostSpecValues (no mkDefault — highest priority)
```

## hostSpec Options Added by Framework

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `organization` | `str` | (none — set by mkFleet) | Organization name |
| `role` | `nullOr str` | `null` | Role name within organization |
| `secretsPath` | `nullOr str` | `null` | Secrets repo path hint (framework-agnostic) |
| `gpgSigningKey` | `nullOr str` | `null` | GPG key fingerprint for git commit signing |
| `sshAuthorizedKeys` | `listOf str` | `[]` | SSH public keys for authorized_keys (primary user and root) |
| `timeZone` | `str` | `"UTC"` | IANA timezone (e.g. `Europe/Paris`) |
| `locale` | `str` | `"en_US.UTF-8"` | System locale |
| `keyboardLayout` | `str` | `"us"` | XKB keyboard layout |
| `hashedPasswordFile` | `nullOr str` | `null` | Path to hashed password file for primary user |
| `rootHashedPasswordFile` | `nullOr str` | `null` | Path to hashed password file for root |
| `theme.flavor` | `str` | `"macchiato"` | Catppuccin flavor (latte, frappe, macchiato, mocha) |
| `theme.accent` | `str` | `"lavender"` | Catppuccin accent color |

## Extension Point

`options.nixfleet.extensions` is an empty attrset option declared on every host. Paid `nixfleet-platform` modules fill it:

```nix
# In nixfleet-platform (future)
config.nixfleet.extensions.sso = { enable = true; provider = "keycloak"; };
```
