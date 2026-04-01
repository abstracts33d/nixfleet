# Example: Client Fleet

This is a minimal example showing how an organization would consume NixFleet as a framework.

## Structure

```
client-fleet/
├── flake.nix        # Imports nixfleet, defines hosts via mkHost
├── secrets.nix      # Org-specific secrets backend (agenix, sops, etc.)
└── README.md
```

## How it works

1. **`flake.nix`** imports NixFleet as a flake input and calls `mkHost` per host
2. **Org defaults** are defined as a `let` binding and merged into each host's `hostSpecValues`
3. **`secrets.nix`** wires the org's chosen secrets backend (NixFleet is secrets-agnostic)

The framework provides all core modules (SSH hardening, firewall, impermanence) and scopes (base, impermanence, agent, control-plane). The client only defines what is specific to their organization.

## Consumption pattern

```nix
# flake.nix
{
  inputs = {
    nixfleet.url = "github:nixfleet/nixfleet";
    # Follow NixFleet's tested versions
    nixpkgs.follows = "nixfleet/nixpkgs";
    home-manager.follows = "nixfleet/home-manager";
    # Org-specific inputs
    secrets.url = "git+ssh://git@github.com/acme/secrets.git";
  };

  outputs = { nixfleet, secrets, ... }:
    let
      mkHost = nixfleet.lib.mkHost;

      orgDefaults = {
        userName = "admin";
        timeZone = "Europe/Berlin";
        locale = "de_DE.UTF-8";
        sshAuthorizedKeys = [ "ssh-ed25519 AAAA..." ];
      };
    in {
      nixosConfigurations = {
        web-01 = mkHost {
          hostName = "web-01";
          platform = "x86_64-linux";
          hardwareModules = [ ./hardware/web-01 ];
          hostSpecValues = orgDefaults // {
            hostName = "web-01";
            isServer = true;
          };
          extraModules = [ secrets.nixosModules.default ];
        };

        dev-workstation = mkHost {
          hostName = "dev-workstation";
          platform = "x86_64-linux";
          hardwareModules = [ ./hardware/dev-workstation ];
          hostSpecValues = orgDefaults // {
            hostName = "dev-workstation";
            isDev = true;
            isGraphical = true;
            isImpermanent = true;
          };
          extraModules = [ secrets.nixosModules.default ];
        };
      };
    };
}
```

## Deployment

```sh
# Fresh install
nixos-anywhere --flake .#web-01 root@192.168.1.10

# Rebuild
sudo nixos-rebuild switch --flake .#web-01

# macOS
darwin-rebuild switch --flake .#<hostname>
```
