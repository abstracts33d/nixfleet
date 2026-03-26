# Nix Code Style

- Format all Nix files with alejandra (via `nix fmt` / treefmt)
- Use `lib.mkIf` for conditional config, `lib.mkDefault` for overridable defaults
- Never use `with pkgs;` in module-level `let` bindings — use `with pkgs;` only in list contexts like `environment.systemPackages`
- Prefer `lib.optional` / `lib.optionals` over `if ... then [...] else []`
- Use `lib.mkForce` sparingly — only to override hardware-specific defaults
- Deferred modules use `config.flake.modules.{nixos,darwin,homeManager}.<name>`
- Scope modules self-activate with `lib.mkIf hS.<flag>`
