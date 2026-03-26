# Enterprise scope: Corporate VPN client
# WireGuard/OpenVPN with split tunneling, auto-connect, kill switch
# Phase 2+: reads VPN server config from org defaults
{...}: {
  flake.modules.nixos.enterpriseVpn = {
    config,
    pkgs,
    lib,
    ...
  }: let
    hS = config.hostSpec;
  in {
    config = lib.mkIf hS.useVpn {
      # WireGuard client
      # TODO: networking.wireguard.interfaces from org config
      # TODO: split tunnel rules (org internal CIDRs via VPN, rest direct)
      # TODO: auto-connect on boot via networkmanager-wireguard
      # TODO: kill switch via nftables (block non-VPN traffic when VPN is down)
      # TODO: OpenVPN fallback profile support
      # TODO: secrets (private key, pre-shared key) via agenix

      environment.systemPackages = with pkgs; [
        wireguard-tools
      ];
    };
  };
}
