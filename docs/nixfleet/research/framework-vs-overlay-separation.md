# Framework vs Organization Overlay Separation

**Date:** 2026-03-25
**Status:** Research / Design Analysis
**Context:** Preparing for NixFleet extraction (S8) — defining the boundary between reusable framework and org-specific config.

## 1. Module-by-Module Classification

### Framework: Pure API (`modules/_shared/lib/`)

| File | Classification | Reason |
|------|---------------|--------|
| `_shared/lib/default.nix` | Framework | Public API entry point (mkFleet, mkOrg, mkRole, mkHost) |
| `_shared/lib/mk-fleet.nix` | Framework | Fleet builder — wires org/role/host composition |
| `_shared/lib/mk-org.nix` | Framework | Organization factory |
| `_shared/lib/mk-role.nix` | Framework | Role factory |
| `_shared/lib/mk-host.nix` | Framework | Host descriptor builder |
| `_shared/lib/mk-batch-hosts.nix` | Framework | Batch host generator |
| `_shared/lib/mk-test-matrix.nix` | Framework | Test matrix generator |
| `_shared/lib/roles.nix` | Framework | Built-in roles (workstation, server, minimal, edge, etc.) |
| `_shared/lib/extensions-options.nix` | Framework | Extension point namespace for paid modules |
| `_shared/host-spec-module.nix` | Framework | Generic hostSpec option definitions |
| `_shared/mk-host.nix` | Framework | Internal NixOS/Darwin/VM constructors |

These files have zero org-specific content. They are the future `nixfleet/` repo, extractable as-is.

### Framework: Core Modules (with org-specific contamination)

| File | Classification | Org-specific contamination |
|------|---------------|---------------------------|
| `core/nixos.nix` | **Mixed** | See detailed breakdown below |
| `core/darwin.nix` | **Mixed** | See detailed breakdown below |
| `core/home.nix` | **Framework** (pure) | Only imports `_home/*.nix` — no hardcoded values |
| `core/_home/zsh.nix` | **Mixed** | Generic shell config, but `BROWSER = "firefox"`, `WORKSPACE = "$HOME/Dev"`, `GITHUB_USERNAME = hS.githubUser`, zplug plugin selection are preference |
| `core/_home/git.nix` | **Mixed** | Generic git settings, but `signing.key = "77C21CC574933465"` is a personal GPG key |
| `core/_home/ssh.nix` | **Framework** | Fully parameterized via hS — no hardcoded values |
| `core/_home/keys.nix` | **Org overlay** | Hardcoded public key files (`githubPublicKey`, `githubPublicSigningKey`) |
| `core/_home/starship.nix` | **Framework** | Just loads `_config/starship.toml` via HM — the TOML itself is preference though |
| `core/_home/neovim.nix` | **Framework** (mechanism) | Loads `_config/nvim/` — nvim config itself is preference |
| `core/_home/tmux.nix` | **Mixed** | Generic tmux setup but keybindings, catppuccin flavor hardcoded, prefix choice (`C-a`) are preference |
| `core/_home/simple.nix` | **Framework** | Tool enables (bat, btop, kitty, etc.) — programs any workstation would want |
| `core/_home/gpg.nix` | **Mixed** | GPG key import path is generic (uses `hS.home`), but assumes `pgp_github.key` filename convention |

### Framework: Scopes (generic, reusable)

| File | Classification | Notes |
|------|---------------|-------|
| `scopes/base.nix` | **Framework** | Universal CLI tools — every fleet needs these |
| `scopes/catppuccin.nix` | **Mixed** | Mechanism is framework; `flavor = "macchiato"`, `accent = "lavender"` are org preference |
| `scopes/nix-index.nix` | **Framework** | Universally useful nix tooling |
| `scopes/impermanence.nix` | **Framework** | Generic btrfs wipe + persist — any impermanent host needs this |
| `scopes/graphical/nixos.nix` | **Mixed** | Pipewire, XDG portal, fonts = framework. But `libreoffice`, `vlc`, `flameshot` in systemPackages = preference |
| `scopes/graphical/home.nix` | **Mixed** | Firefox, Chrome enables = framework. But `brave` with specific extension IDs, `halloy`, `spotifyd`, `cmus`, `mpd`, `neomutt` = personal preference |
| `scopes/dev/nixos.nix` | **Mixed** | Docker, postgresql = framework. `vscode` in systemPackages = debatable preference |
| `scopes/dev/home.nix` | **Heavy org overlay** | `claude-code` settings with personal preferences (language, memory, rules, MCP servers), personal packages (redis, pgcli, iredis, nodejs, python, docker), specific tool allow-lists (`bin/rails`, `bundle`) |
| `scopes/desktop/niri.nix` | **Framework** | Niri compositor integration — generic for any org using Niri |
| `scopes/desktop/gnome.nix` | **Framework** | GNOME integration |
| `scopes/desktop/hyprland.nix` | **Framework** | Hyprland integration |
| `scopes/display/greetd.nix` | **Framework** | Display manager |
| `scopes/display/gdm.nix` | **Framework** | Display manager |
| `scopes/hardware/bluetooth.nix` | **Framework** | Hardware support |
| `scopes/hardware/secure-boot.nix` | **Framework** | Lanzaboote integration |
| `scopes/enterprise/vpn.nix` | **Framework** | Enterprise VPN stub |
| `scopes/enterprise/auth.nix` | **Framework** | Enterprise auth stub |
| `scopes/enterprise/certificates.nix` | **Framework** | Enterprise certs stub |
| `scopes/enterprise/filesharing.nix` | **Framework** | Enterprise filesharing stub |
| `scopes/enterprise/printing.nix` | **Framework** | Enterprise printing stub |
| `scopes/enterprise/proxy.nix` | **Framework** | Enterprise proxy stub |
| `scopes/darwin/aerospace.nix` | **Framework** | AeroSpace WM — generic |
| `scopes/darwin/homebrew.nix` | **Heavy org overlay** | `nix-homebrew` mechanism = framework. But `brews` list (pinentry-mac, gmp, libyaml, openssl, redis, libxml2, xz) and `casks` list (rubymine, discord, notion, slack, etc.) are entirely personal |
| `scopes/darwin/karabiner.nix` | **Org overlay** | Mechanism is generic, but `karabiner.json` content is personal |

### Org Overlay (always org-specific)

| File | Classification | Reason |
|------|---------------|--------|
| `fleet.nix` | **Org overlay** | Organization definition, host list, hardware refs, dock layout, per-host packages (jetbrains, slack) |
| `_hardware/*` | **Org overlay** | Disk configs and hardware-configuration.nix are machine-specific |
| `_config/kitty.conf` | **Org overlay** | Personal terminal preferences |
| `_config/starship.toml` | **Org overlay** | Personal prompt style |
| `_config/gitconfig` | **Org overlay** | Git preferences (though these overlap with framework good defaults) |
| `_config/githubPublicKey` | **Org overlay** | Personal SSH public key |
| `_config/githubPublicSigningKey` | **Org overlay** | Personal GPG signing key |
| `_config/karabiner.json` | **Org overlay** | Personal keyboard remapping |
| `_config/nvim/` | **Org overlay** | Personal neovim configuration |
| `_config/zsh/aliases.zsh` | **Mixed** | Some aliases are universally useful, some are personal |
| `_config/zsh/functions.zsh` | **Mixed** | Same — utility functions vs personal workflow |
| `_config/zsh/wrapperrc.zsh` | **Mixed** | Wrapper shell init — mechanism is framework, content is preference |
| `_shared/keys.nix` | **Org overlay** | Hardcoded SSH public key for authorized_keys |

### Infrastructure (framework tooling)

| File | Classification | Notes |
|------|---------------|-------|
| `module-options.nix` | **Framework** | Declares deferred module namespaces |
| `formatter.nix` | **Framework** | treefmt config |
| `iso.nix` | **Framework** | ISO builder |
| `apps.nix` | **Framework** | CLI tools (build-switch, validate, install, test-vm, etc.) |
| `wrappers/shell.nix` | **Mixed** | Wrapper mechanism = framework. Package list and config sourcing = org preference |
| `wrappers/terminal.nix` | **Mixed** | Same — kitty wrapping shell is framework, kitty.conf content is preference |
| `tests/eval.nix` | **Framework** | Test infrastructure + generic assertions |
| `tests/vm.nix` | **Framework** | VM test infrastructure |

## 2. Fuzzy Boundaries — Detailed Splits

### `modules/core/nixos.nix` — The biggest tangle

**Framework (extract to `nixfleet/modules/core/nixos.nix`):**
- nixpkgs config (`allowUnfree`, `allowBroken`, etc.)
- nix settings (experimental-features, gc, cachix substituters)
- Boot config (systemd-boot, kernel modules, latest kernel)
- Networking (hostName from hS, DHCP, NetworkManager, firewall)
- Programs (gnupg agent, dconf, git, zsh shell enable)
- Security (polkit, sudo rules)
- User creation (isNormalUser, extraGroups, shell, hashedPasswordFile from run/agenix)
- SSH server (enable, hardening settings)
- WiFi bootstrap service (mechanism — reads hS.wifiNetworks)
- Claude Code managed policy (`/etc/claude-code/settings.json` deny list)
- system.stateVersion

**Org overlay (extract to org repo or make configurable):**
- `nix.nixPath` — hardcoded `nixos-config` path assumption (`${hS.home}/.local/share/src/nixos-config`)
- `time.timeZone = "Europe/Paris"` — should be in hostSpec or org defaults
- `i18n.defaultLocale = "en_US.UTF-8"` — reasonable default, but should be overridable
- `boot.initrd.availableKernelModules` list — mix of generic and hardware-specific (nvme, ahci, xhci_pci are generic enough)
- `services.xserver.xkb.layout = "us"` — locale preference
- `hardware.ledger.enable = true` — very personal (hardware wallet)
- `_shared/keys.nix` import — hardcoded SSH authorized_keys
- agenix secret paths — the secret file names (`github-ssh-key.age`, `github-signing-key.age`, `${hS.userName}-hashed-password-file`, `shashed-password-file`, `wifi-${name}.age`) are conventions tied to the abstracts33d nix-secrets repo structure

**Proposed split:**

The framework should provide a _mechanism_ for each of these, not hardcoded values:

```nix
# Framework: nixfleet/modules/core/nixos.nix
{
  # Sensible defaults (overridable via mkDefault)
  time.timeZone = lib.mkDefault "UTC";
  i18n.defaultLocale = lib.mkDefault "en_US.UTF-8";
  services.xserver.xkb.layout = lib.mkDefault "us";

  # Secrets: framework provides the SHAPE, org fills the VALUES
  # No agenix import here — org chooses their secret backend
  # Framework only uses hS.secretsPath as a hint
}
```

```nix
# Org overlay: abstracts33d/core.nix
{
  time.timeZone = "Europe/Paris";
  hardware.ledger.enable = true;
  age.secrets = { ... };  # org-specific agenix wiring
}
```

### `modules/core/darwin.nix` — Similar pattern

**Framework:**
- nixpkgs config
- nix settings (Determinate-compatible, substituters)
- zsh enable
- User creation (parameterized via hS)
- TouchID sudo
- system.defaults (keyboard, dock, finder, trackpad — these are opinionated but sensible defaults)
- Dock management mechanism (`local.dock` option + activation script)

**Org overlay:**
- agenix imports and secret paths (github-ssh-key, github-signing-key) — same as NixOS
- `system.startup.chime = false` — preference
- Specific homebrew brews in `extraModules` (openssl@3 on aether)
- Dock entries (defined in fleet.nix, but the `local.dock` mechanism is framework)
- The entire dock entry list (Slack, Notion, Obsidian, etc.) — in fleet.nix already, good

### `modules/core/_home/git.nix` — GPG key leak

**Framework:** Everything except the signing key.
```nix
# Framework default
programs.git.signing = {
  format = lib.mkDefault "openpgp";
  signByDefault = lib.mkDefault true;
  # key = ???  -- must come from org overlay or hostSpec
};
```

**Org overlay:** `signing.key = "77C21CC574933465"` — this is a personal GPG key fingerprint.

**Fix:** Add `hostSpec.gpgSigningKey` option (nullable string), or let orgs override via `extraHmModules`.

### `modules/scopes/dev/home.nix` — Heaviest contamination

This file is ~80% org-specific:

**Framework (keep):**
- `programs.direnv` enable + integrations
- `programs.mise` enable + integrations
- `programs.claude-code.enable = true` (the fact that dev hosts get claude-code)
- Generic dev packages: `gcc`, `shellcheck`, `uv`, `zip`, `unzip`, `nmap`, `rsync`, `alejandra`, `deadnix`, `nix-tree`, `docker`, `docker-compose`

**Org overlay (extract):**
- Claude Code `settings` block — personal preferences (language "francais", bypassPermissions, enabledPlugins, voiceEnabled, allow list with `bin/rails`, `bundle`)
- Claude Code `memory.text` — personal profile, communication preferences, autonomy preferences
- Claude Code `rules` — personal git workflow, docs maintenance, workflow preferences
- Claude Code `mcpServers` — personal MCP server config
- Specific packages: `act`, `difftastic`, `aspell`, `aspellDicts.fr`, `hunspell`, `wakeonlan`, `iftop`, `redis`, `pgcli`, `iredis`, `nodePackages.*`, `nodejs`, `yarn`, `black`, `python3`, `virtualenv`

**Fix:** Framework provides `programs.claude-code.enable` + sensible security defaults. Org overlay adds settings, memory, rules, MCP servers, and org-specific dev packages.

### `modules/scopes/graphical/home.nix` — Package preferences

**Framework:** Firefox, Chrome, VSCode enables, basic graphical tools.

**Org overlay:** Brave with specific extension IDs, halloy, spotifyd, cmus, mpd, neomutt, hack-font — these are personal app choices.

### `modules/scopes/darwin/homebrew.nix` — Almost entirely org overlay

**Framework:** nix-homebrew mechanism (enable, user, taps, autoMigrate), `onActivation` settings.

**Org overlay:** The entire `brews` and `casks` lists. Every single cask (rubymine, discord, notion, slack, telegram, obsidian, etc.) is a personal choice.

### `modules/scopes/catppuccin.nix` — Flavor is preference

**Framework:** The catppuccin import and `enable = true` mechanism (themed desktop is a feature).

**Org overlay:** `flavor = "macchiato"` and `accent = "lavender"` are the org's brand colors.

**Fix:** Add `hostSpec.theme.flavor` and `hostSpec.theme.accent` to hostSpec with `mkDefault "macchiato"` / `mkDefault "lavender"`, or let orgs override via their own catppuccin config.

## 3. Org Overlay Consumption Interface

### Flake structure

```nix
# abstracts33d-fleet/flake.nix
{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    nixfleet.url = "github:nixfleet/nixfleet";
    # Optional paid modules:
    # nixfleet-platform.url = "github:nixfleet/nixfleet-platform";

    # Org-specific inputs
    secrets.url = "git+ssh://git@github.com/abstracts33d/nix-secrets.git";
    nixos-hardware.url = "github:NixOS/nixos-hardware";
  };

  outputs = { nixfleet, secrets, nixos-hardware, ... }@inputs:
    nixfleet.lib.mkFleet {
      inherit inputs;

      organizations = [
        (nixfleet.lib.mkOrg {
          name = "abstracts33d";
          description = "Personal infrastructure fleet";
          hostSpecDefaults = {
            userName = "s33d";
            githubUser = "abstracts33d";
            githubEmail = "abstract.s33d@gmail.com";
          };
          secretsPath = "${secrets}";
        })
      ];

      # Org-wide NixOS modules (applied to ALL hosts)
      nixosModules = [
        ./modules/secrets.nix       # agenix wiring (org's secret backend)
        ./modules/locale.nix        # timezone, keyboard, locale
        ./modules/hardware-extras.nix  # ledger, special hardware
      ];

      # Org-wide HM modules (applied to ALL hosts)
      hmModules = [
        ./modules/home/git-signing.nix  # GPG key
        ./modules/home/keys.nix         # public key files
        ./modules/home/claude.nix       # claude-code personal settings
        ./modules/home/dev-packages.nix # org-specific dev tools
      ];

      # Org-wide Darwin modules (applied to ALL Darwin hosts)
      darwinModules = [
        ./modules/darwin/homebrew-apps.nix  # brew/cask lists
      ];

      # Dotfiles override directory
      # Files here override framework defaults in _config/
      configDir = ./config;  # kitty.conf, starship.toml, nvim/, zsh/, etc.

      # Theme override
      theme = {
        flavor = "macchiato";
        accent = "lavender";
      };

      hosts = [
        (nixfleet.lib.mkHost {
          hostName = "krach";
          org = abstracts33d;
          platform = "x86_64-linux";
          hardwareModules = [
            ./hardware/krach/disk-config.nix
            ./hardware/krach/hardware-configuration.nix
            nixos-hardware.nixosModules.lenovo-thinkpad-x1-extreme-gen2
          ];
          hostSpecValues = {
            isImpermanent = true;
            useNiri = true;
            wifiNetworks = ["home"];
            networking.interface = "enp6s0";
          };
          extraHmModules = [
            ({ pkgs, ... }: {
              home.packages = with pkgs; [ jetbrains.ruby-mine slack ];
            })
          ];
        })
        # ... more hosts
      ];
    };
}
```

### Key design decisions for the interface

**A. Org-wide modules via `nixosModules`, `hmModules`, `darwinModules`**

`mkFleet` already accepts `extensions` (for paid platform modules). The org overlay needs a parallel mechanism for org-specific modules that are NOT extensions but apply to all hosts. Three new parameters:

```nix
mkFleet {
  nixosModules = [];     # Applied to every NixOS host
  darwinModules = [];    # Applied to every Darwin host
  hmModules = [];        # Applied to every host's HM config
  # These compose with framework deferred modules
}
```

This is where the org puts its agenix wiring, locale overrides, personal packages, and claude-code settings.

**B. Config directory override (`configDir`)**

The framework ships with `_config/` containing sensible defaults (generic gitconfig, minimal starship.toml, basic kitty.conf). The org overlay provides a `configDir` path. `mkFleet` merges them: org files override framework files, framework files fill gaps.

Implementation: `mkFleet` resolves config paths by checking `configDir` first, falling back to framework's built-in `_config/`. This is a file-level override, not a merge — if the org provides `starship.toml`, it completely replaces the framework default.

```nix
# In mkFleet internals:
resolveConfig = name:
  let orgPath = "${configDir}/${name}";
  in if builtins.pathExists orgPath then orgPath
     else ./_config/${name};  # framework default
```

**C. Theme as a first-class parameter**

Rather than requiring orgs to override catppuccin module internals:

```nix
mkFleet {
  theme = {
    flavor = "macchiato";   # default: "macchiato"
    accent = "lavender";    # default: "lavender"
  };
}
```

Framework's catppuccin scope reads from `config.nixfleet.theme.*` options.

**D. Secrets remain org-managed**

The framework NEVER imports agenix, sops, or any secret backend. It provides:
- `hostSpec.secretsPath` — a string hint
- Convention documentation (expected secret names for SSH, passwords, WiFi)
- Example org overlay showing agenix wiring

The org overlay imports `inputs.agenix.nixosModules.default` in its own `nixosModules`.

**E. SSH authorized keys**

Framework provides a `hostSpec.sshAuthorizedKeys` option (list of strings). Org sets this in `hostSpecDefaults`. Framework's core nixos module uses it for `openssh.authorizedKeys.keys`. No more `_shared/keys.nix` hardcoded file.

## 4. Migration Path

### Phase 1: Decontaminate in the monorepo (incremental, no extraction)

These changes happen in the current `nixos-config` repo, preparing for extraction without breaking anything.

**Step 1.1: Extract org-specific values into `fleet.nix`**

Move all hardcoded personal values from modules into the fleet definition or new org-specific module files:

- [ ] Move `time.timeZone = "Europe/Paris"` from `core/nixos.nix` to `fleet.nix` as an `extraModules` entry on each host, or to a new `org-config.nix` imported as `extraNixosModules` for all hosts
- [ ] Move `hardware.ledger.enable = true` to relevant hosts in `fleet.nix`
- [ ] Move `signing.key = "77C21CC574933465"` from `core/_home/git.nix` to an org HM module
- [ ] Move hardcoded nix.nixPath from `core/nixos.nix` to org config
- [ ] Move `services.xserver.xkb.layout = "us"` to `mkDefault` in framework
- [ ] Add `mkDefault` to `time.timeZone`, `i18n.defaultLocale`, `boot.kernelPackages`

**Step 1.2: Parameterize keys and secrets**

- [ ] Add `hostSpec.sshAuthorizedKeys` option with no default
- [ ] Set it in `mkOrg.hostSpecDefaults` in fleet.nix
- [ ] Remove `_shared/keys.nix` — replace usage with `hS.sshAuthorizedKeys`
- [ ] Move `_config/githubPublicKey` and `_config/githubPublicSigningKey` to org-level config
- [ ] Add `hostSpec.gpgSigningKey` option (nullable) for git signing

**Step 1.3: Split dev/home.nix**

- [ ] Extract claude-code settings, memory, rules, mcpServers into a new file (e.g., `_org/claude.nix`)
- [ ] Extract org-specific packages (redis, pgcli, rails tools, etc.) into `_org/dev-packages.nix`
- [ ] Keep generic dev tools (direnv, mise, gcc, shellcheck, docker) in framework `scopes/dev/home.nix`

**Step 1.4: Split homebrew.nix**

- [ ] Keep nix-homebrew mechanism in framework `scopes/darwin/homebrew.nix`
- [ ] Move `brews` and `casks` lists to org-specific file or fleet.nix extraModules

**Step 1.5: Split graphical/home.nix**

- [ ] Keep Firefox, Chrome, VSCode enables in framework
- [ ] Move Brave extension IDs, halloy, spotifyd, cmus, mpd, neomutt to org overlay

**Step 1.6: Add framework defaults to `_config/`**

- [ ] Create minimal `_config/` files that are truly generic good defaults (no personal preferences)
- [ ] Move personal configs to `_org/_config/` or into fleet.nix extraHmModules
- [ ] Make `core/_home/` modules resolve config paths from org config if provided

**Step 1.7: Tag the split boundary**

- [ ] Create `_org/` directory for all org-specific files (mirrors future org overlay repo)
- [ ] Add `_org/` to `.gitignore` of the future framework repo
- [ ] Verify all hosts still build (`nix run .#validate`)

### Phase 2: Extract the framework

- [ ] Create `nixfleet/` repo with Apache 2.0 license
- [ ] Move `_shared/lib/`, `core/`, `scopes/`, `tests/`, `apps.nix`, `formatter.nix`, `iso.nix`, `module-options.nix`, and generic `_config/` defaults to `nixfleet/`
- [ ] This repo becomes `abstracts33d-fleet/` — imports `nixfleet` as a flake input
- [ ] `_org/` contents become top-level in `abstracts33d-fleet/`
- [ ] `fleet.nix` stays (it IS the org overlay entry point)
- [ ] Verify build via `nixfleet.lib.mkFleet` consumption

### Phase 3: Stabilize the API

- [ ] Document every `mkFleet` parameter as stable API
- [ ] Add changelog discipline (breaking changes require major version bump)
- [ ] First external client imports `nixfleet` and validates the consumption model

## 5. The `_config/` Directory Strategy

### Current state

`_config/` contains personal dotfiles consumed by both HM modules and wrappers. Every file in it is org-specific content masquerading as shared infrastructure.

### Target state: three-tier config resolution

```
nixfleet/_config/           # Framework defaults (generic, minimal, unopinionated)
abstracts33d/config/        # Org overrides (personal preferences, branding)
host extraHmModules         # Per-host overrides (rare edge cases)
```

**Tier 1 — Framework defaults (`nixfleet/_config/`)**

Minimal, functional configs that make tools work out of the box without looking ugly:

| File | Framework default content |
|------|--------------------------|
| `gitconfig` | `defaultBranch = main`, `pull.rebase = true`, `push.autoSetupRemote = true` — no user identity |
| `starship.toml` | Basic prompt with git, nix, directory modules — no custom symbols or colors |
| `kitty.conf` | Font size, basic keybindings, no theme (catppuccin handles it) |
| `zsh/aliases.zsh` | Common aliases (`ll`, `la`, `gs`, `gd`, etc.) |
| `zsh/functions.zsh` | Utility functions (mkcd, extract, etc.) |
| `zsh/wrapperrc.zsh` | Minimal shell init for wrapper |
| `nvim/` | NOT shipped by framework — too personal. Framework enables neovim, org provides config |
| `karabiner.json` | NOT shipped — Darwin-specific, highly personal |
| `githubPublicKey` | NOT shipped — always org-specific |
| `githubPublicSigningKey` | NOT shipped — always org-specific |

**Tier 2 — Org overrides (`abstracts33d/config/`)**

The org repo provides its own versions that completely replace framework defaults:

| File | Org content |
|------|-------------|
| `starship.toml` | Custom symbols, catppuccin colors, detailed git status |
| `kitty.conf` | Font preferences, opacity, scrollback |
| `nvim/` | Full neovim config (init.lua, plugins, etc.) |
| `karabiner.json` | Keyboard remapping |
| `zsh/aliases.zsh` | Org-specific aliases on top of framework ones |

**Tier 3 — Per-host (`extraHmModules` in fleet.nix)**

For rare cases where one host needs different config (e.g., ohm uses `fr` layout):
```nix
extraModules = [{ services.xserver.xkb.layout = lib.mkForce "fr,us"; }]
```

### Resolution mechanism

The framework's HM modules need a way to discover org config. Two approaches:

**Option A: `mkFleet` parameter (recommended)**

```nix
mkFleet {
  configDir = ./config;  # org's config directory
}
```

Framework modules use a helper:
```nix
let
  configFile = name:
    if configDir != null && builtins.pathExists "${configDir}/${name}"
    then "${configDir}/${name}"
    else ./_config/${name};  # framework default
in {
  programs.kitty.extraConfig = builtins.readFile (configFile "kitty.conf");
}
```

**Option B: HM module options**

```nix
# Framework declares options
options.nixfleet.config.starship = lib.mkOption {
  type = lib.types.attrs;
  default = lib.importTOML ./_config/starship.toml;
};

# Org overrides via org-wide hmModules
config.nixfleet.config.starship = lib.importTOML ./config/starship.toml;
```

Option A is simpler and matches the current `_config/` pattern. Option B is more NixOS-idiomatic but adds option boilerplate for every config file.

**Recommendation: Option A** for dotfiles (file-level override), **Option B** for structured config (theme, locale, packages lists).

### What about wrappers?

Wrappers (`shell.nix`, `terminal.nix`) bundle configs for portable use. In the split model:

- **Framework ships wrapper mechanism** — the nix-wrapper-modules integration, package structure
- **Org provides wrapper content** — which tools to include, which configs to bundle
- Or: framework ships a generic wrapper with framework `_config/` defaults, org can override via `configDir`

The simplest approach: wrappers stay in the framework with framework defaults. Org can build custom wrappers in their own flake if needed. Most orgs will use HM (local machines) not wrappers, so this is a low-priority concern.

## Summary

### What moves to `nixfleet/` (Apache 2.0)

1. `_shared/lib/` — the API (as-is, already clean)
2. `_shared/host-spec-module.nix` — option definitions (add `sshAuthorizedKeys`, `gpgSigningKey`, `theme.*`)
3. `_shared/mk-host.nix` — internal constructors
4. `core/nixos.nix` — after removing hardcoded timezone, locale, ledger, keys, nix.nixPath; adding `mkDefault`s; removing agenix import (org provides it)
5. `core/darwin.nix` — after removing agenix import; dock mechanism stays
6. `core/home.nix` + `core/_home/*.nix` — after removing `signing.key`, hardcoded public keys
7. `scopes/` — all of them, after extracting org-specific package lists and preferences
8. `wrappers/` — mechanism + generic defaults
9. `tests/` — test infrastructure + generic assertions
10. `apps.nix`, `formatter.nix`, `iso.nix`, `module-options.nix`
11. `_config/` — stripped to generic defaults only

### What stays in `abstracts33d-fleet/` (private)

1. `fleet.nix` — org + host definitions (already the right place)
2. `hardware/` — disk-config, hardware-configuration per host
3. `config/` — personal dotfiles (kitty, starship, nvim, karabiner, zsh extras)
4. `secrets.nix` — agenix wiring with org's secret paths
5. `keys/` — public key files
6. `modules/` — org-specific modules (claude-code settings, homebrew casks, personal packages)

### The critical insight

The framework's job is to provide **mechanisms** (options, modules, constructors, scope activation). The org overlay's job is to provide **policy** (values, packages, preferences, secrets). Every file that hardcodes a value that another organization would change differently is org overlay, even if the mechanism around it is framework.

The current repo does a good job separating mechanism from policy at the `mkFleet`/`mkOrg`/`mkHost` level, but the deferred modules (`core/`, `scopes/`) still embed policy. The migration is about pushing all policy up to the org overlay while keeping mechanisms in the framework.
