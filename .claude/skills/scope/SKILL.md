---
name: scope
description: Scaffold a new NixOS scope with tests and docs baked in. Use when adding a new feature scope to the config.
user-invocable: true
---

# Scaffold New Scope

## Process

1. **Ask** the user:
   - Scope name (e.g., `bluetooth`, `gaming`, `media`)
   - hostSpec flag name (e.g., `hasBluetooth`, `useGaming`)
   - What it enables (NixOS packages, services, HM programs)
   - Does it need HM config? Persistence paths?

1.5. Invoke `superpowers:test-driven-development` — write eval test assertions BEFORE generating the module code

2. **Generate files**:

   **NixOS module** (`modules/scopes/<name>/nixos.nix`):
   ```nix
   {...}: {
     flake.modules.nixos.<name> = { config, pkgs, lib, ... }: let
       hS = config.hostSpec;
     in {
       config = lib.mkIf hS.<flag> {
         # NixOS config here
       };
     };
   }
   ```

   **HM module** (if needed) (`modules/scopes/<name>/home.nix`):
   ```nix
   {...}: {
     flake.modules.homeManager.<name> = { config, pkgs, lib, ... }: let
       hS = config.hostSpec;
     in {
       config = lib.mkIf hS.<flag> {
         # HM config here
       };
     };
   }
   ```

   **hostSpec flag** (if new) in `modules/_shared/host-spec-module.nix`:
   ```nix
   <flag> = lib.mkOption {
     type = lib.types.bool;
     default = false;
     description = "...";
   };
   ```

   **Eval test** assertion in `modules/tests/eval.nix`:
   - Scope activation: flag=true → expected options enabled
   - Scope deactivation: flag=false → options not enabled

3. **Dispatch doc-writer** agent:
   - Update CLAUDE.md (module tree, flags table)
   - Update README.md (scopes table)
   - Create `docs/src/scopes/<name>.md` for the new scope
   - Add entry to `docs/src/SUMMARY.md`
   - Update `docs/guide/concepts/scopes.md` with new scope description

4. **Dispatch test-runner** → `nix run .#validate` to verify eval tests pass

4.5. **Create tracking issue**:

5. **Present** all generated files for review
