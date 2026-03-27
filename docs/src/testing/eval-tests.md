# Eval Tests (Tier C)

## Purpose

Assert config properties at evaluation time. No builds, no VMs — instant feedback. Each check evaluates NixOS config attributes and fails if assertions are false.

## Location

- `modules/tests/eval.nix`
- `modules/tests/_lib/helpers.nix`

## Test Suites (15 checks)

### eval-ssh-hardening
Verifies SSH security config on `krach-qemu`:
- `PermitRootLogin = "prohibit-password"`
- `PasswordAuthentication = false`
- Firewall enabled

### eval-org-field-exists
Verifies NixFleet framework options exist on all hosts:
- `hostSpec.organization` option present
- `hostSpec.role` option present
- `hostSpec.secretsPath` option present

### eval-org-defaults
Verifies organization defaults propagate:
- `krach` has an `organization` set
- `krach` has a `userName` set (from org defaults)

### eval-org-all-hosts
Verifies all test hosts belong to the same organization:
- `krach-qemu`, `qemu`, `ohm`, `lab` share the same `organization` as `krach`

### eval-secrets-agnostic
Verifies secrets path is framework-agnostic:
- `hostSpec.secretsPath` defaults to null

### eval-username-org-default
Verifies `userName` inheritance and override:
- `krach` inherits `userName` from org defaults
- `ohm` overrides to a different `userName` than the org default
- `edge-01` (batch host) inherits from org

### eval-locale-timezone
Verifies locale/timezone/keyboard settings on `krach`:
- `time.timeZone` is non-empty
- `i18n.defaultLocale` is non-empty
- `console.keyMap` is non-empty

### eval-ssh-authorized
Verifies SSH authorized keys from org defaults on `krach`:
- Primary user has at least one authorized key
- Root has at least one authorized key

### eval-password-files
Verifies password file options exist on hostSpec:
- `hostSpec.hashedPasswordFile` option present
- `hostSpec.rootHashedPasswordFile` option present

### eval-extensions-empty
Verifies extensions namespace is clean:
- `nixfleet.extensions` is an empty attrset by default (on `krach-qemu`)

### eval-batch-hosts
*(Runs only when `edge-01` is present)*

Verifies batch hosts generated via `mkBatchHosts`:
- `edge-01` belongs to the same org as `krach`
- `edge-01` has `isServer = true` (from edge role)
- `edge-01` has `isMinimal = true` (from edge role)
- `edge-01` inherits `userName` from org

### eval-test-matrix
*(Runs only when `test-workstation-x86_64` is present)*

Verifies `mkTestMatrix` hosts:
- `test-workstation-x86_64` belongs to the same org as `krach`
- `test-server-x86_64` has `isServer = true` (from server role)
- `test-minimal-x86_64` has `isMinimal = true` (from minimal role)

### eval-role-defaults
*(Runs only when `test-workstation-x86_64` is present)*

Verifies role flag propagation:
- `test-server-x86_64` has `isServer = true`
- `test-minimal-x86_64` has `isMinimal = true`

### eval-hostspec-defaults
*(Listed in validate, verifies framework hostSpec option defaults)*

### eval-theme-defaults
*(Listed in validate — only available when fleet extends hostSpec with theme options)*

## Platform

x86_64-linux only (all test hosts are x86_64-linux configs).

## Running

```sh
nix flake check --no-build         # all eval checks, instant
nix build .#checks.x86_64-linux.eval-ssh-hardening --no-link  # one check
```

## Links

- [Testing Overview](README.md)
- [VM Tests](vm-tests.md)
