# Enterprise scope: System-wide proxy configuration
# HTTP/HTTPS/SOCKS proxy, no_proxy lists, PAC file support
# Phase 2+: reads proxy config from org defaults
{...}: {
  flake.modules.nixos.enterpriseProxy = {
    config,
    lib,
    ...
  }: let
    hS = config.hostSpec;
  in {
    config = lib.mkIf hS.useProxy {
      # System-wide proxy
      # TODO: networking.proxy.httpProxy from org config
      # TODO: networking.proxy.httpsProxy from org config
      # TODO: networking.proxy.noProxy with org internal domains
      # TODO: PAC file URL support (proxy auto-configuration)
      # TODO: per-app proxy exceptions (docker, git, npm, pip)
      # TODO: proxy authentication credentials via agenix
      # TODO: SOCKS proxy support for specific apps

      # Environment variables propagated to all services
      # networking.proxy.default = "http://proxy.corp.example.com:8080";
      # networking.proxy.noProxy = "127.0.0.1,localhost,.corp.example.com";
    };
  };
}
