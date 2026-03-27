# Testing Patterns

Knowledge about test implementation: mkTestNode, eval assertions, VM test patterns.

## mkTestNode Helper

Test nodes use `mkTestNode` (in `modules/tests/_lib/`) which:
- Stubs agenix secrets (no real decryption key needed)
- Provides known test passwords
- Creates minimal NixOS configurations suitable for `nixosTest`
- Allows hostSpec flag overrides per test node

## Writing Eval Tests

Eval tests assert config properties without building. Pattern:

```nix
# In modules/tests/eval.nix
eval-my-feature = {
  expr =
    let
      cfg = nixosConfigurations.testHost.config;
    in
      cfg.services.myService.enable;
  expected = true;
};
```

### What to Assert

- **Flag propagation**: `useNiri = true` implies `isGraphical = true`
- **Scope activation**: `isDev = true` results in `programs.direnv.enable = true`
- **Scope deactivation**: `isMinimal = true` results in no graphical packages
- **Security**: SSH hardening settings are correct
- **Impermanence**: persist paths exist for stateful programs
- **Framework**: org/role defaults apply correctly

## Writing VM Tests

VM tests use NixOS test driver. Pattern:

```nix
# In modules/tests/vm.nix
vm-my-feature = nixosTest {
  name = "my-feature";
  nodes.machine = mkTestNode { hostSpecValues = { ... }; };
  testScript = ''
    machine.wait_for_unit("multi-user.target")
    machine.succeed("systemctl is-active my-service")
  '';
};
```

### Common VM Test Patterns

- `wait_for_unit("multi-user.target")` -- wait for boot
- `succeed("which <binary>")` -- verify binary is installed
- `succeed("systemctl is-active <service>")` -- verify service running
- `fail("which <binary>")` -- negative test (binary should NOT exist)
- `succeed("test -L <symlink>")` -- verify symlink exists

## Common Failure Patterns

| Symptom | Likely cause |
|---------|-------------|
| `attribute 'X' missing` in eval | Wrong config path or scope not activated |
| `infinite recursion` | Circular `mkIf` / `mkDefault` chain |
| `option 'X' does not exist` | Missing module import or Darwin-only option |
| VM test timeout | Service failed to start; check journal |
| Host build fails but eval passes | Runtime dependency issue, not config |

## mkTestMatrix

Generates one VM host per role x platform combination for CI validation:

```nix
mkTestMatrix {
  org = testOrg;
  roles = with builtinRoles; [ workstation server minimal ];
  platforms = [ "x86_64-linux" ];
}
# Generates: test-workstation-x86_64, test-server-x86_64, test-minimal-x86_64
```
