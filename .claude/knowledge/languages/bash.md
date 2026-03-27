# Bash Patterns (Scripts & Apps)

Knowledge about Bash conventions used in `apps.nix`, `.githooks/`, and `scripts/`.

## Script Standards

All scripts use:
```bash
#!/usr/bin/env bash
set -euo pipefail
```

- `set -e`: Exit on error
- `set -u`: Error on undefined variables
- `set -o pipefail`: Pipe failures propagate

## Argument Parsing Pattern

Used in install, build-switch, spawn-qemu, validate:

```bash
while [[ $# -gt 0 ]]; do
  case "$1" in
    --fast) FAST=1; shift ;;
    --vm) VM=1; shift ;;
    -h|--hostname) HOSTNAME="$2"; shift 2 ;;
    *) echo "Unknown option: $1"; exit 1 ;;
  esac
done
```

**Gotcha**: positional args before the `case` loop caused bugs in spawn-qemu. Always use named flags.

## mkScript Helper

`apps.nix` defines apps using:
```nix
mkScript = name: text: {
  type = "app";
  program = "${pkgs.writeShellScriptBin name text}/bin/${name}";
};
```

Inside Nix string interpolation, bash variables need double-escaping: `''${VAR}` not `${VAR}`.

## Key Scripts

### validate (`apps.nix`)
- Builds all hosts, packages, and runs eval tests
- Color-coded pass/fail output
- `--fast` flag skips host builds
- `--vm` flag includes VM integration tests

### install (`apps.nix`)
- Detects Darwin vs NixOS
- Darwin: local `darwin-rebuild switch`
- NixOS: remote via `nixos-anywhere` with `--extra-files` for key provisioning
- `StrictHostKeyChecking=accept-new` (TOFU for initial provisioning)

### spawn-qemu (`apps.nix`)
- QEMU with virtio, SPICE, virgl
- Named flags: `--iso`, `--disk`, `--console`
- Port forwards: 2222->22 (SSH), 5900 (SPICE)

### Git Hooks

**pre-commit** (`.githooks/pre-commit`):
1. `nix fmt -- --fail-on-change`
2. Eval tests via `nix flake show` + `nix build .#checks...`
3. Rust agent tests via `cargo test`

**pre-push** (`.githooks/pre-push`):
1. `nix run .#validate` (full validation)

## GitHub Issue Helper (`scripts/gh-issue-helper.sh`)

Shared functions for issue management:
- `gh_create_issue` -- create with labels, add to project board
- `gh_move_issue` -- transition board column (GraphQL)
- `gh_transition_issue` -- event-based transitions (created->Backlog, started->In Progress, etc.)
- `gh_close_issue` -- close + move to Done
- Caches project metadata (2 GraphQL calls on first use, 0 after)

## Error Handling in Nix Shell Scripts

Inside Nix `writeShellScriptBin`, special quoting rules apply:
- `''${var}` for bash variables (double single-quotes escape nix interpolation)
- `${pkgs.foo}` for nix store paths
- Heredocs work normally but need careful quoting
