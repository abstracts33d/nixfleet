# NixFleet

Declarative NixOS fleet management framework. Nix modules + Rust agent/control-plane/CLI.

## Structure

```
modules/
├── _shared/lib/       # Framework API: mkFleet, mkOrg, mkRole, mkHost, mkBatchHosts, mkTestMatrix
├── _shared/           # hostSpec options, disk templates
├── core/              # Core deferred modules (nixos.nix, darwin.nix, home.nix, _home/)
├── scopes/            # Scope modules (auto-activate via mkIf on hostSpec flags)
├── wrappers/          # Portable composites (shell, terminal)
├── tests/             # Eval tests, VM tests, integration tests
├── apps.nix           # Flake apps (install, build-switch, validate, spawn-qemu, ...)
├── fleet.nix          # Framework test fleet (11 hosts)
└── flake-module.nix   # flakeModules.default for consumers
agent/                 # Rust: nixfleet-agent (state machine daemon)
control-plane/         # Rust: nixfleet-control-plane (Axum HTTP server)
cli/                   # Rust: nixfleet CLI (deploy, status, rollback)
shared/                # Rust: nixfleet-types (shared data types)
docs/
├── src/               # Technical reference (mdbook)
├── guide/             # User guide (mdbook)
└── nixfleet/          # Business docs, specs, research
.claude/               # Agents (15), skills (17), rules (8), knowledge (23), hooks (8)
```

## Commands

```sh
# Nix
nix develop                        # dev shell
nix fmt                            # format (alejandra + shfmt)
nix flake check --no-build         # eval tests (instant)
nix run .#validate                 # full validation (eval + host builds)
nix run .#validate -- --vm         # include VM tests (slow)
nix run .#install -- -h <host> -u <user>                    # macOS local
nix run .#install -- --target root@<ip> -h <host> -u <user> # NixOS remote
nix run .#build-switch             # rebuild and switch
nix run .#spawn-qemu               # QEMU VM
nix run .#test-vm -- -h krach-qemu # VM test cycle
nix build .#iso                    # custom installer ISO

# Rust
cargo test --workspace             # all Rust tests (139)
cargo build --workspace            # build all crates
cargo clippy --workspace           # lint

# Git
nix flake update secrets           # update secrets input
gh issue list -R abstracts33d/nixfleet
```

## Framework API

| Function | Purpose |
|----------|---------|
| `mkFleet` | Top-level: organizations + hosts → nixos/darwinConfigurations |
| `mkOrg` | Organization with shared defaults |
| `mkRole` | Composable role (sets hostSpec flags) |
| `mkHost` | Single host definition |
| `mkBatchHosts` | N identical hosts from a template |
| `mkTestMatrix` | Cross-product of roles × platforms for CI |

## Scope Auto-Activation

| Flag | Scope | What it enables |
|------|-------|-----------------|
| `!isMinimal` | catppuccin, base, nix-index | Theming, CLI tools, command-not-found |
| `isGraphical` | graphical/ | Pipewire, fonts, browsers, editors |
| `isDev` | dev/ | Docker, direnv, mise, Claude Code |
| `useNiri` | desktop/niri | Niri compositor + Noctalia Shell |
| `useGnome` | desktop/gnome | GNOME desktop + GDM |
| `isImpermanent` | impermanence | Ephemeral root, btrfs wipe |
| `isDarwin` | darwin/ | Homebrew, karabiner, aerospace |
| Enterprise | enterprise/ | VPN, filesharing, auth, printing, certs, proxy |

## Testing

3-tier pyramid:
- **Eval** (`modules/tests/eval.nix`) — config correctness, instant. `nix flake check --no-build`
- **VM** (`modules/tests/vm.nix`) — runtime assertions, slow. `nix run .#validate -- --vm`
- **Smoke** (future) — real hardware post-deploy

Git hooks: pre-commit (`nix fmt`, ~2s), pre-push (format + eval + cargo test, ~15s).

## Multi-Repo

| Repo | Content |
|------|---------|
| **nixfleet** (this repo) | Framework, Rust crates, tests, docs, Claude Code config |
| [fleet](https://github.com/abstracts33d/fleet) | Reference fleet (abstracts33d org config, hardware, dotfiles) |
| [fleet-secrets](https://github.com/abstracts33d/fleet-secrets) | Encrypted secrets (agenix) |
| [claude-defaults](https://github.com/abstracts33d/claude-defaults) | Standalone Claude Code plugin for other Nix projects |

## Phase Status

Tracked on the [project board](https://github.com/users/abstracts33d/projects/1).

- **S1+S2** Organizations + Roles: Done
- **S3** Fleet Agent: MVP (Rust, 139 tests)
- **S4** Control Plane: MVP (Axum, machine registry)
- **S5** Binary Cache: Planned
- **S6** Air-Gap: Planned
- **S7** NIS2: Planned
- **S8** Open-Core: Architecture decided

## Git Workflow

Branches: `feat/`, `fix/`, `refactor/`, `docs/`, `infra/` prefix. PRs required for `main`. Squash-merge only. DCO (Signed-off-by) required.

Code style: Nix → `alejandra`, Rust → `cargo fmt`, Shell → `shfmt`. All via `nix fmt`.

## Workflow Principles

- **Skill-first:** Map user requests to skills. If no skill matches, explain why and dispatch an agent directly.
- **Skills dispatch agents**, not the reverse. Users never invoke low-level agents directly.
- **Parallel by default:** 2+ independent tasks → parallel agents in a single message. Never batch sequential.
- **Verify before claiming done:** Run the build, show the output. Evidence before assertions.
- **Test before code:** Write test assertions before implementation (TDD).
- **Stop before shipping:** Present branch summary, ask "review OK, can I ship?" — never push without confirmation.

## Skill Dispatch

| User intent | Skill |
|-------------|-------|
| "add feature X" | `/feature` or `/plan-and-execute` |
| "review the code" | `/review` |
| "ship this" | `/ship` |
| "what should I do?" | `/suggest` |
| "audit the codebase" | `/audit` |
| "check health" | `/health` |
| "X is broken" | `/incident` or `/diagnose` |
| "security audit" | `/security` |
| "add scope X" | `/scope` |
| "manage secrets" | `/secrets` |
| "onboard org X" | `/onboard` |

## Critical Rules

- **Wrapper/HM boundary:** Individual tools → HM `programs.*`. Portable composites → wrappers/. Desktop → scopes/. Never wrap GPU-dependent packages.
- **Keep `_config/` in sync:** Config files are consumed by both HM modules and wrappers. See `.claude/rules/config-dependencies.md`.
- **Deferred module pattern:** Modules register via `config.flake.modules.{nixos,darwin,homeManager}.*`. Scopes self-activate with `lib.mkIf hS.<flag>`.
- **Scope-aware impermanence:** Persist paths live alongside their program definitions, not centralized.
