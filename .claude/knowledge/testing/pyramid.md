# Testing Pyramid

Knowledge about the 3-tier testing strategy.

## The Three Tiers

| Tier | Name | Location | Speed | Runs when |
|------|------|----------|-------|-----------|
| **C** | Eval tests | `modules/tests/eval.nix` | Instant (~10s) | `nix flake check`, pre-commit, `nix run .#validate` |
| **A** | VM tests | `modules/tests/vm.nix` | Slow (~minutes) | `nix run .#validate -- --vm`, before merge |
| **B** | Smoke tests | `modules/tests/smoke.sh` (future) | Variable | Post build-switch on real hardware |

## When to Add Which Tier

- **New hostSpec flag or scope**: Add eval assertions (Tier C)
- **New service or runtime behavior**: Add VM test (Tier A)
- **New host or hardware config**: Ensure it builds (covered by `validate`)
- **New security setting**: Add eval assertion + VM verification

## Current Eval Checks (23)

`eval-hostspec-defaults`, `eval-scope-activation`, `eval-scope-deactivation`, `eval-impermanence-paths`, `eval-ssh-hardening`, `eval-hm-programs`, `eval-dev-scope-activation`, `eval-dev-scope-deactivation`, `eval-org-field-exists`, `eval-enterprise-scope-negative`, `eval-org-defaults`, `eval-org-all-hosts`, `eval-secrets-agnostic`, `eval-batch-hosts`, `eval-test-matrix`, `eval-role-defaults`, `eval-username-org-default`, `eval-locale-timezone`, `eval-gpg-signing`, `eval-ssh-authorized`, `eval-theme-defaults`, `eval-password-files`, `eval-extensions-empty`.

## VM Test Suites

| Suite | Tests |
|-------|-------|
| `vm-core` | SSH, NetworkManager, firewall, user/groups |
| `vm-shell-hm` | HM activation, zsh, git, starship, nvim |
| `vm-graphical` | greetd, niri, kitty, pipewire, fonts |
| `vm-minimal` | Negative test: no graphical, no dev, no docker |

VM tests only run on `x86_64-linux` (nixosTest requirement).

## Git Hooks

| Hook | What | Speed |
|------|------|-------|
| `pre-commit` | `nix fmt --fail-on-change` + eval tests + Rust tests | Fast (~15s) |
| `pre-push` | format + 3 eval tests + cargo test (~15s) | Fast |

Full validation (`nix run .#validate`) runs in CI or manually — too slow for hooks.

## Rust Tests (125+)

```bash
cargo test --workspace --bins --tests --lib   # All 125+ tests
cargo test -p nixfleet-agent --bins           # Agent unit tests only
cargo test -p nixfleet-control-plane          # CP unit + integration tests
```

## VM Integration Test

`vm-nixfleet`: 2-node nixosTest (CP + agent). Proves agent↔CP cycle.
```bash
nix build .#checks.x86_64-linux.vm-nixfleet   # Needs KVM
```

## Validation Commands

```bash
nix run .#validate          # All hosts, packages, formatting, eval tests
nix run .#validate -- --vm  # Above + VM integration tests
nix flake check --no-build  # Eval tests only (instant, but evaluates Darwin too)
nix run .#test-vm -- -h <host>  # Full E2E: build ISO -> install -> verify
cargo test --workspace      # Rust tests only
```
