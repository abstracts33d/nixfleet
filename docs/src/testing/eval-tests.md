# Eval Tests (Tier C)

## Purpose

Assert config properties at evaluation time. No builds, no VMs -- instant feedback. Each check evaluates NixOS or HM config attributes and fails if assertions are false.

## Location

- `modules/tests/eval.nix`
- `modules/tests/_lib/helpers.nix`

## Test Suites (23 checks)

### eval-hostspec-defaults
Verifies smart default propagation:
- `useNiri` implies `isGraphical` and `useGreetd` (tested on krach-qemu)
- `useGnome` implies `isGraphical` and `useGdm` (tested on ohm)
- `isMinimal` implies `!isGraphical` and `!isDev` (tested on qemu)

### eval-scope-activation
Verifies scopes activate correctly on krach-qemu:
- `programs.niri.enable` is true
- `services.greetd.enable` is true
- `security.polkit.enable` is true
- `networking.firewall.enable` is true
- `networking.networkmanager.enable` is true

### eval-scope-deactivation
Verifies scopes deactivate on minimal qemu:
- `isGraphical` is false
- `isDev` is false
- Niri not enabled
- greetd not enabled

### eval-impermanence-paths
Verifies persist directories on krach-qemu:
- `/etc/nixos`, `/etc/NetworkManager/system-connections`, `/var/lib/nixos`, `/var/log` present
- `/persist` marked `neededForBoot`

### eval-ssh-hardening
Verifies SSH security on krach-qemu:
- `PermitRootLogin = "prohibit-password"`
- `PasswordAuthentication = false`
- Firewall enabled

### eval-hm-programs
Verifies core HM programs on krach-qemu:
- zsh, git, starship, ssh all enabled

### eval-dev-scope-activation
Verifies dev scope on krach (isDev = true by default):
- `isDev` is true
- Docker enabled

### eval-dev-scope-deactivation
Verifies dev scope off on qemu (isMinimal implies !isDev):
- `isDev` is false
- Docker not enabled

### eval-org-field-exists
Verifies NixFleet framework options exist on all hosts:
- `hostSpec.organization` option present
- `hostSpec.role` option present
- `hostSpec.secretsPath` option present

### eval-enterprise-scope-negative
Verifies enterprise scopes inactive on krach-qemu (no enterprise flags set):
- `useVpn` is false
- `useFilesharing` is false
- `useLdap` is false
- `wireguard-tools` not in systemPackages

### eval-org-defaults
Verifies organization defaults propagate to hosts:
- krach inherits `githubUser` from org
- krach inherits `githubEmail` from org
- krach has `organization = "abstracts33d"`

### eval-org-all-hosts
Verifies organization set on all hosts:
- krach-qemu, qemu, ohm, lab all have `organization = "abstracts33d"`

### eval-secrets-agnostic
Verifies secrets path is framework-agnostic:
- `secretsPath` defaults to null

### eval-batch-hosts
Verifies batch hosts (edge fleet via `mkBatchHosts`):
- edge-01 belongs to abstracts33d org
- edge-01 has `isServer` from edge role
- edge-01 has `isMinimal` from edge role
- edge-01 inherits `userName` from org

### eval-test-matrix
Verifies test matrix hosts (via `mkTestMatrix`):
- `test-workstation-x86_64` belongs to abstracts33d org
- `test-server-x86_64` has `isServer` from server role
- `test-minimal-x86_64` has `isMinimal` from minimal role

### eval-role-defaults
Verifies role default propagation:
- workstation role sets `isDev = true`, `isGraphical = true`
- server role sets `isServer = true`, `isDev = false`

### eval-username-org-default
Verifies userName inheritance and override:
- krach inherits `userName = "s33d"` from org
- ohm overrides to `userName = "sabrina"`
- edge-01 batch host inherits from org

### eval-locale-timezone
Verifies locale/timezone/keyboard settings on krach:
- `time.timeZone` matches org default (`Europe/Paris`)
- `i18n.defaultLocale` matches org default (`en_US.UTF-8`)
- `console.keyMap` matches org default (`us`)

### eval-gpg-signing
Verifies GPG signing config propagated from org defaults to krach:
- `programs.git.signing.key` is the org GPG fingerprint
- `programs.git.signing.signByDefault` is true

### eval-ssh-authorized
Verifies SSH authorized keys from org defaults on krach:
- Primary user has at least one authorized key
- Root has at least one authorized key

### eval-theme-defaults
Verifies theme hostSpec defaults on krach-qemu:
- `hostSpec.theme.flavor` is `"macchiato"` (default)
- `hostSpec.theme.accent` is `"lavender"` (default)

### eval-password-files
Verifies password file paths set from org defaults on krach:
- User `hashedPasswordFile` points to `/run/agenix/user-password`
- Root `hashedPasswordFile` points to `/run/agenix/root-password`
- `hostSpec.hashedPasswordFile` and `rootHashedPasswordFile` match

### eval-extensions-empty
Verifies extensions namespace:
- `nixfleet.extensions` is empty attrset by default

## Platform

x86_64-linux only (all test hosts are x86_64-linux configs).

## Links

- [Testing Overview](README.md)
- [VM Tests](vm-tests.md)
