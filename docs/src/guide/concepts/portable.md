# Portable Environments

Use your dev setup on any machine with Nix installed.

## The Idea

Not every machine you work on runs NixOS. SSH into a server, use a colleague's laptop, or work on a fresh VM — you still want your tools and config.

Portable wrappers solve this: self-contained packages that bring your entire development environment.

## The Shell

```sh
nix run github:abstracts33d/nixfleet#shell
```

This gives you:
- Zsh with your full configuration
- 20+ CLI tools (git, ripgrep, fd, bat, jq, tmux, neovim, etc.)
- Starship prompt
- Git config

No installation. No system changes. Just run and go.

## The Terminal

```sh
nix run github:abstracts33d/nixfleet#terminal
```

Kitty terminal wrapping the portable shell. For when you also want your terminal emulator config.

## What Is Not Portable

Anything requiring GPU drivers or system-level integration:
- Desktop compositors (Niri, Hyprland)
- Audio (PipeWire)
- Bluetooth

These live in NixOS scopes, not wrappers. The boundary is clear: if it needs host drivers, it is a scope. If it is pure software, it can be a wrapper.

## How It Works

Wrappers use [nix-wrapper-modules](https://github.com/BirdeeHub/nix-wrapper-modules) to create self-contained packages. Config files from `_config/` are shared between wrappers and Home Manager, so you get the same experience everywhere.

## Further Reading

- [Technical Wrapper Details](../../wrappers/README.md) — implementation details
- [Cross-Platform Design](../advanced/cross-platform.md) — platform considerations
