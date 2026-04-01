# Nixfleet Simplification Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace nixfleet's 4-function DSL (mkFleet/mkOrg/mkRole/mkHost) with a single `mkHost` that returns standard `nixosSystem`/`darwinSystem`, enabling `nixos-anywhere --flake .#host root@ip` and `nixos-rebuild switch --flake .#host` without custom scripts.

**Architecture:** New `mkHost` is a closure over nixfleet's pinned inputs. It calls `nixosSystem`/`darwinSystem` directly, injecting core modules and scopes. Scopes become plain NixOS modules (no deferred registration). Fleet repos define `nixosConfigurations` in their flake outputs using `nixfleet.lib.mkHost`.

**Tech Stack:** Nix (flake-parts, nixpkgs, home-manager, disko, darwin), alejandra formatter

**Spec:** `docs/superpowers/specs/2026-03-31-nixfleet-simplification-design.md`
**ADRs:** `docs/decisions/001-005`

---

## File Structure

### nixfleet repo — files to create/modify

| Action | File | Responsibility |
|--------|------|---------------|
| **Rewrite** | `modules/_shared/lib/default.nix` | New public API: exports mkHost only + mkVmApps + diskoTemplates |
| **Rewrite** | `modules/_shared/lib/mk-host.nix` | New mkHost — closure over inputs, returns nixosSystem/darwinSystem |
| **Create** | `modules/_shared/lib/mk-vm-apps.nix` | VM helper app generator for fleet repos |
| **Delete** | `modules/_shared/lib/mk-fleet.nix` | Replaced by direct nixosConfigurations |
| **Delete** | `modules/_shared/lib/mk-org.nix` | Replaced by let bindings |
| **Delete** | `modules/_shared/lib/mk-role.nix` | Replaced by hostSpec flags |
| **Delete** | `modules/_shared/lib/mk-batch-hosts.nix` | Trivial builtins.map |
| **Delete** | `modules/_shared/lib/mk-test-matrix.nix` | Trivial helper |
| **Delete** | `modules/_shared/lib/roles.nix` | Replaced by flags |
| **Delete** | `modules/_shared/lib/flake-module.nix` | Replaced by direct exports |
| **Delete** | `modules/_shared/lib/extensions-options.nix` | Future concern |
| **Delete** | `modules/module-options.nix` | Deferred module namespace no longer needed |
| **Modify** | `modules/_shared/host-spec-module.nix` | Remove organization/role options |
| **Rewrite** | `modules/_shared/mk-host.nix` | Becomes internal constructors (mkNixosHost/mkDarwinHost/mkVmHost), consumed by new mkHost |
| **Rewrite** | `modules/core/nixos.nix` | Plain NixOS module (remove deferred registration) |
| **Rewrite** | `modules/core/darwin.nix` | Plain Darwin module (remove deferred registration) |
| **Rewrite** | `modules/scopes/base.nix` | Plain NixOS/HM modules (remove deferred registration) |
| **Rewrite** | `modules/scopes/impermanence.nix` | Plain NixOS/HM modules (remove deferred registration) |
| **Modify** | `modules/apps.nix` | Remove install/build-switch/docs apps, keep VM helpers as internal |
| **Rewrite** | `modules/flake-module.nix` | New exports: lib.mkHost, packages, nixosModules, diskoTemplates |
| **Rewrite** | `modules/fleet.nix` | Test fleet uses new mkHost directly |
| **Rewrite** | `modules/tests/eval.nix` | Update tests for new API (no org/role) |
| **Modify** | `flake.nix` | May need minor adjustments |
| **Rewrite** | `examples/client-fleet/fleet.nix` | New pattern example |

### fleet repo — files to create/modify

| Action | File | Responsibility |
|--------|------|---------------|
| **Rewrite** | `flake.nix` | Use nixfleet.lib.mkHost, define nixosConfigurations directly |
| **Delete** | `modules/fleet.nix` | Host definitions move to flake.nix |
| **Modify** | `modules/modules.nix` | Remove fleet.nix import |
| **Modify** | `modules/host-spec-fleet.nix` | Convert from deferred module to plain NixOS/HM module |
| **Modify** | `modules/hosts/web-01/default.nix` | Remove mkHost/org args, become plain NixOS module or hostSpec attrset |

---

## Part 1: nixfleet repo

### Task 1: Convert scopes from deferred to plain modules

Scopes currently register via `flake.modules.nixos.<name>`. Convert them to plain NixOS/HM modules that mkHost will import directly.

**Files:**
- Modify: `modules/scopes/base.nix`
- Modify: `modules/scopes/impermanence.nix`

- [ ] **Step 1: Convert base.nix from deferred to plain modules**

The current file registers via `flake.modules.nixos.base-packages`, `flake.modules.darwin.base-packages`, `flake.modules.homeManager.base-packages`. Convert to a plain file that returns an attrset of modules.

Replace the entire content of `modules/scopes/base.nix` with:

```nix
# Base packages — truly universal tools for ALL hosts.
# Returns { nixos, darwin, homeManager } module attrsets.
# mkHost imports these directly; they self-activate via lib.mkIf.
{
  nixos = {
    config,
    pkgs,
    lib,
    ...
  }: let
    hS = config.hostSpec;
  in {
    environment.systemPackages = with pkgs;
      lib.optionals (!hS.isMinimal) [
        unixtools.ifconfig
        unixtools.netstat
        xdg-utils
      ];
  };

  darwin = {pkgs, ...}: {
    environment.systemPackages = with pkgs; [dockutil mas];
  };

  homeManager = {
    config,
    pkgs,
    lib,
    ...
  }: let
    hS = config.hostSpec;
  in {
    home.packages = with pkgs;
      lib.optionals (!hS.isMinimal) [
        coreutils
        killall
        openssh
        wget
        age
        gnupg
        fastfetch
        gh
        duf
        eza
        fd
        fzf
        jq
        procs
        ripgrep
        tldr
        tree
        yq
        home-manager
        nh
      ];
  };
}
```

- [ ] **Step 2: Convert impermanence.nix from deferred to plain modules**

Read the current `modules/scopes/impermanence.nix` and convert the same way: remove `flake.modules.nixos.impermanence = ...` wrapping, return `{ nixos, hmLinux }` attrset.

Replace the deferred pattern:
```nix
# old
{...}: {
  flake.modules.nixos.impermanence = { config, lib, ... }: { ... };
  flake.modules.hmLinux.impermanence = { config, lib, ... }: { ... };
}
```

With plain module attrset:
```nix
# new — returns { nixos, hmLinux }
{
  nixos = { config, lib, inputs, ... }: let
    hS = config.hostSpec;
  in {
    config = lib.mkIf hS.isImpermanent {
      # ... existing impermanence NixOS config unchanged ...
    };
  };

  hmLinux = { config, lib, ... }: let
    hS = config.hostSpec;
  in {
    # ... existing impermanence HM config unchanged ...
  };
}
```

Keep the actual config bodies identical — only remove the deferred module wrapper.

- [ ] **Step 3: Verify formatting**

Run: `cd /home/s33d/dev/nix-org/nixfleet && nix fmt`

- [ ] **Step 4: Commit**

```bash
cd /home/s33d/dev/nix-org/nixfleet
git add modules/scopes/base.nix modules/scopes/impermanence.nix
git commit -m "refactor: convert scopes from deferred modules to plain module attrsets"
```

---

### Task 2: Convert core modules from deferred to plain modules

**Files:**
- Modify: `modules/core/nixos.nix`
- Modify: `modules/core/darwin.nix`

- [ ] **Step 1: Convert core/nixos.nix**

Current pattern:
```nix
{ config, inputs, lib, ... }: {
  config.flake.modules.nixos.core = { config, pkgs, lib, ... }: {
    # ... core NixOS config ...
  };
}
```

New pattern — plain NixOS module:
```nix
# Core NixOS module. Imported directly by mkHost.
{ config, pkgs, lib, inputs, ... }: let
  hS = config.hostSpec;
in {
  imports = [inputs.disko.nixosModules.default];

  # ... all existing config, unchanged, just unwrapped from the deferred layer ...
}
```

Read the full file, strip the outer `config.flake.modules.nixos.core = ...` wrapper, keep all inner config.

- [ ] **Step 2: Convert core/darwin.nix**

Same transformation: strip `config.flake.modules.darwin.core = ...` wrapper, keep inner config as a plain Darwin module.

- [ ] **Step 3: Format and commit**

```bash
cd /home/s33d/dev/nix-org/nixfleet
nix fmt
git add modules/core/nixos.nix modules/core/darwin.nix
git commit -m "refactor: convert core modules from deferred to plain NixOS/Darwin modules"
```

---

### Task 3: Update hostSpec module — remove org/role fields

**Files:**
- Modify: `modules/_shared/host-spec-module.nix`

- [ ] **Step 1: Remove organization and role options**

In `modules/_shared/host-spec-module.nix`, remove these options:

```nix
    organization = lib.mkOption {
      type = lib.types.str;
      description = "Organization this host belongs to (set by mkFleet)";
    };
    role = lib.mkOption {
      type = lib.types.nullOr lib.types.str;
      default = null;
      description = "Named role within the organization (optional)";
    };
```

Keep everything else (userName, hostName, networking, secretsPath, timeZone, locale, keyboardLayout, sshAuthorizedKeys, home, isMinimal, isDarwin, isImpermanent, isServer, hashedPasswordFile, rootHashedPasswordFile).

- [ ] **Step 2: Format and commit**

```bash
cd /home/s33d/dev/nix-org/nixfleet
nix fmt
git add modules/_shared/host-spec-module.nix
git commit -m "refactor: remove organization and role from hostSpec (ADR-001, ADR-002)"
```

---

### Task 4: Rewrite mkHost as the single API function

This is the core task. The new mkHost is a closure over nixfleet's inputs that returns `nixosSystem` or `darwinSystem`.

**Files:**
- Rewrite: `modules/_shared/lib/mk-host.nix`
- Rewrite: `modules/_shared/lib/default.nix`
- Delete: `modules/_shared/lib/mk-fleet.nix`
- Delete: `modules/_shared/lib/mk-org.nix`
- Delete: `modules/_shared/lib/mk-role.nix`
- Delete: `modules/_shared/lib/mk-batch-hosts.nix`
- Delete: `modules/_shared/lib/mk-test-matrix.nix`
- Delete: `modules/_shared/lib/roles.nix`
- Delete: `modules/_shared/lib/extensions-options.nix`
- Delete: `modules/_shared/lib/flake-module.nix`
- Delete: `modules/module-options.nix`
- Keep: `modules/_shared/mk-host.nix` (internal constructors — rename for clarity)

- [ ] **Step 1: Rewrite `modules/_shared/lib/mk-host.nix`**

This becomes the new public API. Replace entirely with:

```nix
# mkHost — the single NixFleet API function.
# Returns a nixosSystem or darwinSystem.
# Closure over framework inputs (nixpkgs, home-manager, disko, etc.).
{
  inputs,
  lib,
}: let
  hostSpecModule = ../host-spec-module.nix;

  # Import scope modules as plain attrsets
  baseScope = import ../../scopes/base.nix;
  impermanenceScope = import ../../scopes/impermanence.nix;

  # Core modules (plain NixOS/Darwin modules)
  coreNixos = ../../core/nixos.nix;
  coreDarwin = ../../core/darwin.nix;

  backupCmd = ''mv {} {}.nbkp.$(date +%Y%m%d%H%M%S) && ls -t {}.nbkp.* 2>/dev/null | tail -n +6 | xargs -r rm -f'';

  isDarwinPlatform = platform:
    builtins.elem platform ["aarch64-darwin" "x86_64-darwin"];
in
  {
    hostName,
    platform,
    stateVersion ? "24.11",
    hostSpec ? {},
    modules ? [],
    isVm ? false,
  }: let
    isDarwin = isDarwinPlatform platform;

    # Merge hostName into hostSpec (always present)
    effectiveHostSpec =
      {inherit hostName;}
      // hostSpec;

    # Framework NixOS modules injected by mkHost
    frameworkNixosModules = [
      {nixpkgs.hostPlatform = platform;}
      hostSpecModule
      {hostSpec = lib.mapAttrs (_: v: lib.mkDefault v) effectiveHostSpec;}
      # Override hostName without mkDefault (must match)
      {hostSpec.hostName = hostName;}
      coreNixos
      baseScope.nixos
      impermanenceScope.nixos
    ]
    ++ lib.optionals isVm [
      ({
        lib,
        pkgs,
        ...
      }: {
        services.spice-vdagentd.enable = true;
        networking.useDHCP = lib.mkForce true;
        environment.variables.LIBGL_ALWAYS_SOFTWARE = "1";
        environment.systemPackages = [pkgs.mesa];
      })
    ];

    # Framework Darwin modules injected by mkHost
    frameworkDarwinModules = [
      {nixpkgs.hostPlatform = platform;}
      hostSpecModule
      {hostSpec = lib.mapAttrs (_: v: lib.mkDefault v) effectiveHostSpec;}
      {hostSpec.hostName = hostName;}
      {hostSpec.isDarwin = true;}
      coreDarwin
      baseScope.darwin
    ];

    # Home-Manager modules
    hmModules =
      [
        hostSpecModule
        baseScope.homeManager
      ]
      ++ lib.optionals (!isDarwin) [
        impermanenceScope.hmLinux
      ];

    # Build NixOS system
    buildNixos =
      inputs.nixpkgs.lib.nixosSystem {
        specialArgs = {inherit inputs;};
        modules =
          frameworkNixosModules
          ++ [
            inputs.home-manager.nixosModules.home-manager
            {
              home-manager = {
                useGlobalPkgs = true;
                useUserPackages = true;
                backupCommand = backupCmd;
                users.${effectiveHostSpec.userName} = {
                  imports =
                    hmModules
                    ++ [{hostSpec = effectiveHostSpec;}];
                  home = {
                    inherit stateVersion;
                    username = effectiveHostSpec.userName;
                    homeDirectory = "/home/${effectiveHostSpec.userName}";
                    enableNixpkgsReleaseCheck = false;
                  };
                  systemd.user.startServices = "sd-switch";
                };
              };
            }
          ]
          ++ modules;
      };

    # Build Darwin system
    buildDarwin =
      inputs.darwin.lib.darwinSystem {
        specialArgs = {inherit inputs;};
        modules =
          frameworkDarwinModules
          ++ [
            inputs.home-manager.darwinModules.home-manager
            {
              home-manager = {
                useGlobalPkgs = true;
                backupCommand = backupCmd;
                users.${effectiveHostSpec.userName} = {
                  imports =
                    hmModules
                    ++ [{hostSpec = effectiveHostSpec;}];
                  home = {
                    inherit stateVersion;
                    username = effectiveHostSpec.userName;
                    homeDirectory = "/Users/${effectiveHostSpec.userName}";
                    enableNixpkgsReleaseCheck = false;
                  };
                };
              };
            }
          ]
          ++ modules;
      };
  in
    if isDarwin
    then buildDarwin
    else buildNixos
```

- [ ] **Step 2: Rewrite `modules/_shared/lib/default.nix`**

```nix
# Public API of the NixFleet framework library.
{
  inputs,
  lib,
}: {
  mkHost = import ./mk-host.nix {inherit inputs lib;};
}
```

- [ ] **Step 3: Delete removed files**

```bash
cd /home/s33d/dev/nix-org/nixfleet
rm modules/_shared/lib/mk-fleet.nix
rm modules/_shared/lib/mk-org.nix
rm modules/_shared/lib/mk-role.nix
rm modules/_shared/lib/mk-batch-hosts.nix
rm modules/_shared/lib/mk-test-matrix.nix
rm modules/_shared/lib/roles.nix
rm modules/_shared/lib/extensions-options.nix
rm modules/_shared/lib/flake-module.nix
rm modules/module-options.nix
```

- [ ] **Step 4: Delete old internal constructors file**

The old `modules/_shared/mk-host.nix` (which contained mkNixosHost/mkDarwinHost/mkVmHost) is now inlined into the new `modules/_shared/lib/mk-host.nix`. Delete it:

```bash
rm modules/_shared/mk-host.nix
```

- [ ] **Step 5: Format and commit**

```bash
cd /home/s33d/dev/nix-org/nixfleet
nix fmt
git add -A
git commit -m "feat: rewrite mkHost as single API function returning nixosSystem/darwinSystem

Removes mkFleet, mkOrg, mkRole, mkBatchHosts, mkTestMatrix, roles,
deferred module registration, and extensions.

ADR-001: mkHost over mkFleet
ADR-002: flags over roles"
```

---

### Task 5: Rewrite flake-module.nix (framework exports)

**Files:**
- Rewrite: `modules/flake-module.nix`

- [ ] **Step 1: Rewrite the framework export**

Replace `modules/flake-module.nix` entirely:

```nix
# NixFleet Framework Export
#
# Exports:
#   flake.lib.nixfleet.mkHost  — the API
#   flake.nixosModules.nixfleet-core — for users who want modules without mkHost
#   flake.diskoTemplates — reusable disk layout templates
#   perSystem packages — ISO, agent, CP, CLI binaries
{
  inputs,
  lib,
  ...
}: let
  nixfleetLib = import ./_shared/lib/default.nix {inherit inputs lib;};
in {
  config.flake = {
    # Primary API
    lib.nixfleet = nixfleetLib;

    # For consumers who don't want mkHost (just raw modules)
    nixosModules.nixfleet-core = ./core/nixos.nix;

    # Disko templates
    diskoTemplates = {
      btrfs = ./_shared/disk-templates/btrfs-disk.nix;
      btrfs-impermanence = ./_shared/disk-templates/btrfs-impermanence-disk.nix;
    };
  };
}
```

- [ ] **Step 2: Format and commit**

```bash
cd /home/s33d/dev/nix-org/nixfleet
nix fmt
git add modules/flake-module.nix
git commit -m "feat: new framework exports — lib.nixfleet.mkHost, nixosModules, diskoTemplates"
```

---

### Task 6: Slim down apps.nix — remove deployment scripts, keep VM helpers

**Files:**
- Modify: `modules/apps.nix`

- [ ] **Step 1: Read the full apps.nix**

Read `modules/apps.nix` completely. Identify and remove:
- `install` app (replaced by `nixos-anywhere` directly)
- `build-switch` app (replaced by `nixos-rebuild` directly)
- `docs` app (not a framework concern)

Keep:
- `validate` app (eval tests + host builds)
- `spawn-qemu` app
- `launch-vm` app
- `test-vm` app
- `spawn-utm` app (if it exists)
- `build-iso` / ISO-related app
- `devShells.default` (if present)

- [ ] **Step 2: Remove the install, build-switch, and docs apps**

Delete the `mkScript` blocks and app definitions for `install`, `build-switch`, and `docs`. Keep the rest.

- [ ] **Step 3: Format and commit**

```bash
cd /home/s33d/dev/nix-org/nixfleet
nix fmt
git add modules/apps.nix
git commit -m "refactor: remove install/build-switch/docs apps (ADR-004)

Deployment is now standard: nixos-anywhere, nixos-rebuild, darwin-rebuild.
VM helpers and validate remain."
```

---

### Task 7: Rewrite test fleet (modules/fleet.nix)

The framework's internal test fleet needs to use the new mkHost directly.

**Files:**
- Rewrite: `modules/fleet.nix`

- [ ] **Step 1: Rewrite fleet.nix with new mkHost**

Replace the entire `modules/fleet.nix`:

```nix
# Minimal test fleet for the NixFleet framework repo.
# These hosts exist to make eval tests pass — they are NOT a real org fleet.
# No secrets, no agenix, no real hardware.
{config, ...}: let
  mkHost = config.flake.lib.nixfleet.mkHost;

  # Shared test defaults (replaces mkOrg)
  testDefaults = {
    userName = "testuser";
    timeZone = "UTC";
    locale = "en_US.UTF-8";
    keyboardLayout = "us";
    sshAuthorizedKeys = [
      "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAINixfleetTestKeyDoNotUseInProduction"
    ];
  };
in {
  flake.nixosConfigurations = {
    # web-01: used for defaults / password / SSH tests
    web-01 = mkHost {
      hostName = "web-01";
      platform = "x86_64-linux";
      isVm = true;
      hostSpec = testDefaults // {
        isImpermanent = true;
      };
    };

    # web-02: scope activation tests
    web-02 = mkHost {
      hostName = "web-02";
      platform = "x86_64-linux";
      isVm = true;
      hostSpec = testDefaults // {
        isImpermanent = true;
      };
    };

    # dev-01: userName override
    dev-01 = mkHost {
      hostName = "dev-01";
      platform = "x86_64-linux";
      isVm = true;
      hostSpec = testDefaults // {
        userName = "alice";
      };
    };

    # edge-01: isMinimal
    edge-01 = mkHost {
      hostName = "edge-01";
      platform = "x86_64-linux";
      isVm = true;
      hostSpec = testDefaults // {
        isMinimal = true;
      };
    };

    # srv-01: server host
    srv-01 = mkHost {
      hostName = "srv-01";
      platform = "x86_64-linux";
      isVm = true;
      hostSpec = testDefaults // {
        isServer = true;
      };
    };
  };
}
```

Note: batch hosts (edge-01/02/03) and test matrix hosts are removed. The eval tests that reference them use `lib.optionalAttrs (hasHost ...)` guards, so they'll simply be skipped.

- [ ] **Step 2: Format and commit**

```bash
cd /home/s33d/dev/nix-org/nixfleet
nix fmt
git add modules/fleet.nix
git commit -m "refactor: rewrite test fleet with new mkHost API (no mkFleet/mkOrg)"
```

---

### Task 8: Update eval tests

**Files:**
- Modify: `modules/tests/eval.nix`

- [ ] **Step 1: Update eval tests for new API**

Changes needed:
- Remove `eval-org-field-exists` (no more organization/role options)
- Remove `eval-org-defaults` (no more org concept)
- Remove `eval-org-all-hosts` (no more org concept)
- Remove `eval-secrets-agnostic` (secretsPath still exists but test was about org-level default)
- Remove `eval-batch-hosts` (no batch hosts in test fleet)
- Remove `eval-test-matrix` and `eval-role-defaults` (no test matrix or roles)
- Keep `eval-ssh-hardening` (core module still does this)
- Keep `eval-username-org-default` — rename to `eval-username-override`, adapt to test that dev-01 has different userName
- Keep `eval-locale-timezone` (still set via hostSpec)
- Keep `eval-ssh-authorized` (still set via hostSpec)
- Keep `eval-password-files` (options still exist)
- Keep `eval-extensions-empty` — remove (extensions gone)

Replace the checks section:

```nix
{self, ...}: {
  perSystem = {
    pkgs,
    system,
    lib,
    ...
  }: let
    helpers = import ./_lib/helpers.nix {inherit lib;};
    mkEvalCheck = helpers.mkEvalCheck pkgs;
    nixosCfg = name: self.nixosConfigurations.${name}.config;
  in
    lib.optionalAttrs (system == "x86_64-linux") {
      checks = {
        # --- SSH hardening (core/nixos.nix) ---
        eval-ssh-hardening = let
          cfg = nixosCfg "web-02";
        in
          mkEvalCheck "ssh-hardening" [
            {
              check = cfg.services.openssh.settings.PermitRootLogin == "prohibit-password";
              msg = "PermitRootLogin is prohibit-password";
            }
            {
              check = cfg.services.openssh.settings.PasswordAuthentication == false;
              msg = "PasswordAuthentication is false";
            }
            {
              check = cfg.networking.firewall.enable;
              msg = "firewall is enabled";
            }
          ];

        # --- hostSpec defaults propagate ---
        eval-hostspec-defaults = let
          cfg = nixosCfg "web-01";
        in
          mkEvalCheck "hostspec-defaults" [
            {
              check = cfg.hostSpec.userName != "";
              msg = "web-01 should have userName set";
            }
            {
              check = cfg.hostSpec.hostName == "web-01";
              msg = "web-01 should have hostName set";
            }
          ];

        # --- userName override ---
        eval-username-override = let
          refUser = (nixosCfg "web-01").hostSpec.userName;
        in
          mkEvalCheck "username-override" [
            {
              check = refUser != "";
              msg = "web-01 should have userName from shared defaults";
            }
            {
              check = (nixosCfg "dev-01").hostSpec.userName != refUser;
              msg = "dev-01 should override userName (different from shared default)";
            }
          ];

        # --- Locale / timezone ---
        eval-locale-timezone = let
          cfg = nixosCfg "web-01";
        in
          mkEvalCheck "locale-timezone" [
            {
              check = cfg.time.timeZone != "";
              msg = "web-01 should have timezone set";
            }
            {
              check = cfg.i18n.defaultLocale != "";
              msg = "web-01 should have locale set";
            }
            {
              check = cfg.console.keyMap != "";
              msg = "web-01 should have keyboard layout set";
            }
          ];

        # --- SSH authorized keys ---
        eval-ssh-authorized = let
          cfg = nixosCfg "web-01";
          userName = cfg.hostSpec.userName;
        in
          mkEvalCheck "ssh-authorized" [
            {
              check = builtins.length cfg.users.users.${userName}.openssh.authorizedKeys.keys > 0;
              msg = "web-01 should have SSH authorized keys";
            }
            {
              check = builtins.length cfg.users.users.root.openssh.authorizedKeys.keys > 0;
              msg = "web-01 root should have SSH authorized keys";
            }
          ];

        # --- Password file options exist ---
        eval-password-files = let
          cfg = nixosCfg "web-01";
        in
          mkEvalCheck "password-files" [
            {
              check = cfg.hostSpec ? hashedPasswordFile;
              msg = "hostSpec should have hashedPasswordFile option";
            }
            {
              check = cfg.hostSpec ? rootHashedPasswordFile;
              msg = "hostSpec should have rootHashedPasswordFile option";
            }
          ];
      };
    };
}
```

- [ ] **Step 2: Format and commit**

```bash
cd /home/s33d/dev/nix-org/nixfleet
nix fmt
git add modules/tests/eval.nix
git commit -m "refactor: update eval tests for new mkHost API (no org/role/batch/matrix)"
```

---

### Task 9: Update flake.nix and verify eval

**Files:**
- Modify: `flake.nix` (if needed)

- [ ] **Step 1: Check if flake.nix needs changes**

The flake.nix uses import-tree to auto-import all `.nix` files under `modules/`. The deleted files (mk-fleet.nix, etc.) are under `_shared/lib/` which is `_`-prefixed and excluded from import-tree. The `module-options.nix` file IS auto-imported — its deletion might cause issues.

Check: does import-tree skip deleted files automatically? Yes — it only imports files that exist. But `module-options.nix` was auto-imported and declared the `flake.modules.*` namespace. Since nothing references it anymore (scopes are plain modules), its deletion is safe.

No changes to `flake.nix` needed — import-tree handles the rest.

- [ ] **Step 2: Run eval tests**

```bash
cd /home/s33d/dev/nix-org/nixfleet
nix flake check --no-build
```

Expected: all eval checks pass. If not, debug the specific failure.

- [ ] **Step 3: Verify a test host evaluates**

```bash
cd /home/s33d/dev/nix-org/nixfleet
nix eval .#nixosConfigurations.web-02.config.hostSpec.userName
```

Expected: `"testuser"`

```bash
nix eval .#nixosConfigurations.dev-01.config.hostSpec.userName
```

Expected: `"alice"`

- [ ] **Step 4: Commit if any fixes were needed**

```bash
cd /home/s33d/dev/nix-org/nixfleet
git add -A
git commit -m "fix: resolve eval issues from simplification migration"
```

---

### Task 10: Update example client fleet

**Files:**
- Rewrite: `examples/client-fleet/fleet.nix`

- [ ] **Step 1: Rewrite example**

Replace `examples/client-fleet/fleet.nix`:

```nix
# Example: Acme Corp fleet using NixFleet framework
{config, ...}: let
  mkHost = config.flake.lib.nixfleet.mkHost;

  # Organization defaults (replaces mkOrg)
  acme = {
    userName = "deploy";
    timeZone = "America/New_York";
    locale = "en_US.UTF-8";
    keyboardLayout = "us";
  };
in {
  flake.nixosConfigurations = {
    # Developer workstation
    dev-01 = mkHost {
      hostName = "dev-01";
      platform = "x86_64-linux";
      hostSpec = acme // {
        isImpermanent = true;
        # Fleet-specific flags would be declared by a fleet hostSpec module
      };
      modules = [
        # ./hosts/dev-01/hardware.nix
        # ./hosts/dev-01/disk-config.nix
      ];
    };

    # Production server
    prod-web-01 = mkHost {
      hostName = "prod-web-01";
      platform = "x86_64-linux";
      hostSpec = acme // {
        isServer = true;
        isMinimal = true;
      };
      modules = [
        # ./hosts/prod-web-01/hardware.nix
        # ./hosts/prod-web-01/disk-config.nix
      ];
    };
  };
}
```

- [ ] **Step 2: Format and commit**

```bash
cd /home/s33d/dev/nix-org/nixfleet
nix fmt
git add examples/client-fleet/fleet.nix
git commit -m "docs: update example client fleet to new mkHost API"
```

---

## Part 2: fleet repo

### Task 11: Rewrite fleet flake.nix with new mkHost

**Files:**
- Rewrite: `flake.nix`

- [ ] **Step 1: Point nixfleet input to local path for development**

During migration, use a local path instead of GitHub:

```nix
nixfleet.url = "path:/home/s33d/dev/nix-org/nixfleet";
```

(Revert to `github:abstracts33d/nixfleet` after migration is verified.)

- [ ] **Step 2: Rewrite flake.nix**

Replace `flake.nix` entirely:

```nix
{
  description = "abstracts33d fleet — NixOS fleet configuration consuming NixFleet framework";
  inputs = {
    # Framework
    nixfleet.url = "path:/home/s33d/dev/nix-org/nixfleet";

    # Follow framework inputs for consistency
    nixpkgs.follows = "nixfleet/nixpkgs";
    flake-parts.follows = "nixfleet/flake-parts";
    import-tree.follows = "nixfleet/import-tree";
    home-manager.follows = "nixfleet/home-manager";
    darwin.follows = "nixfleet/darwin";
    disko.follows = "nixfleet/disko";
    impermanence.follows = "nixfleet/impermanence";
    agenix.follows = "nixfleet/agenix";
    treefmt-nix.follows = "nixfleet/treefmt-nix";
    nixos-anywhere.follows = "nixfleet/nixos-anywhere";
    nixos-hardware.follows = "nixfleet/nixos-hardware";
    lanzaboote.follows = "nixfleet/lanzaboote";

    # Fleet-specific inputs
    catppuccin.url = "github:catppuccin/nix";
    catppuccin.inputs.nixpkgs.follows = "nixpkgs";
    nix-index-database.url = "github:nix-community/nix-index-database";
    nix-index-database.inputs.nixpkgs.follows = "nixpkgs";
    wrapper-modules.url = "github:BirdeeHub/nix-wrapper-modules";
    wrapper-modules.inputs.nixpkgs.follows = "nixpkgs";
    claude-desktop-linux.url = "github:k3d3/claude-desktop-linux-flake";
    claude-desktop-linux.inputs.nixpkgs.follows = "nixpkgs";
    nixvim.url = "github:nix-community/nixvim";
    nixvim.inputs.nixpkgs.follows = "nixpkgs";
    nix-homebrew.url = "github:zhaofengli-wip/nix-homebrew";
    homebrew-bundle = {
      url = "github:homebrew/homebrew-bundle";
      flake = false;
    };
    homebrew-core = {
      url = "github:homebrew/homebrew-core";
      flake = false;
    };
    homebrew-cask = {
      url = "github:homebrew/homebrew-cask";
      flake = false;
    };
    nix-devshells = {
      url = "github:abstracts33d/nix-devshells";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    secrets = {
      url = "git+ssh://git@github.com/abstracts33d/fleet-secrets.git";
      flake = false;
    };
  };

  outputs = inputs: let
    mkHost = inputs.nixfleet.lib.nixfleet.mkHost;

    # Organization defaults (replaces mkOrg)
    org = {
      userName = "s33d";
      githubUser = "abstracts33d";
      githubEmail = "abstract.s33d@gmail.com";
      timeZone = "Europe/Paris";
      locale = "en_US.UTF-8";
      keyboardLayout = "us";
      gpgSigningKey = "77C21CC574933465";
      sshAuthorizedKeys = [
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIB+qnpVT15QebM41WFgwktTMP6W/KXymb8gxNV0bu5dw"
      ];
      hashedPasswordFile = "/run/agenix/user-password";
      rootHashedPasswordFile = "/run/agenix/root-password";
      theme = {
        flavor = "macchiato";
        accent = "peach";
      };
    };

    # Modules shared across all fleet hosts
    fleetModules = [
      ./modules/host-spec-fleet.nix
      ./modules/modules.nix
      # Agenix + secrets wiring (was in mkOrg.nixosModules)
      ./modules/org-secrets.nix
    ];

    fleetDarwinModules = [
      ./modules/host-spec-fleet.nix
      # Darwin-specific modules
      ./modules/org-secrets-darwin.nix
    ];
  in
    inputs.flake-parts.lib.mkFlake {inherit inputs;} {
      systems = ["x86_64-linux" "aarch64-linux" "aarch64-darwin" "x86_64-darwin"];

      imports = [
        # VM helpers from nixfleet
        inputs.nixfleet.flakeModules.apps
        # Tests
        inputs.nixfleet.flakeModules.tests
        # ISO
        inputs.nixfleet.flakeModules.iso
        # Formatter
        inputs.nixfleet.flakeModules.formatter
        # Rails devshells
        ./modules/devshells.nix
      ];

      flake = {
        nixosConfigurations = {
          web-01 = mkHost {
            hostName = "web-01";
            platform = "x86_64-linux";
            stateVersion = "26.05";
            hostSpec = org // {
              networking.interface = "enp6s0";
              isImpermanent = true;
              isDev = true;
              useHyprland = true;
              useGnome = true;
              wifiNetworks = ["home"];
              hasBluetooth = true;
              useNvidia = true;
              cpuVendor = "amd";
            };
            modules = fleetModules ++ [
              ./modules/hosts/web-01/hardware-configuration.nix
              ./modules/hosts/web-01/disk-config.nix
              {
                wayland.windowManager.hyprland.settings.monitor = "DP-1, 5120x1440@120, 0x0, 1";
              }
            ];
          };

          dev-01 = mkHost {
            hostName = "dev-01";
            platform = "x86_64-linux";
            stateVersion = "24.11";
            hostSpec = org // {
              userName = "alice";
              isImpermanent = true;
              useGnome = true;
              keyboardLayout = "fr";
            };
            modules = fleetModules ++ [
              ./modules/hosts/dev-01/hardware-configuration.nix
              ./modules/hosts/dev-01/disk-config.nix
            ];
          };

          srv-01 = mkHost {
            hostName = "srv-01";
            platform = "x86_64-linux";
            stateVersion = "24.11";
            hostSpec = org // {
              isImpermanent = true;
              isServer = true;
            };
            modules = fleetModules ++ [
              ./modules/hosts/srv-01/hardware-configuration.nix
              ./modules/hosts/srv-01/disk-config.nix
            ];
          };

          # VMs
          web-02 = mkHost {
            hostName = "web-02";
            platform = "x86_64-linux";
            isVm = true;
            hostSpec = org // {
              isImpermanent = true;
              isDev = true;
              useHyprland = true;
            };
            modules = fleetModules ++ [
              {environment.sessionVariables.LIBGL_ALWAYS_SOFTWARE = "1";}
            ];
          };

          edge-01 = mkHost {
            hostName = "edge-01";
            platform = "x86_64-linux";
            isVm = true;
            hostSpec = org // {isMinimal = true;};
            modules = fleetModules;
          };
        };

        darwinConfigurations = {
          mac-01 = mkHost {
            hostName = "mac-01";
            platform = "aarch64-darwin";
            hostSpec = org // {isDev = true;};
            modules = fleetDarwinModules ++ [
              ./modules/hosts/mac-01/default.nix
            ];
          };
        };
      };
    };
}
```

**Important notes:**
- The org secrets wiring (agenix config that was in `mkOrg.nixosModules`) needs to be extracted into `modules/org-secrets.nix` and `modules/org-secrets-darwin.nix` (next task)
- The `flakeModules.apps/tests/iso/formatter` imports may need updating depending on whether nixfleet still exports them. If not, these lines get removed and fleet defines its own formatter/tests.
- The `web-02` and `utm` VMs are omitted for brevity — add them similarly.

- [ ] **Step 3: Commit (will likely have eval errors — that's expected)**

```bash
cd /home/s33d/dev/nix-org/fleet
git add flake.nix
git commit -m "wip: rewrite flake.nix with new mkHost API"
```

---

### Task 12: Extract org secrets into fleet modules

The agenix config that was inline in `mkOrg.nixosModules` needs to become standalone fleet modules.

**Files:**
- Create: `modules/org-secrets.nix`
- Create: `modules/org-secrets-darwin.nix`

- [ ] **Step 1: Create `modules/org-secrets.nix`**

Extract the NixOS agenix wiring from the old `fleet.nix` mkOrg nixosModules:

```nix
# Org-level secrets wiring (agenix)
{
  config,
  inputs,
  pkgs,
  lib,
  ...
}: let
  hS = config.hostSpec;
in {
  imports = [inputs.agenix.nixosModules.default];

  age = {
    identityPaths =
      ["${hS.home}/.keys/id_ed25519"]
      ++ lib.optional hS.isImpermanent "/persist${hS.home}/.keys/id_ed25519";
    secrets =
      {
        "github-ssh-key" = {
          symlink = true;
          path = "${hS.home}/.ssh/id_ed25519";
          file = "${inputs.secrets}/github-ssh-key.age";
          mode = "600";
          owner = "${hS.userName}";
          group = "wheel";
        };
        "github-signing-key" = {
          symlink = true;
          path = "${hS.home}/.ssh/pgp_github.key";
          file = "${inputs.secrets}/github-signing-key.age";
          mode = "600";
          owner = "${hS.userName}";
          group = "wheel";
        };
        "user-password" = {
          file = "${inputs.secrets}/${hS.userName}-hashed-password-file.age";
          mode = "600";
          owner = "root";
          group = "root";
        };
        "root-password" = {
          file = "${inputs.secrets}/hashed-password-file.age";
          mode = "600";
          owner = "root";
          group = "root";
        };
      }
      // lib.listToAttrs (map (name: {
          name = "wifi-${name}";
          value = {
            file = "${inputs.secrets}/wifi-${name}.age";
            mode = "600";
            owner = "root";
            group = "root";
          };
        })
        hS.wifiNetworks);
  };

  systemd.services.bootstrap-wifi = lib.mkIf (hS.wifiNetworks != []) {
    description = "Bootstrap WiFi connections from agenix secrets";
    after = ["agenix.service"];
    before = ["NetworkManager.service"];
    wantedBy = ["multi-user.target"];
    serviceConfig.Type = "oneshot";
    script = let
      nmDir =
        if hS.isImpermanent
        then "/persist/system/etc/NetworkManager/system-connections"
        else "/etc/NetworkManager/system-connections";
    in
      lib.concatMapStringsSep "\n" (name: ''
        target="${nmDir}/${name}.nmconnection"
        if [ ! -f "$target" ]; then
          mkdir -p "${nmDir}"
          cp /run/agenix/wifi-${name} "$target"
          chmod 600 "$target"
        fi
      '')
      hS.wifiNetworks;
  };

  environment.systemPackages = [
    inputs.agenix.packages.${pkgs.stdenv.hostPlatform.system}.default
  ];
}
```

- [ ] **Step 2: Create `modules/org-secrets-darwin.nix`**

```nix
# Org-level secrets wiring for Darwin (agenix)
{
  config,
  inputs,
  pkgs,
  ...
}: let
  hS = config.hostSpec;
in {
  imports = [inputs.agenix.darwinModules.default];

  age = {
    identityPaths = ["${hS.home}/.keys/id_ed25519"];
    secrets = {
      "github-ssh-key" = {
        symlink = false;
        path = "${hS.home}/.ssh/id_ed25519";
        file = "${inputs.secrets}/github-ssh-key.age";
        mode = "600";
        owner = "${hS.userName}";
      };
      "github-signing-key" = {
        symlink = false;
        path = "${hS.home}/.ssh/pgp_github.key";
        file = "${inputs.secrets}/github-signing-key.age";
        mode = "600";
        owner = "${hS.userName}";
      };
    };
  };

  environment.systemPackages = [
    inputs.agenix.packages.${pkgs.stdenv.hostPlatform.system}.default
  ];
}
```

- [ ] **Step 3: Format and commit**

```bash
cd /home/s33d/dev/nix-org/fleet
nix fmt
git add modules/org-secrets.nix modules/org-secrets-darwin.nix
git commit -m "refactor: extract org secrets wiring from mkOrg into standalone modules"
```

---

### Task 13: Convert host-spec-fleet.nix from deferred to plain module

**Files:**
- Modify: `modules/host-spec-fleet.nix`

- [ ] **Step 1: Read the current file**

Read `modules/host-spec-fleet.nix` and identify the deferred module patterns. Currently it declares options and smart defaults via `flake.modules.nixos.*`, `flake.modules.darwin.*`, `flake.modules.homeManager.*`.

- [ ] **Step 2: Convert to plain NixOS/HM module**

The fleet-specific hostSpec options need to be available in both NixOS and HM contexts. Since mkHost now imports modules directly (not via deferred registration), this file should be a plain NixOS module that declares the additional hostSpec options.

Strip the `flake.modules.nixos.host-spec-fleet = ...` wrapper and make it a direct NixOS module. The smart defaults (e.g., useHyprland implies isGraphical) stay as `lib.mkDefault` inside the module.

- [ ] **Step 3: Format and commit**

```bash
cd /home/s33d/dev/nix-org/fleet
nix fmt
git add modules/host-spec-fleet.nix
git commit -m "refactor: convert host-spec-fleet from deferred to plain NixOS module"
```

---

### Task 14: Clean up fleet modules.nix

**Files:**
- Modify: `modules/modules.nix`

- [ ] **Step 1: Remove fleet.nix import**

The old `modules/modules.nix` imported `fleet.nix` (or fleet.nix was imported directly from flake.nix). Since host definitions now live in `flake.nix`, remove the fleet.nix reference. Keep the core/home.nix, core/home-linux.nix, and scopes imports.

- [ ] **Step 2: Delete modules/fleet.nix**

```bash
rm /home/s33d/dev/nix-org/fleet/modules/fleet.nix
```

- [ ] **Step 3: Format and commit**

```bash
cd /home/s33d/dev/nix-org/fleet
nix fmt
git add modules/modules.nix
git rm modules/fleet.nix
git commit -m "refactor: remove fleet.nix, host definitions now in flake.nix"
```

---

### Task 15: Verify fleet evaluation

**Files:** None — verification only

- [ ] **Step 1: Evaluate a host**

```bash
cd /home/s33d/dev/nix-org/fleet
nix eval .#nixosConfigurations.web-01.config.hostSpec.userName
```

Expected: `"s33d"`

```bash
nix eval .#nixosConfigurations.web-01.config.hostSpec.hostName
```

Expected: `"web-01"`

- [ ] **Step 2: Evaluate Darwin host**

```bash
nix eval .#darwinConfigurations.mac-01.config.hostSpec.userName
```

Expected: `"s33d"`

- [ ] **Step 3: Run eval tests (if fleet has them)**

```bash
nix flake check --no-build 2>&1 | head -50
```

Fix any errors.

- [ ] **Step 4: Test standard deployment commands parse correctly**

```bash
# Verify the nixosConfiguration builds (just eval, no actual build)
nix eval .#nixosConfigurations.web-01.config.system.build.toplevel.drvPath
```

Expected: a `/nix/store/...` derivation path (means the config evaluates successfully).

- [ ] **Step 5: Commit any fixes**

```bash
cd /home/s33d/dev/nix-org/fleet
git add -A
git commit -m "fix: resolve eval issues from fleet migration"
```

---

## Notes for Implementation

### Scope conversion details

Each scope file currently uses the deferred pattern:
```nix
{...}: {
  flake.modules.nixos.<name> = { config, ... }: { ... };
  flake.modules.homeManager.<name> = { config, ... }: { ... };
}
```

The new pattern returns a plain attrset:
```nix
{
  nixos = { config, ... }: { ... };
  homeManager = { config, ... }: { ... };
}
```

mkHost then imports individual keys: `baseScope.nixos`, `baseScope.homeManager`, etc.

### The `inputs` problem in scope modules

Some scope modules (impermanence, core/nixos) reference `inputs` for disko, impermanence, etc. In the deferred pattern, `inputs` came from the flake-parts module args. In the new pattern, these modules are plain NixOS modules — `inputs` must be passed through.

Solution: mkHost adds `inputs` to `specialArgs` so all NixOS modules can access it:

```nix
inputs.nixpkgs.lib.nixosSystem {
  specialArgs = {inherit inputs;};
  modules = [ ... ];
};
```

This is already a common pattern in NixOS flakes. Make sure to add this in Task 4 (mkHost rewrite). Similarly for darwinSystem.

### Fleet scopes that use the deferred pattern

Fleet scopes in `modules/scopes/` (catppuccin, hyprland, dev, etc.) also use the deferred `flake.modules.*` pattern. These will need the same conversion. However, since they're imported via import-tree into modules.nix (which is imported as a flake-parts module), they might still work with the deferred pattern as long as `module-options.nix` is available.

**Decision:** Fleet scopes can be converted incrementally. The immediate priority is getting nixfleet's framework scopes converted and mkHost working. Fleet scopes can keep the deferred pattern temporarily if fleet's flake.nix still uses flake-parts (which it does).

### flakeModules that fleet still imports

The fleet flake.nix currently imports `flakeModules.apps`, `.tests`, `.iso`, `.formatter` from nixfleet. After the simplification, nixfleet may or may not still export these. If they're removed, fleet needs its own:
- Formatter (treefmt-nix — planned in fleet enhancements Phase 1)
- Tests (fleet can define its own eval checks)
- Apps (VM helpers via `mkVmApps`)
- ISO (built from nixfleet packages directly)

**For now:** Keep the flakeModules imports in fleet's flake.nix if nixfleet still exports them. Migrate incrementally.
