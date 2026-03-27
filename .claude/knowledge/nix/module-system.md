# Nix Module System

Knowledge about the NixOS module system as used in this repository.

## Deferred Module Pattern

Modules under `modules/core/` and `modules/scopes/` define deferred modules via `config.flake.modules.{nixos,darwin,homeManager}.*`. These are auto-included by `mkNixosHost`/`mkDarwinHost` via `builtins.attrValues` -- hosts never list features manually.

Scope modules self-activate using `mkIf` based on `hostSpec` flags. Adding a new scope file automatically applies to all hosts with the matching flag.

```nix
# Pattern: deferred module definition
{ config, inputs, lib, ... }: {
  config.flake.modules.nixos.myScope = { config, pkgs, lib, ... }: {
    # This becomes a NixOS module applied to all hosts
  };
}
```

## hostSpec Smart Defaults

Compositor flags auto-propagate related settings via `lib.mkDefault` in `host-spec-module.nix`:

- `useNiri` -> `isGraphical = true`, `useGreetd = true`
- `useHyprland` -> `isGraphical = true`, `useGreetd = true`
- `useGnome` -> `isGraphical = true`, `useGdm = true`
- `isMinimal` -> `isGraphical = false`, `isDev = false`

Hosts only declare the compositor; display manager and graphical flag follow automatically. Overridable per-host.

## hostSpec Options Reference

Defined in `modules/_shared/host-spec-module.nix` (39 options total):

### Data fields
| Option | Type | Description |
|--------|------|-------------|
| `userName` | `str` | Primary user |
| `hostName` | `str` | Hostname |
| `networking` | `attrsOf anything` | Network config (interface, etc.) |
| `githubUser` | `str` | GitHub handle |
| `githubEmail` | `str` | GitHub email |
| `home` | `str` | Home directory (auto: `/home/X` or `/Users/X`) |

### Configuration flags
| Flag | Default | Controls |
|------|---------|----------|
| `isMinimal` | `false` | Disables non-essential packages |
| `isServer` | `false` | Server mode |
| `isDarwin` | `false` | macOS host (set automatically) |
| `isImpermanent` | `false` | Ephemeral root with persist |
| `isDev` | `true` | Dev tools (direnv, mise, nodejs, python, etc.) |
| `isGraphical` | `true` | Graphical desktop (pipewire, fonts, browsers, etc.) |
| `useGnome` | `false` | GNOME desktop + GDM |
| `useHyprland` | `false` | Hyprland compositor |
| `useNiri` | `false` | Niri compositor + Noctalia Shell |
| `useGdm` | `false` | Standalone GDM |
| `useGreetd` | `false` | Greetd display manager |
| `useAerospace` | `false` | AeroSpace WM (Darwin) |
| `wifiNetworks` | `[]` | WiFi connections to bootstrap |
| `hasBluetooth` | `false` | Bluetooth support |
| `useSecureBoot` | `false` | Lanzaboote Secure Boot |
| `useVpn` | `false` | Corporate VPN (WireGuard/OpenVPN) |
| `useFilesharing` | `false` | Samba/CIFS file sharing |
| `useLdap` | `false` | LDAP/AD authentication |
| `usePrinting` | `false` | Network printing (CUPS) |
| `useCorporateCerts` | `false` | Corporate CA trust |
| `useProxy` | `false` | System-wide HTTP/HTTPS proxy |
| `organization` | `"default"` | Organization name (set by mkFleet) |
| `role` | `null` | Named role within organization |
| `secretsPath` | `null` | Secrets repo path hint |
| `gpgSigningKey` | `null` | GPG key fingerprint for signing |
| `sshAuthorizedKeys` | `[]` | SSH public keys |
| `timeZone` | `"UTC"` | IANA timezone |
| `locale` | `"en_US.UTF-8"` | System locale |
| `keyboardLayout` | `"us"` | XKB keyboard layout |
| `hashedPasswordFile` | `null` | Hashed password path |
| `rootHashedPasswordFile` | `null` | Root hashed password path |
| `theme.flavor` | `"macchiato"` | Catppuccin flavor |
| `theme.accent` | `"lavender"` | Catppuccin accent color |

## Built-in Roles

`mkFleet` ships six built-in roles in `modules/_shared/lib/roles.nix`:

| Role | Flags set |
|------|-----------|
| `workstation` | isDev, isGraphical, isImpermanent, useNiri |
| `server` | isServer, !isDev, !isGraphical |
| `minimal` | isMinimal |
| `vm-test` | !isDev, isGraphical, isImpermanent, useNiri |
| `edge` | isServer, isMinimal |
| `darwin-workstation` | isDarwin, isDev, isGraphical |

## Priority Order

Defaults compose via `lib.mkDefault` (priority 1000). Explicit host values always win.

```
org hostSpecDefaults (mkDefault)
  -> overridden by role hostSpecDefaults (mkDefault, later in mkMerge)
  -> overridden by hostSpec smart defaults (mkDefault)
  -> overridden by host hostSpecValues (no mkDefault -- highest priority)
```

## Config Dependency Chains (knowledge portion)

When modifying files, these pairs must stay in sync:

- `_shared/host-spec-module.nix` (new flag) -> CLAUDE.md flags table + README.md
- `_shared/host-spec-module.nix` (smart defaults) -> verify all hosts still build
- `_shared/lib/roles.nix` (new role) -> CLAUDE.md roles table + docs/src/
- `modules/fleet.nix` <-> `modules/_shared/lib/*.nix` (fleet.nix consumes the lib API)

## Claude Code Permissions (NixOS-managed)

Claude Code settings are managed declaratively by NixOS modules, not manually edited:

| Level | NixOS Module | Generated File | Content |
|-------|-------------|----------------|---------|
| **Org** | `core/nixos.nix` (`environment.etc`) | `/etc/claude-code/settings.json` | Non-overridable deny list (security floor) |
| **Project** | Git-tracked | `.claude/settings.json` | Allow list for repo tools + hooks |
| **User** | `scopes/dev/home.nix` (`programs.claude-code`) | `~/.claude/settings.json` | `bypassPermissions`, personal allows |

The org deny list blocks: destructive ops (`rm -rf`, `dd`, `mkfs`), privilege escalation (`sudo`, `pkexec`), dangerous git (`force push`, `hard reset`), nix store manipulation. This cannot be bypassed even with `bypassPermissions`.

User/org CLAUDE.md and rules are also Nix-managed — manual edits are overwritten on `nix run .#build-switch`.

## The `_` Prefix Exclusion Convention

Directories and files prefixed with `_` are excluded from import-tree. They are pulled in via explicit `imports` or relative paths:

- `_shared/` -- framework API, host helpers, hostSpec options, disk templates
- `_config/` -- config files shared between HM and wrappers
- `_hardware/` -- per-host hardware configs
- `core/_home/` -- HM tool config fragments (imported by `core/home.nix`)
