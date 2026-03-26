# Testing Your Config

A 3-tier test pyramid ensures your config works before deploying to hardware.

## The Pyramid

| Tier | Speed | What it tests | Command |
|------|-------|--------------|---------|
| Eval | Instant | Config correctness (flags, options) | `nix flake check` |
| VM | Minutes | Runtime behavior (services, binaries) | `nix run .#validate -- --vm` |
| Smoke | Manual | Real-world state on live hardware | Post build-switch |

## Eval Tests (Tier C)

23 eval checks run instantly and catch configuration errors:
- hostSpec smart defaults propagate correctly
- Scopes activate/deactivate based on flags
- Impermanence paths are declared
- SSH hardening options are set
- Home Manager programs are enabled
- Dev scope activates/deactivates with isDev flag
- Organization defaults (userName, githubUser, githubEmail) propagate to all hosts
- Enterprise scopes remain inactive when flags are off
- Batch hosts (edge fleet) inherit org defaults and role flags
- Test matrix hosts get correct role defaults
- The `nixfleet.extensions` namespace exists and defaults to empty
- Secrets path defaults to null (framework-agnostic)

Run them with:
```sh
nix flake check --no-build    # eval only, no builds
nix run .#validate            # includes eval + builds
```

## VM Tests (Tier A)

These boot real NixOS VMs and verify runtime behavior:
- **vm-core** — SSH, NetworkManager, firewall, user/groups
- **vm-shell-hm** — Home Manager, zsh, git, starship, neovim
- **vm-graphical** — greetd, niri, kitty, pipewire, fonts
- **vm-minimal** — negative test: no graphical, no dev, no docker

Run them with:
```sh
nix run .#validate -- --vm    # x86_64-linux only
```

## Pre-commit and Pre-push

Git hooks enforce quality:
- **pre-commit** — format check
- **pre-push** — full validation

## When to Add Tests

- New hostSpec flag? Add eval assertions
- New scope with services? Add VM test cases
- New runtime behavior? Add to the appropriate VM suite

## Further Reading

- [VM Testing](vm-testing.md) — detailed VM test guide
- [Technical Test Details](../../testing/README.md) — test implementation
