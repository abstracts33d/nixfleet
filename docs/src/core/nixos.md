# core/nixos.nix

## Purpose

Universal NixOS configuration applied to every NixOS host. Covers boot, networking, user management, security, secrets (agenix), WiFi bootstrap, SSH hardening, and the org-level Claude Code deny list.

## Location

- `modules/core/nixos.nix`

## Configuration

### Nix settings
- `allowUnfree = true`, `allowBroken = false`
- Binary caches: nixos cache + nix-community cachix
- `auto-optimise-store = true` (hardlink deduplication)
- Weekly gc (`--delete-older-than 7d`)
- `trusted-users` includes regular user (gated: not on servers)

### Boot
- systemd-boot with 42 configuration limit
- Latest kernel (`linuxPackages_latest`)
- initrd modules: xhci_pci, ahci, nvme, usbhid, usb_storage, sd_mod
- uinput kernel module

### Networking
- NetworkManager enabled
- Firewall enabled
- Per-interface DHCP (when `networking.interface` is set)

### Security
- polkit enabled
- sudo with NOPASSWD reboot for wheel group
- SSH hardened: `PermitRootLogin = "prohibit-password"`, `PasswordAuthentication = false`

### Secrets (agenix — org-level)
- Agenix configuration lives in org nixosModules (injected via `fleet.nix`), not in `core/nixos.nix`
- Identity paths: `~/.keys/id_ed25519` + impermanent fallback at `/persist`
- Secrets: `github-ssh-key`, `github-signing-key`, `user-password`, `root-password`
- WiFi secrets: dynamically generated from `hS.wifiNetworks` list

### WiFi bootstrap service
Systemd oneshot that copies WiFi `.nmconnection` files from agenix secrets to NetworkManager if absent. Runs after agenix, before NetworkManager.

### Users
- Primary user: normal user, wheel + optional groups (audio, video, docker, git, networkmanager)
- Shell: zsh
- Authorized SSH keys from `hS.sshAuthorizedKeys` (set by org defaults in `fleet.nix`)
- Hashed password from `hS.hashedPasswordFile` / `hS.rootHashedPasswordFile` (set by org defaults)

### Claude Code org-level deny list
Writes `/etc/claude-code/settings.json` with non-overridable deny rules:
- Destructive: `rm -rf`, `dd`, `mkfs`, `shred`
- Privilege escalation: `sudo`, `pkexec`, `doas`, `su`
- Dangerous git: `push --force`, `reset --hard`, `clean -fd`
- Nix store: `nix-store --delete`, `nix store delete`

### System packages
gitFull, inetutils

## Dependencies

- Inputs: disko
- Agenix and secrets are org-level concerns, injected via `mkOrg nixosModules`

## Links

- [Core Overview](README.md)
- [Secrets](../secrets/README.md)
- [Claude Permissions](../claude/permissions.md)
