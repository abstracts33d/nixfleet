# Rollback App

Roll back the system to the previous generation.

**Command:** `nix run .#rollback`

**Platform:** macOS only (nix-darwin)

## Usage

```sh
nix run .#rollback
```

Switches to the previous nix-darwin generation. On NixOS, use `nixos-rebuild switch --rollback` or the NixOS boot menu to select a previous generation.

## Implementation

Defined in `modules/apps.nix`. Wraps `darwin-rebuild` with the `--rollback` flag.
