# `modules/scopes/nixfleet/_microvm-host.nix`

NixOS module for hosting microVMs as first-class fleet members.
Provides bridge networking, DHCP, and NAT infrastructure.
MicroVMs are defined via the upstream microvm.vms option.
Auto-included by mkHost (disabled by default).

## Bindings

### `systemd.network`

Bridge interface

### `boot.kernel.sysctl`

IP forwarding for NAT

### `networking.nat`

NAT for microVM bridge subnet

### `services.dnsmasq`

DHCP server on bridge

