# Nix Gotchas

Pitfalls learned in this repo. Check before touching affected areas.

1. **perSystem pkgs don't inherit `allowUnfree`** -- unfree packages (claude-code) must go in NixOS/HM modules, not wrappers or perSystem.

2. **catppuccin/nix has no darwinModules** -- only `nixosModules` and `homeModules`. Don't import nixosModules into Darwin (class mismatch).

3. **Wrapped packages conflict with HM programs** -- don't put wrapped zsh alongside `programs.zsh.enable`.

4. **`home.persistence` doesn't exist on Darwin** -- wrap with `lib.optionalAttrs (!hS.isDarwin)`, not just `lib.mkIf` (mkIf still evaluates the option type).

5. **Don't persist `.ssh`/`.gnupg`** -- agenix creates parent dirs as root, causing HM permission errors. Keep ephemeral; only persist `known_hosts` as a file.

6. **Agenix secrets paths** -- write to ephemeral `~/.ssh/`, not `/persist/.ssh/`. Agenix re-decrypts each boot.

7. **`nix.gc` on Darwin** requires `nix.enable = true` -- incompatible with Determinate installer (`nix.enable = false`).

8. **QEMU nixpkgs hardcodes `/run/opengl-driver`** -- needs sudo shim on non-NixOS for graphical VMs.

9. **`nixos-anywhere --chown`** args with `:` are misinterpreted by SSH -- use activation scripts instead.

10. **`constructFiles` keys** in nix-wrapper-modules become bash variable names -- use underscores only (no dots, no dashes).

11. **Niri is NixOS-only, not portable** -- Wayland compositors need host GPU drivers. Use `programs.niri` from nixpkgs; deploy config via HM `xdg.configFile`.

12. **`spawn-qemu` script** -- first positional arg was consumed as disk path before arg parsing. Always use named flags (`--iso`, `--disk`).
