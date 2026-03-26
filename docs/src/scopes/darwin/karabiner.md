# karabiner

## Purpose

Karabiner-Elements key remapping configuration. Deploys the shared JSON config to `~/.config/karabiner/karabiner.json` via HM.

## Location

- `modules/scopes/darwin/karabiner.nix`
- `modules/_config/karabiner.json` (shared config)

## Configuration

**Platform:** Darwin only (registered as `flake.modules.homeManager.karabiner`)

No gate flag -- always active on all hosts with HM (the karabiner cask is installed via [homebrew](homebrew.md)).

Reads config from `_config/karabiner.json` and writes it to the home directory.

## Dependencies

- Config source: `modules/_config/karabiner.json`
- Requires: karabiner-elements cask (installed by [homebrew scope](homebrew.md))

## Links

- [Scope Overview](../README.md)
- [Homebrew](homebrew.md)
