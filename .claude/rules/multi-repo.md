# Multi-Repository Dependencies

NixFleet is the framework hub. Related repos are referenced via flake inputs or adjacent checkouts.

## fleet (reference implementation)
- **Location:** `../nixos-config` or `../fleet` (adjacent checkout)
- **Contains:** Org fleet config (fleet.nix, _hardware/, _config/, demo/)
- **Relationship:** Consumes `inputs.nixfleet.flakeModules.default`
- **Claude config:** Minimal overlay only -- full .claude/ lives here in nixfleet

## fleet-secrets (private)
- **Input:** `inputs.secrets` (git+ssh://git@github.com/abstracts33d/fleet-secrets.git)
- **Contains:** Encrypted `.age` files (SSH keys, passwords, WiFi connections)
- **Workflow:** Edit in fleet-secrets -> commit -> push -> `nix flake update secrets` in fleet repo

## When modifying secrets
1. Never output decrypted content
2. Always commit in fleet-secrets first, then update the flake input in fleet
3. Use `/secrets` skill for guided operations
4. After `nix flake update secrets`, verify build in fleet repo

## Framework vs Overlay Separation

The framework provides **mechanisms** (options, modules, constructors, scope activation). The org overlay provides **policy** (values, packages, preferences, secrets). Every file that hardcodes a value another org would change is org overlay.

- **Pure framework**: `_shared/lib/`, `_shared/host-spec-module.nix`, core modules (parameterized), scope modules (generic), tests, apps
- **Mixed (need decontamination)**: `core/nixos.nix` (hardcoded timezone/locale), `core/darwin.nix` (agenix imports), `core/_home/git.nix` (GPG key)
- **Pure org overlay**: `fleet.nix`, `_hardware/*`, `_config/`, personal keys

## Distribution Mechanism

Exported as flake-parts `flakeModule` via `importApply`:
- Client imports: `imports = [inputs.nixfleet.flakeModules.default]`
- Framework inputs captured by `importApply` closure
- Deferred modules merge via `config.flake.modules.*`
- Config resolution: framework defaults (mkDefault) < org overrides < host values
