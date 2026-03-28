# build-switch

## Purpose

Day-to-day rebuild and switch. Detects the current hostname and rebuilds the matching NixOS or Darwin configuration from the flake.

## Location

- `modules/apps.nix` (the `build-switch` app definition)

## Usage

```sh
nix run .#build-switch
```

## How it works

**On Linux (NixOS):** Runs `nixos-rebuild switch --flake .#<hostname>` via sudo, preserving the SSH agent socket for any remote operations.

**On Darwin:** Builds `darwinConfigurations.<hostname>.system` then runs `darwin-rebuild switch --flake .#<hostname>`.

The hostname is determined automatically from the running machine.

## Links

- [Apps Overview](README.md)
