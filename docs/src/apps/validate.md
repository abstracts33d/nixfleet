# validate

## Purpose

Full validation suite: formatting check, eval tests, host builds, cross-platform eval, package builds, and optional VM integration tests.

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
2. **Eval tests** (Linux only) -- 18 checks covering hostspec defaults, scope activation/deactivation, impermanence, SSH hardening, HM programs, dev scope, org defaults, enterprise scope negative, batch hosts, test matrix, role defaults, username inheritance, extensions namespace
3. **NixOS host builds** -- krach, krach-qemu, qemu, ohm, lab, edge-01..03, test-workstation/server/minimal
4. **Cross-platform eval** -- utm, krach-utm (eval only, can't build aarch64 on x86_64), aether (Darwin)
5. **Package builds** -- shell, terminal
6. **VM integration tests** (with `--vm`) -- vm-core, vm-shell-hm, vm-graphical, vm-minimal

## Output

Color-coded pass/fail/skip for each check, with summary counts.

## Dependencies

- Pre-push hook runs this automatically
- VM tests require x86_64-linux

## Links

- [Apps Overview](README.md)
- [Eval Tests](../testing/eval-tests.md)
- [VM Tests](../testing/vm-tests.md)
