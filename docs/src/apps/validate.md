# validate

## Purpose

Full validation suite: formatting check, eval tests, host builds, and optional VM integration tests.

## Location

- `modules/apps.nix` (the `validate` app definition)

## Usage

```sh
nix run .#validate              # default: format + eval + builds
nix run .#validate -- --vm      # include VM integration tests
nix run .#validate -- --fast    # (reserved for future use)
```

## Validation Steps

1. **Formatting** -- `nix fmt --fail-on-change`
2. **Eval tests** (Linux only) -- 15 checks covering hostspec defaults, SSH hardening, org fields, org defaults, org across all hosts, secrets path, batch hosts, test matrix, role defaults, username inheritance, locale/timezone, SSH authorized keys, theme defaults, password files, extensions namespace
3. **NixOS host builds** -- all hosts in `nixosConfigurations` (krach, krach-qemu, qemu, ohm, lab, edge-01..03, test-workstation/server/minimal)
4. **VM integration tests** (with `--vm`) -- vm-core, vm-minimal

## Output

Color-coded pass/fail/skip for each check, with summary counts.

## Dependencies

- Pre-push hook runs this automatically
- VM tests require x86_64-linux

## Links

- [Apps Overview](README.md)
- [Eval Tests](../testing/eval-tests.md)
- [VM Tests](../testing/vm-tests.md)
