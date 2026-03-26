# VM Tests (Tier A)

## Purpose

Boot NixOS VMs and assert runtime state: services running, binaries present, configs deployed, negative tests for disabled features. Uses `pkgs.testers.nixosTest`.

## Location

- `modules/tests/vm.nix`

## Test Suites

### vm-core
Non-graphical, non-dev node. Tests:
- `multi-user.target` active
- sshd running
- NetworkManager running
- iptables Chain INPUT present (firewall)
- testuser exists, in wheel group
- zsh and git available

### vm-shell-hm
Non-graphical, non-dev node. Tests HM activation:
- `home-manager-testuser.service` completes
- `~/.config/starship.toml` exists
- Binaries: starship, nvim, tmux, fzf, eza, rg, bat
- `git config user.name` works

### vm-graphical
Node with `useNiri = true`. Tests:
- greetd service running
- `niri` binary available
- tuigreet in nix store
- HM activation complete
- kitty available
- `~/.config/niri/config.kdl` deployed
- pipewire unit file exists
- MesloLG font installed

### vm-minimal
Node with `isMinimal = true`. Negative tests:
- zsh present (core, always)
- niri NOT present
- firefox NOT present (no graphical HM apps)
- docker NOT running (no dev)

## mkTestNode Helper

Builds nixosTest-compatible node configs with:
- All deferred NixOS + HM modules included
- Agenix secrets stubbed (`lib.mkForce {}`)
- Known test password ("test") for user and root
- nixpkgs with `allowUnfree = true`

## Platform

x86_64-linux only (nixosTest requirement).

## Links

- [Testing Overview](README.md)
- [Eval Tests](eval-tests.md)
