# Impermanence

Why the root filesystem wipes on every boot — and why that is a good thing.

## The Concept

On impermanent hosts, the root filesystem (`/`) is ephemeral. Every boot starts fresh. Only explicitly persisted paths survive across reboots.

This means:
- **No configuration drift** — the system always matches the Nix config
- **No accumulated cruft** — temp files, caches, leftover configs vanish
- **Forced explicitness** — if something needs to persist, you declare it

## How It Works

1. The root partition uses btrfs with a subvolume that gets wiped on boot
2. A `/persist` partition holds data that must survive reboots
3. The `impermanence` module creates bind mounts from `/persist` to their expected locations
4. Programs see their data at normal paths (e.g., `~/.local/share/`) without knowing it is a bind mount

## What Persists

The framework persists essential system and user paths (see [impermanence scope](../../scopes/impermanence.md) for the full list). Fleet repos extend persistence with their own paths, declared alongside the programs that need them:

```nix
# Example: a fleet scope that adds browser persistence
home.persistence."/persist".directories =
  lib.mkIf (osConfig.hostSpec.isImpermanent or false)
  [ ".config/firefox" ];
```

## What Does Not Persist

- `/tmp`, `/var/tmp` — ephemeral by nature
- Application state not explicitly persisted — recreated or managed by fleet modules
- Downloaded files outside persisted paths

## Opting In

Impermanence is a per-host flag:

```nix
hostSpec = {
  isImpermanent = true;
};
```

Hosts without this flag use a normal persistent root.

## Further Reading

- [Technical Impermanence Details](../../scopes/impermanence.md) — paths and implementation
- [Secrets Management](secrets.md) — how secrets work with ephemeral roots
