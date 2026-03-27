# Testing Strategy

Tests follow a 3-tier pyramid:

## Tier C -- Eval Tests (base, instant)
- Location: `modules/tests/eval.nix`
- Runs: every `nix flake check`, pre-commit
- Tests: config correctness (flags, options, scope activation)

## Tier A -- VM Tests (middle, slow) ✓ DONE
- Location: `modules/tests/vm.nix`
- Runs: before merge (`nix run .#validate -- --vm`)
- Tests: runtime (services, binaries, symlinks)
- Suites: `vm-core`, `vm-shell-hm`, `vm-graphical`, `vm-minimal`
- Only runs on x86_64-linux (nixosTest requirement)

## Tier B -- Smoke Tests (top, real hardware)
- Location: `modules/tests/smoke.sh` (future)
- Runs: post build-switch
- Tests: real-world state (SSH into live host)

## Adding tests
When adding a new scope/feature:
1. Add eval assertions in `modules/tests/eval.nix`
2. If it has runtime behavior, add VM test in `modules/tests/vm.nix`
3. VM tests use `mkTestNode` helper which stubs agenix secrets and provides known test passwords
