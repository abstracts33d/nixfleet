# Testing Strategy (nixfleet specifics)

Extends the generic test pyramid from the claude-core plugin.

## Tier 1: Eval Tests
- Location: `modules/tests/eval.nix`
- Command: `nix flake check --no-build`
- Tests: config correctness (flags, options, scope activation)

## Tier 2: VM Tests
- Location: `modules/tests/vm.nix`
- Command: `nix run .#validate -- --vm`
- Tests: runtime (services, binaries, symlinks)
- Suites: `vm-core`, `vm-minimal`
- Only runs on x86_64-linux (nixosTest requirement)
- VM tests use `mkTestNode` helper which stubs agenix secrets and provides known test passwords

## Tier 3: Smoke Tests (future)
- Location: `modules/tests/smoke.sh`
- Runs: post build-switch

## Adding tests
When adding a new scope/feature:
1. Add eval assertions in `modules/tests/eval.nix`
2. If it has runtime behavior, add VM test in `modules/tests/vm.nix`
