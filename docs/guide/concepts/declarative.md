# Declarative Configuration

How this config turns flags into fully configured systems.

## The Idea

Instead of running commands to install and configure software, you declare the desired state:

```nix
# "I want a NixOS machine with Niri, dev tools, and impermanent root"
hostSpecValues = {
  useNiri = true;
  isDev = true;
  isImpermanent = true;
};
```

Nix figures out the rest: packages, services, config files, display manager, theming.

## How It Works

The config uses a **deferred module pattern**:

1. **Host files** declare flags (what the machine should be)
2. **Scope modules** self-activate based on those flags (`lib.mkIf hS.isDev ...`)
3. **Core modules** provide the universal base (networking, users, security)
4. **Home Manager** configures user-level tools (shell, editor, git)

No host ever lists features manually. Add a new scope module, and every host with the matching flag gets it automatically.

## Smart Defaults

Flags propagate intelligently. Setting `useNiri = true` automatically enables:
- `isGraphical = true` (you need graphics for a compositor)
- `useGreetd = true` (you need a display manager)

These are `mkDefault` values — overridable per-host if needed.

## The Build

When you run `nix run .#build-switch`:

1. Nix evaluates all modules for your host
2. Derivations are built (or fetched from cache)
3. The new system generation is activated atomically
4. If anything fails, the previous generation remains active

## Further Reading

- [The Scope System](scopes.md) — how features are organized
- [Technical Module Details](../../src/core/README.md) — core module internals
