# NixFleet

Declarative NixOS fleet management framework.

## Quick Start

```nix
# In your fleet's flake.nix
{
  inputs.nixfleet.url = "github:abstracts33d/nixfleet";

  outputs = inputs:
    inputs.nixfleet.lib.mkFlake {
      inherit inputs;
      fleet = ./fleet.nix;
    };
}
```

## Documentation

- [Technical docs](docs/src/) — architecture, modules, testing
- [User guide](docs/guide/) — getting started, concepts

## Development

```sh
nix develop                        # dev shell
cargo test --workspace             # Rust tests
nix flake check --no-build         # eval tests
nix run .#validate                 # full validation
nix fmt                            # format
```

## License

Apache-2.0
