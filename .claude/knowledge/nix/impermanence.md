# Impermanence

Knowledge about the ephemeral root filesystem and persistence patterns.

## How It Works

Impermanent NixOS hosts use btrfs with a subvolume that gets wiped on every boot. Only explicitly persisted paths survive. The `impermanence.nix` scope handles the btrfs wipe script (boot.initrd.postMountCommands) and universal persist paths.

## Scope-Aware Persistence

Persist paths live **alongside their program definitions**, not in a central file. When adding a new program to a scope, add its persist paths in the same scope module.

Use the **HM persistence module** (`home.persistence."/persist"`), not the NixOS one (`environment.persistence."/persist".users`). The HM module creates directories with correct user ownership.

## Ephemeral vs Persisted Directories

| Directory | Status | Reason |
|-----------|--------|--------|
| `.ssh` | **Ephemeral** | Managed by agenix (keys) and HM (config) each boot |
| `.gnupg` | **Ephemeral** | Managed by agenix/HM each boot |
| `.keys` | **Persisted** | Contains agenix decryption key provisioned during install |
| `known_hosts` | **Persisted (file)** | SSH host key memory survives reboots |
| All other user dirs | **Persisted** | Via bind mounts in `home.persistence` |

## Critical Gotchas

1. **Never persist `.ssh` or `.gnupg`** -- agenix creates parent dirs as root, causing HM permission errors. Keep ephemeral; only persist `known_hosts` as a file.

2. **Darwin has no `home.persistence`** -- wrap with `lib.optionalAttrs (!hS.isDarwin)`, not just `lib.mkIf`. The `mkIf` still evaluates the option type, which does not exist on Darwin, causing an eval error.

3. **Agenix secrets paths** -- write to ephemeral `~/.ssh/`, not `/persist/.ssh/`. Agenix re-decrypts each boot.

## Adding Persistence for a New Program

```nix
# In the same module where you enable the program:
home.persistence."/persist" = lib.optionalAttrs (!hS.isDarwin) {
  directories = [
    ".local/share/myprogram"
  ];
  files = [
    ".config/myprogram/settings.json"
  ];
};
```
