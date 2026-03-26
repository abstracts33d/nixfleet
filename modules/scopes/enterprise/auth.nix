# Enterprise scope: LDAP/AD authentication
# sssd + PAM integration for centralized user management
# Phase 2+: reads LDAP/AD server config from org defaults
{...}: {
  flake.modules.nixos.enterpriseAuth = {
    config,
    pkgs,
    lib,
    ...
  }: let
    hS = config.hostSpec;
  in {
    config = lib.mkIf hS.useLdap {
      # sssd for LDAP/AD integration
      # TODO: services.sssd.enable + sssd.conf from org config
      # TODO: LDAP server URI, base DN, bind credentials via agenix
      # TODO: PAM configuration for sssd auth (security.pam.services)
      # TODO: sudo rules from LDAP/AD groups
      # TODO: home directory auto-creation on first login (pam_mkhomedir)
      # TODO: offline credential caching policy
      # TODO: Kerberos integration for AD environments
      # TODO: SAML/SSO federation (Enterprise tier)

      environment.systemPackages = with pkgs; [
        openldap # ldapsearch, ldapwhoami for debugging
      ];
    };
  };
}
