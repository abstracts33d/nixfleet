# Enterprise Scopes

Enterprise scopes provide optional integration with corporate infrastructure. Each scope is gated by a hostSpec flag and activates only on hosts that declare it.

## Available Scopes

| Flag | Scope | What it enables |
|------|-------|-----------------|
| `useVpn` | [vpn](vpn.md) | Corporate VPN client (WireGuard/OpenVPN) |
| `useFilesharing` | [filesharing](filesharing.md) | Samba/CIFS file sharing and network drives |
| `useLdap` | [auth](auth.md) | LDAP/AD authentication (sssd/PAM) |
| `usePrinting` | [printing](printing.md) | Network printing (CUPS + auto-discovery) |
| `useCorporateCerts` | [certificates](certificates.md) | Corporate CA trust and client certificates |
| `useProxy` | [proxy](proxy.md) | System-wide HTTP/HTTPS proxy |

## Usage

Add the relevant flag(s) to your host's `hostSpecValues`:

```nix
hostSpecValues = {
  hostName = "work-laptop";
  useVpn = true;
  useLdap = true;
  useCorporateCerts = true;
};
```

Scope modules in `modules/scopes/enterprise/` self-activate via `lib.mkIf hS.<flag>`.
