# Framework vs Overlay Separation

Distilled knowledge about the NixFleet extraction path.

## The Core Insight

The framework provides **mechanisms** (options, modules, constructors, scope activation). The org overlay provides **policy** (values, packages, preferences, secrets). Every file that hardcodes a value that another organization would change is org overlay.

## Classification Summary

### Pure Framework (extractable as-is)

- `_shared/lib/` -- the API (mkFleet, mkOrg, mkRole, mkHost, mkBatchHosts, mkTestMatrix)
- `_shared/host-spec-module.nix` -- option definitions
- `_shared/mk-host.nix` -- internal NixOS/Darwin/VM constructors
- `core/home.nix` -- imports `_home/` files
- `core/_home/ssh.nix`, `starship.nix`, `neovim.nix`, `simple.nix` -- parameterized
- All `scopes/enterprise/*` -- generic stubs
- `scopes/desktop/*`, `display/*`, `hardware/*` -- generic integrations
- `module-options.nix`, `formatter.nix`, `iso.nix`, `apps.nix`
- `tests/` -- test infrastructure + generic assertions

### Mixed (need decontamination)

- `core/nixos.nix` -- framework mechanisms + hardcoded timezone, locale, nix.nixPath, ledger, keys
- `core/darwin.nix` -- framework mechanisms + agenix imports, startup chime
- `core/_home/git.nix` -- GPG signing key hardcoded
- `core/_home/zsh.nix` -- BROWSER, WORKSPACE, plugin selection
- `scopes/catppuccin.nix` -- mechanism is framework, flavor/accent are preference (now parameterized via hostSpec.theme)
- `scopes/graphical/home.nix` -- Firefox/Chrome = framework, Brave/halloy/spotifyd = preference
- `scopes/darwin/homebrew.nix` -- mechanism = framework, brews/casks lists = preference
- `scopes/dev/home.nix` -- ~80% org-specific (claude-code settings, personal packages)

### Pure Org Overlay

- `fleet.nix` -- org + host definitions
- `_hardware/*` -- per-host disk/hardware configs
- `_config/` -- personal dotfiles (kitty, starship, nvim, karabiner)
- `_config/githubPublicKey`, `githubPublicSigningKey`

## Config Resolution Mechanism

Three-tier config resolution for the split:

1. **Framework defaults** (`nixfleet/_config/`): minimal, functional, unopinionated
2. **Org overrides** (`<org>/config/`): org preferences, completely replace framework files
3. **Per-host** (`extraHmModules` in fleet.nix): rare edge cases

Resolution: `mkFleet { configDir = ./config; }` -- framework checks `configDir` first, falls back to built-in `_config/`.

## Distribution Mechanism

The framework will be exported as a flake-parts `flakeModule` via `importApply`:

- Client imports with one line: `imports = [inputs.nixfleet.flakeModules.default]`
- Framework inputs captured by `importApply` closure
- Deferred modules merge naturally via `config.flake.modules.*`
- No architecture change -- the split is a refactor, not a rewrite

## Migration Path

1. **Phase 0 (decontamination)**: Move all org-specific values from modules to fleet.nix/org modules. Add `mkDefault` everywhere. Done: S1+S2 implemented.
2. **Phase 1 (restructure)**: Create `flake-module.nix`, replace import-tree with explicit imports
3. **Phase 2 (extract)**: Create `nixfleet/` repo, current repo becomes the reference fleet
4. **Phase 3 (stabilize API)**: Document, version, first external client
