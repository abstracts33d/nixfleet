# Example: Client Fleet

This is a minimal example showing how an organization would consume NixFleet as a framework.

## Structure

```
client-fleet/
├── flake.nix        # Imports nixfleet.flakeModules.default
├── fleet.nix        # Organization + hosts definition
├── secrets.nix      # Org-specific secrets backend (agenix, sops, etc.)
└── README.md
```

## How it works

1. **`flake.nix`** imports NixFleet as a flake input and uses `flakeModules.default`
2. **`fleet.nix`** defines the org via `mkOrg` and hosts via `mkHost`
3. **`secrets.nix`** wires the org's chosen secrets backend (NixFleet is secrets-agnostic)

The framework provides all core modules (SSH hardening, firewall, impermanence) and scopes (graphical, dev, enterprise). The client only defines what's specific to their organization.

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

  outputs = inputs: inputs.flake-parts.lib.mkFlake { inherit inputs; } {
    # Import the NixFleet framework
    imports = [inputs.nixfleet.flakeModules.default];
    # Import org fleet definition
    imports = [./fleet.nix];
  };
}
```
