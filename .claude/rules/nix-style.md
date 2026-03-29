# Nix Code Style (nixfleet overrides)

These extend the generic Nix style rules from the claude-core plugin.

- Deferred modules use `config.flake.modules.{nixos,darwin,homeManager}.<name>`
- Scope modules self-activate with `lib.mkIf hS.<flag>`
