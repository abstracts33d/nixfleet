# Flake Ecosystem

Knowledge about Nix flakes, inputs, and the flake-parts architecture.

## Flake Structure

- **nixpkgs** follows `nixos-unstable`. Formatter is **treefmt-nix** (alejandra + shfmt).
- Built on **flake-parts** + **import-tree**: `flake.nix` calls `inputs.flake-parts.lib.mkFlake` with `inputs.import-tree ./modules`.
- Every `.nix` file under `modules/` is automatically imported by import-tree, except files/directories prefixed with `_`.
- The `systems` list covers `x86_64-linux`, `aarch64-linux`, `aarch64-darwin`, `x86_64-darwin`.

## Key Integrations

| Integration | Purpose |
|-------------|---------|
| **flake-parts** | NixOS module system at the flake level |
| **import-tree** | Auto-imports all `.nix` files under `modules/` |
| **nix-wrapper-modules** | Portable wrapped composites (shell, terminal) |
| **nixos-anywhere** | Remote NixOS installation over SSH |
| **disko** | Declarative disk partitioning |
| **impermanence** | Ephemeral root filesystem |
| **agenix** | Age-encrypted secrets management (org-level, not framework) |
| **catppuccin/nix** | Consistent theming across 200+ apps |
| **nix-index-database** | Command-not-found + comma |
| **lanzaboote** | Secure Boot |
| **treefmt-nix** | Multi-language formatting |
| **nixos-hardware** | Hardware-specific optimizations |
| **nix-homebrew** | Homebrew integration on macOS |
| **home-manager** | User environment management |
| **nix-darwin** | macOS system configuration |

## Multi-Repository Dependencies

### nix-secrets (private)

- **Input:** `inputs.secrets` — private secrets repo (detected from flake input, org-specific)
- **Contains:** Encrypted `.age` files (SSH keys, passwords, WiFi connections)
- **Workflow:** Edit in nix-secrets -> commit -> push -> `nix flake update secrets` here
- **Verification:** After update, build a host: `nix build .#nixosConfigurations.krach`

## Store Optimization

- `auto-optimise-store = true` on both NixOS and Darwin (hardlink deduplication)
- Automatic weekly gc (`--delete-older-than 7d`) on both platforms
- `nix-community.cachix.org` configured with proper public key

## Two-Repo Split Architecture (future)

The framework will be extracted to a `nixfleet/` repo using flake-parts' `flakeModules` mechanism:

- **`importApply` pattern**: framework module receives its own flake scope (`localFlake`) separately from the consumer's inputs
- **Inputs strategy**: framework owns nixpkgs/HM/etc pins, clients `follows` them
- **`config` merging**: framework and client modules evaluate in a single flake-parts module pass -- no special wiring needed
- **import-tree stays client-side**: framework uses explicit imports internally
- **Precedent**: treefmt-nix, devenv, hercules-ci-effects all use this distribution pattern

### Key Design Decisions

- Framework exports `flakeModules.default` -- clients import it as one line
- `mkFleet` stays as a lib function (not an option), preserving current pattern
- Framework captures its own inputs via `importApply` closure -- clients never thread framework inputs
- Deferred modules merge naturally across framework and client via `config.flake.modules.*`

## Flake Inputs (Key Integrations)

| Input | Purpose |
|-------|---------|
| **nixpkgs** | NixOS package set (nixos-unstable) |
| **flake-parts** | NixOS module system at the flake level |
| **import-tree** | Auto-imports all `.nix` files under `modules/` |
| **home-manager** | User environment management |
| **nix-darwin** | macOS system configuration |
| **disko** | Declarative disk partitioning |
| **impermanence** | Ephemeral root filesystem |
| **agenix** | Age-encrypted secrets (org-level, not framework) |
| **catppuccin/nix** | Consistent theming across 200+ apps |
| **nix-index-database** | command-not-found + comma |
| **lanzaboote** | Secure Boot |
| **treefmt-nix** | Multi-language formatting (alejandra + shfmt) |
| **nixos-hardware** | Hardware-specific optimizations (per-host) |
| **nix-homebrew** | Homebrew integration on macOS |
| **nixos-anywhere** | Remote NixOS installation over SSH |
| **nix-wrapper-modules** | Portable wrapped composites (shell, terminal) |
| **secrets** | Private nix-secrets repo (flake = false) |
