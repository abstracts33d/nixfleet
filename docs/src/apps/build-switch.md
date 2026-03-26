# build-switch

## Purpose

Day-to-day rebuild and switch command. Delegates to platform-specific scripts in `apps/<system>/build-switch`.

## Location

- `modules/apps.nix` (the `build-switch` app definition)
- `apps/x86_64-linux/build-switch` (Linux script)
- `apps/aarch64-darwin/build-switch` (Darwin script)

## Usage

```sh
nix run .#build-switch
```

## How it works

The app is a thin wrapper that execs the platform-specific build-switch script from the `apps/` directory.

## Links

- [Apps Overview](README.md)
