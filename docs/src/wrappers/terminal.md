# terminal

## Purpose

Portable terminal: kitty wrapping the portable shell environment. Run from any machine with Nix: `nix run .#terminal`.

## Location

- `modules/wrappers/terminal.nix`

## Configuration

Wraps kitty with:
- Config from `_config/kitty.conf` (same source as HM)
- Default command: launches the [shell](shell.md) wrapper

Built as a `writeShellScriptBin` that execs wrapped-kitty with the shell package as the entrypoint.

## Dependencies

- Input: `wrapper-modules`
- Config source: `_config/kitty.conf`
- Depends on: [shell](shell.md) package

## Links

- [Wrappers Overview](README.md)
