# Enterprise scope: Corporate certificate management
# CA trust store, client certificate provisioning, rotation
# Phase 2+: reads CA bundles from org config / agenix secrets
{...}: {
  flake.modules.nixos.enterpriseCertificates = {
    config,
    pkgs,
    lib,
    ...
  }: let
    hS = config.hostSpec;
  in {
    config = lib.mkIf hS.useCorporateCerts {
      # Corporate CA trust
      # TODO: security.pki.certificateFiles from org config (agenix-managed CA bundles)
      # TODO: security.pki.certificates for inline PEM certs
      # TODO: client certificate provisioning per host (mutual TLS)
      # TODO: OCSP stapling and CRL distribution point config
      # TODO: certificate rotation automation (renewal before expiry)
      # TODO: HSM integration for Enterprise tier (PKCS#11)

      # Useful tools for cert debugging
      environment.systemPackages = with pkgs; [
        openssl
      ];
    };
  };
}
