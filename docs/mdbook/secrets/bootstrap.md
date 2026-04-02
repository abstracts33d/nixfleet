# Secrets Bootstrap

## Purpose

During NixOS installation, secrets decryption keys may need to be provisioned to the target machine. This is a fleet-level concern — the framework does not mandate a specific approach.

## General Pattern

1. Prepare a temporary directory with files to copy to the target
2. Pass it to nixos-anywhere via `--extra-files <dir>`
3. nixos-anywhere copies the files to the target during installation
4. On first boot, the secrets tool finds its key and decrypts secrets

## Example: agenix

```sh
# Prepare extra files
mkdir -p /tmp/extra/persist/home/<user>/.keys
cp ~/.keys/id_ed25519 /tmp/extra/persist/home/<user>/.keys/

# Install with key provisioning
nixos-anywhere --flake .#<hostname> --extra-files /tmp/extra root@<ip>
```

## Example: sops-nix

```sh
# sops-nix typically uses age keys or GPG
mkdir -p /tmp/extra/persist/home/<user>/.config/sops/age
cp ~/.config/sops/age/keys.txt /tmp/extra/persist/home/<user>/.config/sops/age/

nixos-anywhere --flake .#<hostname> --extra-files /tmp/extra root@<ip>
```

## Alternative: generate keys on the target

Some setups generate a host-specific key pair on the target machine during or after installation, then encrypt secrets for that key. This avoids copying keys from the installer machine entirely.

## Security Notes

- Keys provisioned via `--extra-files` are copied from the installer's machine to the target
- On impermanent hosts, place keys in a persisted path (e.g., under `/persist/`)
- The framework's impermanence scope auto-persists `.keys` if it exists

## Links

- [Secrets Overview](README.md)
- [Impermanence](../scopes/impermanence.md)
