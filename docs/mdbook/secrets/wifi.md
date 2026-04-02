# WiFi Provisioning

## Purpose

Bootstrap WiFi connectivity on first boot using encrypted credentials. This is a fleet-level concern — the framework does not include a built-in WiFi provisioning mechanism, but provides the patterns for fleet repos to implement it.

## Pattern

Fleet repos can implement WiFi provisioning by:

1. Encrypting WiFi `.nmconnection` files with their secrets tool (agenix, sops, etc.)
2. Adding a systemd service that copies the decrypted credentials to NetworkManager's directory before it starts
3. NetworkManager picks up the connection and connects automatically

## Example Implementation

```nix
# In a fleet secrets/wifi module:
age.secrets."wifi-home" = {
  file = "${secretsRepo}/wifi-home.age";
  path = "/run/agenix/wifi-home";
};

# Systemd service to bootstrap WiFi
systemd.services."bootstrap-wifi" = {
  after = [ "agenix.service" ];
  before = [ "NetworkManager.service" ];
  # Copy .nmconnection from decrypted secret to NM directory
};
```

## Extending hostSpec

Fleet repos that want a declarative WiFi interface can extend `hostSpec` with their own option:

```nix
# In a fleet module:
options.hostSpec.wifiNetworks = lib.mkOption {
  type = lib.types.listOf lib.types.str;
  default = [];
  description = "WiFi network names to provision on this host";
};
```

This keeps the framework generic while letting each fleet define its own WiFi provisioning strategy.

## Links

- [Secrets Overview](README.md)
