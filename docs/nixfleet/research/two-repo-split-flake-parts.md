# Two-Repo Split via flake-parts: Feasibility Analysis

**Date:** 2026-03-25
**Status:** Research
**Context:** Can the monorepo be split into `nixfleet/` (framework) and `abstracts33d-fleet/` (org overlay) while keeping both as flake-parts flakes?

## Verdict: Yes, with conditions

The split is feasible. flake-parts has a first-class mechanism for this exact pattern: `flakeModules`. The framework exports a `flakeModules.default` that injects its deferred modules (core/, scopes/) into the client's flake-parts evaluation. The client imports it like any other flake-parts module (treefmt-nix, devenv, hercules-ci-effects all use this pattern).

The conditions are:
1. The framework's lib functions (`mkFleet`, `mkHost`, etc.) must stop capturing `config` at import time and instead receive it via module arguments or closures.
2. `import-tree` stays client-side only — the framework uses explicit imports internally.
3. Mixed modules (core/nixos.nix, scopes/dev/home.nix, etc.) must be decontaminated first (Phase 1 from `framework-vs-overlay-separation.md`).

---

## 1. flake-parts as Framework Distribution

### The `flakeModules` mechanism

flake-parts provides `flakeModules` — an option that exports flake-parts modules for consumption by other flakes. This is the standard distribution mechanism used by:

- **treefmt-nix**: `imports = [inputs.treefmt-nix.flakeModule]`
- **devenv**: `imports = [inputs.devenv.flakeModule]`
- **hercules-ci-effects**: `imports = [hercules-ci-effects.flakeModule]`

When a client flake does `imports = [inputs.nixfleet.flakeModules.default]`, the framework's module becomes part of the client's flake-parts module evaluation. It can:
- Define new options (e.g., `options.nixfleet.theme`)
- Set `config.flake.modules.nixos.*` (inject deferred NixOS modules)
- Set `config.flake.modules.darwin.*` (inject deferred Darwin modules)
- Set `config.flake.modules.homeManager.*` (inject deferred HM modules)
- Define `perSystem` outputs (packages, apps, checks)

This is exactly what we need. The framework's core/ and scopes/ modules currently define `config.flake.modules.nixos.core = {...}`, `config.flake.modules.nixos.graphical = {...}`, etc. When exported as a flakeModule, these definitions merge into the client's `config.flake.modules` namespace via the NixOS module system's standard merging behavior.

### The `importApply` pattern

When a flakeModule needs access to its own flake's inputs (not the consumer's), flake-parts provides `importApply`. The module file receives a "local flake" argument:

```nix
# nixfleet/flake-module.nix
localFlake:    # <-- the nixfleet flake's own scope
{ lib, config, self, inputs, ... }:   # <-- the consumer's flake-parts args
{
  # 'self' and 'inputs' refer to the CONSUMER's flake
  # 'localFlake' refers to NIXFLEET's own scope
}
```

This cleanly separates "framework resources" from "client resources." The framework can reference its own built-in `_config/` defaults via `localFlake` while the client's `inputs` (secrets, nixos-hardware) are accessed via the standard module arguments.

### Precedent

This is not a novel pattern. It is the documented, recommended way to distribute reusable flake-parts logic. The flake-parts documentation has a dedicated "Dogfood a Reusable Flake Module" page explaining it. Multiple production projects use it.

---

## 2. The `inputs` Problem

### Current state

`mk-host.nix` (the internal constructors) directly references `inputs.nixpkgs`, `inputs.home-manager`, `inputs.darwin`, etc. These are needed to call `nixpkgs.lib.nixosSystem`, `darwin.lib.darwinSystem`, and import HM modules.

### Strategy: Framework declares inputs, client follows

**Recommended:** The framework (`nixfleet/`) declares its own `inputs` (nixpkgs, home-manager, disko, impermanence, catppuccin, etc.). The client `follows` the framework's pinned versions:

```nix
# abstracts33d-fleet/flake.nix
inputs = {
  nixfleet.url = "github:nixfleet/nixfleet";
  nixpkgs.follows = "nixfleet/nixpkgs";           # use framework's pin
  home-manager.follows = "nixfleet/home-manager";  # use framework's pin

  # Client-only inputs (not in framework)
  secrets.url = "git+ssh://...";
  nixos-hardware.url = "github:NixOS/nixos-hardware";
};
```

**Why this direction (framework owns, client follows):**

1. **Consistency guarantee.** The framework tests against its pinned nixpkgs. If clients bring their own, module evaluation may break in subtle ways (option renames, package removals).
2. **Single lock file update.** When the framework bumps nixpkgs, all clients get the tested version via `follows`.
3. **Ergonomic.** Clients write 2 lines of `follows` instead of 15 lines of input declarations.
4. **Override escape hatch.** A client who needs a different nixpkgs pin can stop following and declare their own — the framework's constructors receive `inputs` from the module system, not from a hardcoded reference.

**Alternative (client owns, framework follows):**

```nix
# Client declares all inputs, framework follows them
inputs.nixfleet.inputs.nixpkgs.follows = "nixpkgs";
```

This gives the client more control but breaks the framework's testing assumptions. It is the right choice for advanced users who need a specific nixpkgs commit. Both patterns work with Nix flakes — it is a policy choice, not a technical limitation.

**Recommendation:** Default to "framework owns, client follows." Document the override path for advanced users.

### How constructors access inputs

Currently `mk-host.nix` receives `inputs` directly: `import ./mk-host.nix { inherit inputs config; }`. In the split model, the framework's `flakeModule` captures its own inputs via `importApply`:

```nix
# nixfleet/flake-module.nix (simplified)
localFlake:
{ config, lib, inputs, ... }:
let
  # Framework's own inputs (nixpkgs, home-manager, etc.)
  frameworkInputs = localFlake.inputs;
  # Client's inputs are in 'inputs' (for secrets, nixos-hardware, etc.)

  constructors = import ./mk-host.nix {
    inputs = frameworkInputs;
    inherit config;
  };
in {
  # ... module body
}
```

The `inputs` variable inside the flakeModule refers to the **consumer's** flake inputs. `localFlake.inputs` refers to the **framework's** inputs. When the client uses `follows`, both point to the same derivation (deduplication).

---

## 3. The `config` Problem

### Current state

`mkFleet` receives `config` from the flake-parts module arguments: `{ config, inputs, lib, ... }`. It uses `config.flake.modules.nixos` to collect all deferred modules and pass them to `mk-host.nix`, which does `builtins.attrValues nixosModules` to include them in every host.

### Solution: flakeModules participate in config

When the framework is imported as a `flakeModule`, its module definitions (core/nixos.nix setting `config.flake.modules.nixos.core = {...}`) merge into the client's `config` via the NixOS module system. By the time `mkFleet` reads `config.flake.modules.nixos`, it sees both:
- Framework-provided modules (core, scopes, etc.)
- Client-provided modules (org-specific overrides, if any)

This works because flake-parts modules are evaluated together in a single module system pass. There is no "framework config" vs "client config" — there is one unified `config` that both contribute to.

**Concrete flow:**

1. Client's `flake.nix` calls `mkFlake` with `imports = [inputs.nixfleet.flakeModules.default]`
2. flake-parts evaluates all imported modules together
3. Framework's flakeModule sets `config.flake.modules.nixos.core = {...}`, `.scopes-graphical = {...}`, etc.
4. Client's `fleet.nix` reads `config.flake.modules.nixos` — it contains everything
5. `mkFleet` passes these to `mk-host.nix` which includes them in `nixosSystem` calls

**No changes needed to `mkFleet`'s signature.** It already receives `config` from flake-parts module arguments. The split is transparent.

### Where mkFleet lives

Option A: `mkFleet` stays in the framework, exported as `nixfleet.lib.mkFleet`. The client calls it from their own flake-parts module, passing `config` from module arguments.

Option B: The framework's flakeModule declares an option (`options.nixfleet.fleet = ...`) and the client sets it. `mkFleet` runs inside the framework's module.

**Recommendation: Option A.** It matches the current pattern, is simpler to understand, and gives clients full control. Option B is more "module-system native" but adds indirection for no benefit at this stage.

```nix
# abstracts33d-fleet/fleet.nix (client's flake-parts module)
{ config, inputs, lib, ... }:
let
  nixfleet = inputs.nixfleet.lib;  # or: import from the flakeModule's localFlake
in {
  flake = nixfleet.mkFleet {
    inherit config lib;
    organizations = [ ... ];
    hosts = [ ... ];
  };
}
```

Wait — this has a subtlety. `mkFleet` currently receives `inputs` to pass to `mk-host.nix`. In the split model, `mk-host.nix` needs the **framework's** inputs (nixpkgs, home-manager). If `mkFleet` is a plain lib function, it needs those inputs injected.

**Resolution:** The framework's `lib` is constructed inside the flakeModule (which has access to `localFlake.inputs`). The client calls `nixfleet.lib.mkFleet` which already has framework inputs baked in:

```nix
# nixfleet/flake-module.nix
localFlake:
{ config, lib, ... }: let
  nixfleetLib = import ./lib/default.nix {
    inputs = localFlake.inputs;  # framework's nixpkgs, HM, etc.
    inherit config lib;
  };
in {
  # Export lib for client use
  options.nixfleet.lib = lib.mkOption {
    type = lib.types.attrs;
    default = nixfleetLib;
    internal = true;
  };

  # Or: simply expose via flake outputs
  config.flake.lib.nixfleet = nixfleetLib;
}
```

The client then does:

```nix
# abstracts33d-fleet/fleet.nix
{ config, ... }: let
  nixfleet = config.nixfleet.lib;  # or: config.flake.lib.nixfleet
in {
  flake = nixfleet.mkFleet { ... };
}
```

This is clean: the client never needs to know about framework inputs. They are captured by the flakeModule.

---

## 4. Deferred Modules Across Repos

### Current pattern

```nix
# modules/core/nixos.nix (in current monorepo)
{ config, inputs, lib, ... }: {
  config.flake.modules.nixos.core = { config, pkgs, lib, ... }: {
    # ... NixOS module body
  };
}
```

### In the split

The framework's flakeModule imports these files. They set `config.flake.modules.nixos.*` inside the client's flake-parts evaluation:

```nix
# nixfleet/flake-module.nix
localFlake:
{ ... }: {
  imports = [
    ./modules/core/nixos.nix
    ./modules/core/darwin.nix
    ./modules/core/home.nix
    ./modules/scopes/base.nix
    ./modules/scopes/catppuccin.nix
    ./modules/scopes/graphical/nixos.nix
    ./modules/scopes/graphical/home.nix
    # ... all framework modules
  ];
}
```

Each of these files is a flake-parts module that contributes to `config.flake.modules.nixos.*`. When imported via the flakeModule, they merge into the client's config namespace.

**Will they conflict with client modules?** No, as long as attribute names are unique. The framework uses names like `core`, `scopes-base`, `scopes-graphical`. If the client defines `config.flake.modules.nixos.org-secrets`, there is no collision. The module system's `attrsOf deferredModule` type merges by attribute name.

**Can the client override framework modules?** Yes, via `lib.mkForce` on specific settings within the deferred module, or by defining their own module that sets conflicting options with higher priority. They can also use `disabledModules` if they need to completely replace a framework module — though this is an advanced escape hatch.

---

## 5. Import-Tree Across Repos

### The problem

Currently, `flake.nix` does `inputs.import-tree ./modules` which auto-imports every `.nix` file (except `_`-prefixed ones) as flake-parts modules. In the split:

- The framework has its own `modules/` directory
- The client has its own `modules/` directory (fleet.nix, org-specific modules)

### Solution: import-tree stays client-side

The framework does NOT use import-tree internally. It uses explicit imports in its flakeModule (listing each core/ and scopes/ file). This gives the framework precise control over what gets imported and in what order.

The client uses import-tree for their own modules:

```nix
# abstracts33d-fleet/flake.nix
outputs = inputs:
  inputs.flake-parts.lib.mkFlake { inherit inputs; } (
    (inputs.import-tree ./modules)  # auto-imports client modules
    // {
      imports = [
        inputs.nixfleet.flakeModules.default  # framework modules
      ];
      systems = [ "x86_64-linux" "aarch64-linux" "aarch64-darwin" ];
    }
  );
```

**Subtlety:** `import-tree ./modules` returns an attrset (flake-parts module). This is merged (`//`) with the attrset containing `imports` and `systems`. The `imports` key in the latter attrset takes precedence. But `import-tree` may also produce an `imports` key if any of the client's modules have one.

**Safer approach:** Make the client's top-level module explicit:

```nix
outputs = inputs:
  inputs.flake-parts.lib.mkFlake { inherit inputs; } {
    imports = [
      inputs.nixfleet.flakeModules.default
      (inputs.import-tree ./modules)   # import-tree result as a single import
    ];
    systems = [ "x86_64-linux" "aarch64-linux" "aarch64-darwin" ];
  };
```

This avoids the `//` merge issue. `import-tree` returns a module (attrset), which can be an element of the `imports` list. flake-parts evaluates it recursively.

---

## 6. Proposed Flake Structures

### nixfleet/flake.nix (framework)

```nix
{
  description = "NixFleet - Declarative NixOS fleet management framework";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-parts.url = "github:hercules-ci/flake-parts";
    home-manager = {
      url = "github:nix-community/home-manager";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    darwin = {
      url = "github:LnL7/nix-darwin/master";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    disko = {
      url = "github:nix-community/disko";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    impermanence.url = "github:nix-community/impermanence";
    catppuccin = {
      url = "github:catppuccin/nix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    nix-index-database = {
      url = "github:Mic92/nix-index-database";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    lanzaboote = {
      url = "github:nix-community/lanzaboote/v1.0.0";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    treefmt-nix = {
      url = "github:numtide/treefmt-nix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    wrapper-modules = {
      url = "github:BirdeeHub/nix-wrapper-modules";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    nixos-anywhere = {
      url = "github:nix-community/nixos-anywhere";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = inputs@{ flake-parts, ... }:
    flake-parts.lib.mkFlake { inherit inputs; } ({ withSystem, flake-parts-lib, ... }:
    let
      inherit (flake-parts-lib) importApply;
      flakeModule = importApply ./flake-module.nix {
        inherit withSystem;
        frameworkInputs = inputs;
      };
    in {
      imports = [
        flakeModule                    # dogfood our own module
        ./modules/formatter.nix        # treefmt for framework dev
        ./modules/tests/eval.nix       # framework test infrastructure
      ];

      systems = [ "x86_64-linux" "aarch64-linux" "aarch64-darwin" "x86_64-darwin" ];

      flake = {
        flakeModules.default = flakeModule;
        flakeModule = flakeModule;     # alias

        # Standalone lib (for clients who don't use flake-parts)
        lib = {
          inherit (import ./lib/default.nix {}) mkOrg mkHost mkBatchHosts mkTestMatrix builtinRoles;
          # mkFleet is NOT here — it needs config, so it lives in the flakeModule
        };
      };
    });
}
```

### nixfleet/flake-module.nix (the distributed module)

```nix
# Receives the framework's own scope via importApply
{ frameworkInputs, withSystem }:

# Standard flake-parts module arguments (from the CONSUMER's flake)
{ config, lib, inputs, self, ... }:
let
  # Build the lib with framework inputs baked in
  nixfleetLib = import ./lib/default.nix {
    inputs = frameworkInputs;
    inherit config lib;
  };
in {
  # Import all framework deferred modules
  imports = [
    ./modules/module-options.nix       # declares flake.modules.{nixos,darwin,homeManager}
    ./modules/core/nixos.nix
    ./modules/core/darwin.nix
    ./modules/core/home.nix
    ./modules/scopes/base.nix
    ./modules/scopes/catppuccin.nix
    ./modules/scopes/nix-index.nix
    ./modules/scopes/impermanence.nix
    ./modules/scopes/graphical/nixos.nix
    ./modules/scopes/graphical/home.nix
    ./modules/scopes/dev/nixos.nix
    ./modules/scopes/dev/home.nix
    ./modules/scopes/desktop/niri.nix
    ./modules/scopes/desktop/gnome.nix
    ./modules/scopes/desktop/hyprland.nix
    ./modules/scopes/display/greetd.nix
    ./modules/scopes/display/gdm.nix
    ./modules/scopes/hardware/bluetooth.nix
    ./modules/scopes/hardware/secure-boot.nix
    ./modules/scopes/enterprise/vpn.nix
    ./modules/scopes/enterprise/auth.nix
    ./modules/scopes/enterprise/certificates.nix
    ./modules/scopes/enterprise/filesharing.nix
    ./modules/scopes/enterprise/printing.nix
    ./modules/scopes/enterprise/proxy.nix
    ./modules/scopes/darwin/aerospace.nix
    ./modules/scopes/darwin/homebrew.nix
    ./modules/scopes/darwin/karabiner.nix
    ./modules/apps.nix
    ./modules/iso.nix
    ./lib/extensions-options.nix
  ];

  # Expose lib for client consumption
  options.nixfleet = {
    lib = lib.mkOption {
      type = lib.types.attrs;
      default = nixfleetLib;
      readOnly = true;
      description = "NixFleet library functions (mkFleet, mkOrg, mkHost, etc.)";
    };
  };
}
```

### abstracts33d-fleet/flake.nix (org overlay)

```nix
{
  description = "abstracts33d infrastructure fleet";

  inputs = {
    nixfleet.url = "github:nixfleet/nixfleet";

    # Follow framework's pins for consistency
    nixpkgs.follows = "nixfleet/nixpkgs";
    flake-parts.follows = "nixfleet/flake-parts";
    home-manager.follows = "nixfleet/home-manager";
    darwin.follows = "nixfleet/darwin";

    # Org-specific inputs
    secrets = {
      url = "git+ssh://git@github.com/abstracts33d/nix-secrets.git";
      flake = false;
    };
    agenix = {
      url = "github:ryantm/agenix";
      inputs.nixpkgs.follows = "nixpkgs";
      inputs.home-manager.follows = "home-manager";
      inputs.darwin.follows = "darwin";
    };
    nixos-hardware.url = "github:NixOS/nixos-hardware/master";

    # Homebrew (Darwin-only, org choice)
    nix-homebrew.url = "github:zhaofengli-wip/nix-homebrew";
    homebrew-bundle = { url = "github:homebrew/homebrew-bundle"; flake = false; };
    homebrew-core = { url = "github:homebrew/homebrew-core"; flake = false; };
    homebrew-cask = { url = "github:homebrew/homebrew-cask"; flake = false; };

    # For client's own import-tree
    import-tree.url = "github:vic/import-tree";
  };

  outputs = inputs:
    inputs.flake-parts.lib.mkFlake { inherit inputs; } {
      imports = [
        inputs.nixfleet.flakeModules.default   # framework modules
        (inputs.import-tree ./modules)          # org-specific modules
      ];
      systems = [ "x86_64-linux" "aarch64-linux" "aarch64-darwin" "x86_64-darwin" ];
    };
}
```

### abstracts33d-fleet/modules/fleet.nix (org fleet definition)

```nix
{ config, inputs, lib, ... }:
let
  nf = config.nixfleet.lib;
  inherit (nf) mkFleet mkOrg mkHost mkBatchHosts mkTestMatrix builtinRoles;

  abstracts33d = mkOrg {
    name = "abstracts33d";
    description = "Personal infrastructure fleet";
    hostSpecDefaults = {
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
      theme = { flavor = "macchiato"; accent = "lavender"; };
      hashedPasswordFile = "/run/agenix/user-password";
      rootHashedPasswordFile = "/run/agenix/root-password";
    };
    nixosModules = [ /* agenix wiring, org NixOS overrides */ ];
    darwinModules = [ /* agenix + homebrew lists */ ];
    hmModules = [ /* claude-code settings, personal packages */ ];
  };
in {
  flake = mkFleet {
    organizations = [ abstracts33d ];
    hosts = [
      (mkHost {
        hostName = "krach";
        org = abstracts33d;
        platform = "x86_64-linux";
        hardwareModules = [
          ./hardware/krach/disk-config.nix
          ./hardware/krach/hardware-configuration.nix
        ];
        hostSpecValues = { /* ... */ };
      })
      # ... more hosts
    ];
  };
}
```

### abstracts33d-fleet/ directory structure

```
abstracts33d-fleet/
  flake.nix
  flake.lock
  modules/
    fleet.nix              # org + host definitions (the main file)
    org-secrets.nix        # agenix NixOS module (inline in fleet.nix today)
    org-darwin.nix         # homebrew casks, chime, etc.
    org-home.nix           # claude-code settings, personal packages
  hardware/
    krach/
      disk-config.nix
      hardware-configuration.nix
    ohm/
      disk-config.nix
      hardware-configuration.nix
    # ...
  config/                  # personal dotfiles (override framework defaults)
    kitty.conf
    starship.toml
    gitconfig
    nvim/
    zsh/
    karabiner.json
  keys/
    githubPublicKey
    githubPublicSigningKey
```

---

## 7. Challenges and Gotchas

### 7.1 Module option declaration conflicts

**Risk:** Both framework and client define `options.flake.modules.nixos`. The framework's `module-options.nix` declares it; if the client also has a copy, flake-parts throws "option defined multiple times."

**Mitigation:** Only the framework declares `options.flake.modules.*`. The client's `fleet.nix` reads `config.flake.modules.nixos` but never re-declares the option. Since the client imports the framework's flakeModule (which imports `module-options.nix`), the option exists. The client deletes their copy of `module-options.nix`.

### 7.2 Framework modules referencing `inputs`

**Risk:** Several core/scopes modules reference `inputs` in their flake-parts module arguments (e.g., `inputs.catppuccin`, `inputs.impermanence`). When the framework is a separate flake, `inputs` in the module arguments refers to the **consumer's** inputs, not the framework's.

**Mitigation:** The framework's flakeModule must inject its own inputs into the modules that need them. Two approaches:

- **Approach A (recommended):** Use `importApply` for modules that need framework inputs. The module file receives `frameworkInputs` as a parameter.
- **Approach B:** Define an internal option (`config.nixfleet._inputs`) set by the flakeModule, read by child modules.

Approach A is cleaner because it uses the established pattern and avoids polluting the option namespace.

### 7.3 Tests reference specific hosts

**Risk:** Current eval tests in `modules/tests/eval.nix` reference `config.flake.nixosConfigurations.krach`, etc. These hosts only exist in the client repo.

**Mitigation:** Split tests into two layers:
- **Framework tests** (in `nixfleet/`): Test generic properties — "a host with `useNiri = true` has greetd enabled", "a minimal host has no graphical packages." Use synthetic test hosts defined within the test file (the `mkTestMatrix` helper already does this).
- **Client tests** (in `abstracts33d-fleet/`): Test org-specific properties — "krach has impermanence", "aether is Darwin."

The current `mkTestMatrix` function already generates synthetic hosts for testing. The framework's tests would exclusively use this pattern.

### 7.4 Wrappers (shell, terminal) across repos

**Risk:** Wrappers bundle configs from `_config/`. In the split, `_config/` is split between framework defaults and org overrides.

**Mitigation:** Wrappers stay in the framework with framework defaults. They are functional out of the box (generic starship.toml, basic kitty.conf, universal aliases). Clients who want personalized wrappers can:
1. Build their own wrappers in their repo (importing framework's wrapper mechanism)
2. Or: the framework supports a `configDir` override (see `framework-vs-overlay-separation.md` Option A)

For the abstracts33d fleet, the portable shell/terminal is a nice-to-have, not critical. HM provides the daily-driver config. The framework's generic wrappers suffice for "SSH into a random machine" use cases.

### 7.5 catppuccin input location

**Risk:** catppuccin/nix provides nixosModules and homeModules. The framework's scopes/catppuccin.nix imports them. If catppuccin is a framework input, the client doesn't need to declare it.

**Decision:** catppuccin stays in the **framework**. It is part of the framework's opinionated-but-overridable defaults (every fleet gets themed tools). The theme flavor/accent are configurable via `hostSpec.theme.*`, but the catppuccin integration mechanism is framework-level.

### 7.6 agenix stays client-side

The framework must NOT import agenix. Secrets management is an org-level decision (agenix, sops, vault, etc.). The framework provides:
- `hostSpec.hashedPasswordFile` / `hostSpec.rootHashedPasswordFile` options (paths to secret files)
- `hostSpec.secretsPath` (hint for the secrets repo location)
- Convention documentation (expected secret names)

The client imports `inputs.agenix.nixosModules.default` in their org's `nixosModules`.

### 7.7 nix-homebrew stays client-side

Homebrew integration is org-specific (which brews/casks to install). The framework can provide a scope that enables `nix-homebrew` mechanism if the input is present, but the brew/cask lists are always org-level.

**Decision:** The framework's `scopes/darwin/homebrew.nix` provides the mechanism (nix-homebrew enable, onActivation settings). But `nix-homebrew` itself becomes a client input, because the taps (homebrew-core, homebrew-cask, homebrew-bundle) are flake inputs that the framework shouldn't pin.

---

## 8. Comparison with Non-flake-parts Alternatives

### Alternative A: Plain Nix library (no flake-parts)

```nix
# nixfleet as a plain flake exporting lib + nixosModules
{
  outputs = inputs: {
    lib = { mkFleet = ...; mkHost = ...; };
    nixosModules.default = { ... };
    homeModules.default = { ... };
  };
}
```

**Pros:**
- No flake-parts dependency for the framework
- Simpler mental model (just functions and modules)

**Cons:**
- Loses flake-parts' `perSystem` (apps, packages, formatter need manual system iteration)
- Loses deferred module merging — client would manually compose module lists
- No `importApply` pattern — must thread `inputs` manually through every function
- The current codebase is deeply flake-parts native — extracting it would be a rewrite, not a refactor

**Verdict:** Not viable without rewriting the entire module structure.

### Alternative B: NixOS module set (like nixos-hardware)

```nix
# nixfleet exports raw NixOS/Darwin/HM modules
{
  outputs = inputs: {
    nixosModules = {
      core = import ./modules/core/nixos.nix;
      graphical = import ./modules/scopes/graphical/nixos.nix;
      # ...
    };
  };
}
```

**Pros:**
- No flake-parts dependency
- Familiar pattern (nixos-hardware, agenix, etc.)

**Cons:**
- Client must manually list which modules to import: `modules = with inputs.nixfleet.nixosModules; [core graphical dev ...]`
- Loses the "deferred module" pattern (self-activating scopes via `mkIf hS.useNiri`)
- The current architecture depends on `config.flake.modules.nixos` being populated automatically — this is fundamentally a flake-parts feature
- `mkFleet` can't read `config.flake.modules` if there's no flake-parts evaluation

**Verdict:** Would work for simple NixOS modules but cannot replicate the deferred module/scope auto-activation pattern that is the core of the architecture.

### Alternative C: Flake-parts flakeModule (recommended)

As described in sections 1-6 above.

**Pros:**
- Zero architecture change — the split is a refactor, not a rewrite
- Deferred modules, scope auto-activation, `mkFleet` all work unchanged
- Standard pattern used by treefmt-nix, devenv, hercules-ci-effects
- `importApply` cleanly separates framework inputs from client inputs
- Client gets framework modules automatically via a single import line

**Cons:**
- Couples the framework to flake-parts (but the framework already is flake-parts-native)
- Clients must use flake-parts (but they already do, since `mkFleet` outputs to `flake.nixosConfigurations`)
- `import-tree` doesn't auto-discover framework modules (but explicit imports give more control)

**Verdict:** The natural choice. It preserves every architectural pattern in the current codebase.

---

## 9. Migration Path

### Phase 0: Decontaminate (prerequisite, in monorepo)

Complete the Phase 1 steps from `framework-vs-overlay-separation.md`:
- Move all org-specific values from core/scopes modules into `fleet.nix` (timezone, GPG key, ledger, brew lists, claude-code settings, personal packages)
- Add `mkDefault` to all framework-provided values
- Add `hostSpec.sshAuthorizedKeys`, `hostSpec.gpgSigningKey`, `hostSpec.theme.*`
- Remove hardcoded agenix imports from core modules — move to org's `nixosModules`/`darwinModules`

### Phase 1: Restructure monorepo as if split

Without actually creating a second repo:
1. Create `flake-module.nix` at the root — the future `nixfleet/flake-module.nix`
2. Replace `import-tree` in `flake.nix` with explicit `imports` in `flake-module.nix`
3. Add `config.flake.flakeModules.default = flakeModule;` — export it
4. Verify `nix run .#validate` still passes
5. Move `fleet.nix` to use `config.nixfleet.lib` instead of direct import

This is a low-risk refactor: the repo still works as a monorepo, but the internal structure matches the target split.

### Phase 2: Extract nixfleet/ repo

1. `git filter-branch` or manual extraction of framework files into `nixfleet/` repo
2. Add Apache 2.0 LICENSE, NOTICE, CONTRIBUTING.md
3. Create `nixfleet/flake.nix` with framework inputs
4. Create `nixfleet/flake-module.nix` (already structured in Phase 1)
5. This repo becomes `abstracts33d-fleet/` — add `inputs.nixfleet`, remove framework files
6. `follows` all shared inputs
7. Verify build

### Phase 3: Stabilize API

1. Document every `mkFleet`, `mkOrg`, `mkHost` parameter
2. Add integration tests: "a minimal client flake that imports flakeModules.default and defines one host"
3. Version the framework (semver, breaking changes = major bump)
4. First external client tests the consumption model

---

## 10. Recommendation

**Use flake-parts `flakeModules` (Alternative C).** It is the only approach that preserves the current architecture without a rewrite. The deferred module pattern, scope auto-activation, and `mkFleet` composition all depend on flake-parts' module evaluation.

The split is mechanically straightforward:
1. Framework exports `flakeModules.default` via `importApply`
2. Framework inputs are captured by the closure — clients don't thread them
3. `config.flake.modules.*` merges naturally across framework and client modules
4. `mkFleet` continues to read unified `config` — no signature change
5. Client uses `follows` for shared inputs, adds org-specific inputs separately

The main work is in Phase 0 (decontamination), which is needed regardless of whether you split repos or not. Once decontaminated, the actual extraction is a mechanical file move + flake.nix restructure.

**Do not split prematurely.** The decontamination (Phase 0) and internal restructure (Phase 1) can happen in the monorepo today, validating the pattern before any repo surgery. Only split when there is an external consumer who needs a clean import path.

---

## Sources

- [Dogfood a Reusable Flake Module - flake-parts](https://flake.parts/dogfood-a-reusable-module)
- [flakeModules option documentation - flake-parts](https://flake.parts/options/flake-parts-flakemodules)
- [flake-parts introduction](https://flake.parts/)
- [flake-parts GitHub repository](https://github.com/hercules-ci/flake-parts)
- [flakeModules.nix source](https://github.com/hercules-ci/flake-parts/blob/main/extras/flakeModules.nix)
- [VTimofeenko/flake-modules - reusable flake modules collection](https://github.com/VTimofeenko/flake-modules)
- [Flake-parts: writing custom flake modules - Vladimir Timofeenko](https://vtimofeenko.com/posts/flake-parts-writing-custom-flake-modules/)
- [import-tree - auto-import nix modules](https://github.com/vic/import-tree)
- [devenv flake-parts integration](https://devenv.sh/guides/using-with-flake-parts/)
- [treefmt-nix flake-parts module](https://flake.parts/options/treefmt-nix)
