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
- Suites: `vm-core`, `vm-shell-hm`, `vm-graphical`, `vm-minimal`, `vm-nixfleet`
- Only runs on x86_64-linux (nixosTest requirement)

## Tier 3: Smoke Tests (future)
- Location: `modules/tests/smoke.sh`
- Runs: post build-switch

## mkTestNode Helper

Test nodes use `mkTestNode` (in `modules/tests/_lib/`) which:
- Stubs agenix secrets (no real decryption key needed)
- Provides known test passwords
- Creates minimal NixOS configs suitable for `nixosTest`
- Allows hostSpec flag overrides per test node

## Writing Eval Tests

```nix
eval-my-feature = {
  expr = let cfg = nixosConfigurations.testHost.config; in cfg.services.myService.enable;
  expected = true;
};
```

What to assert: flag propagation, scope activation/deactivation, security settings, impermanence persist paths, org/role defaults.

## Writing VM Tests

```nix
vm-my-feature = nixosTest {
  name = "my-feature";
  nodes.machine = mkTestNode { hostSpecValues = { ... }; };
  testScript = ''
    machine.wait_for_unit("multi-user.target")
    machine.succeed("systemctl is-active my-service")
  '';
};
```

Common patterns: `wait_for_unit`, `succeed("which <binary>")`, `fail("which <binary>")`, `succeed("test -L <symlink>")`.

## Common Failure Patterns

| Symptom | Likely cause |
|---------|-------------|
| `attribute 'X' missing` in eval | Wrong config path or scope not activated |
| `infinite recursion` | Circular `mkIf` / `mkDefault` chain |
| `option 'X' does not exist` | Missing module import or Darwin-only option |
| VM test timeout | Service failed to start; check journal |

## Validation Commands

```bash
nix flake check --no-build          # Eval tests only (instant)
nix run .#validate                  # All hosts + packages + format + eval
nix run .#validate -- --vm          # Above + VM integration tests
nix run .#test-vm -- -h <host>      # Full E2E: build ISO -> install -> verify
cargo test --workspace              # Rust tests only (125+)
```

## Adding tests
When adding a new scope/feature:
1. Add eval assertions in `modules/tests/eval.nix`
2. If it has runtime behavior, add VM test in `modules/tests/vm.nix`
