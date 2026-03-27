# Tier C — Eval tests: assert config properties at evaluation time.
# Runs via `nix flake check` (--no-build skips VM tests, eval checks are instant).
# Each check is a `pkgs.runCommand` that fails if any assertion is false.
#
# NOTE: Fleet-specific hostSpec options (isDev, isGraphical, useNiri, theme, etc.)
# are NOT tested here — those are declared by consuming fleets and tested there.
#
# Tests guard against missing hosts with `lib.optionalAttrs` so they work
# both in the framework repo (reference fleet) and in consuming fleets.
{self, ...}: {
  perSystem = {
    pkgs,
    system,
    lib,
    ...
  }: let
    helpers = import ./_lib/helpers.nix {inherit lib;};
    mkEvalCheck = helpers.mkEvalCheck pkgs;

    # Helper to get a NixOS config by hostname
    nixosCfg = name: self.nixosConfigurations.${name}.config;
    hasHost = name: self.nixosConfigurations ? ${name};
    # Only run on x86_64-linux (all test hosts are x86_64-linux)
  in
    lib.optionalAttrs (system == "x86_64-linux") {
      checks =
        {
          # --- SSH hardening (core/nixos.nix — still in framework) ---
          eval-ssh-hardening = let
            cfg = nixosCfg "krach-qemu";
          in
            mkEvalCheck "ssh-hardening" [
              {
                check = cfg.services.openssh.settings.PermitRootLogin == "prohibit-password";
                msg = "PermitRootLogin is prohibit-password";
              }
              {
                check = cfg.services.openssh.settings.PasswordAuthentication == false;
                msg = "PasswordAuthentication is false";
              }
              {
                check = cfg.networking.firewall.enable;
                msg = "firewall is enabled";
              }
            ];

          # --- NixFleet framework options exist on all hosts ---
          eval-org-field-exists = mkEvalCheck "org-field-exists" [
            {
              check = (nixosCfg "krach-qemu").hostSpec ? organization;
              msg = "hostSpec should have organization option";
            }
            {
              check = (nixosCfg "krach-qemu").hostSpec ? role;
              msg = "hostSpec should have role option";
            }
            {
              check = (nixosCfg "krach-qemu").hostSpec ? secretsPath;
              msg = "hostSpec should have secretsPath option";
            }
          ];

          # --- Organization defaults ---
          eval-org-defaults = let
            cfg = nixosCfg "krach";
          in
            mkEvalCheck "org-defaults" [
              {
                check = cfg.hostSpec ? organization && cfg.hostSpec.organization != "";
                msg = "krach should have an organization set";
              }
              {
                check = cfg.hostSpec ? userName && cfg.hostSpec.userName != "";
                msg = "krach should inherit userName from org";
              }
            ];

          # --- Organization on all hosts ---
          eval-org-all-hosts = let
            orgOf = name: (nixosCfg name).hostSpec.organization;
            refOrg = orgOf "krach";
          in
            mkEvalCheck "org-all-hosts" [
              {
                check = orgOf "krach-qemu" == refOrg;
                msg = "krach-qemu should have same organization as krach";
              }
              {
                check = orgOf "qemu" == refOrg;
                msg = "qemu should have same organization as krach";
              }
              {
                check = orgOf "ohm" == refOrg;
                msg = "ohm should have same organization as krach";
              }
              {
                check = orgOf "lab" == refOrg;
                msg = "lab should have same organization as krach";
              }
            ];

          # --- Secrets path agnostic ---
          eval-secrets-agnostic = mkEvalCheck "secrets-agnostic" [
            {
              check = (nixosCfg "krach").hostSpec.secretsPath == null;
              msg = "secretsPath should default to null (framework-agnostic)";
            }
          ];

          # --- userName in org defaults ---
          eval-username-org-default = let
            refUser = (nixosCfg "krach").hostSpec.userName;
          in
            mkEvalCheck "username-org-default" ([
                {
                  check = refUser != "";
                  msg = "krach should inherit userName from org defaults";
                }
                {
                  check = (nixosCfg "ohm").hostSpec.userName != refUser;
                  msg = "ohm should override userName (different from org default)";
                }
              ]
              ++ lib.optionals (hasHost "edge-01") [
                {
                  check = (nixosCfg "edge-01").hostSpec.userName == refUser;
                  msg = "edge-01 batch host should inherit userName from org";
                }
              ]);

          # --- Locale / timezone (from org defaults) ---
          eval-locale-timezone = let
            cfg = nixosCfg "krach";
          in
            mkEvalCheck "locale-timezone" [
              {
                check = cfg.time.timeZone != "";
                msg = "krach should have timezone from org defaults";
              }
              {
                check = cfg.i18n.defaultLocale != "";
                msg = "krach should have locale from org defaults";
              }
              {
                check = cfg.console.keyMap != "";
                msg = "krach should have keyboard layout from org defaults";
              }
            ];

          # --- SSH authorized keys (from org defaults) ---
          eval-ssh-authorized = let
            cfg = nixosCfg "krach";
            userName = cfg.hostSpec.userName;
          in
            mkEvalCheck "ssh-authorized" [
              {
                check = builtins.length cfg.users.users.${userName}.openssh.authorizedKeys.keys > 0;
                msg = "krach should have SSH authorized keys from org defaults";
              }
              {
                check = builtins.length cfg.users.users.root.openssh.authorizedKeys.keys > 0;
                msg = "krach root should have SSH authorized keys from org defaults";
              }
            ];

          # --- Password files (hostSpec options exist and are wired correctly) ---
          eval-password-files = let
            cfg = nixosCfg "krach";
          in
            mkEvalCheck "password-files" [
              {
                check = cfg.hostSpec ? hashedPasswordFile;
                msg = "hostSpec should have hashedPasswordFile option";
              }
              {
                check = cfg.hostSpec ? rootHashedPasswordFile;
                msg = "hostSpec should have rootHashedPasswordFile option";
              }
            ];

          # --- Extensions namespace ---
          eval-extensions-empty = mkEvalCheck "extensions-empty" [
            {
              check = (nixosCfg "krach-qemu").nixfleet.extensions == {};
              msg = "nixfleet.extensions should be empty attrset by default";
            }
          ];
        }
        # --- Batch hosts (only when edge-01 exists) ---
        // lib.optionalAttrs (hasHost "edge-01") {
          eval-batch-hosts = let
            refOrg = (nixosCfg "krach").hostSpec.organization;
            refUser = (nixosCfg "krach").hostSpec.userName;
          in
            mkEvalCheck "batch-hosts" [
              {
                check = (nixosCfg "edge-01").hostSpec.organization == refOrg;
                msg = "edge-01 batch host should belong to same org as krach";
              }
              {
                check = (nixosCfg "edge-01").hostSpec.isServer == true;
                msg = "edge-01 should have isServer from edge role";
              }
              {
                check = (nixosCfg "edge-01").hostSpec.isMinimal == true;
                msg = "edge-01 should have isMinimal from edge role";
              }
              {
                check = (nixosCfg "edge-01").hostSpec.userName == refUser;
                msg = "edge-01 should inherit userName from org";
              }
            ];
        }
        # --- Test matrix hosts (only when test matrix hosts exist) ---
        // lib.optionalAttrs (hasHost "test-workstation-x86_64") {
          eval-test-matrix = let
            refOrg = (nixosCfg "krach").hostSpec.organization;
          in
            mkEvalCheck "test-matrix" [
              {
                check = (nixosCfg "test-workstation-x86_64").hostSpec.organization == refOrg;
                msg = "test-workstation-x86_64 should belong to same org as krach";
              }
              {
                check = (nixosCfg "test-server-x86_64").hostSpec.isServer == true;
                msg = "test-server-x86_64 should have isServer from server role";
              }
              {
                check = (nixosCfg "test-minimal-x86_64").hostSpec.isMinimal == true;
                msg = "test-minimal-x86_64 should have isMinimal from minimal role";
              }
            ];

          eval-role-defaults = mkEvalCheck "role-defaults" [
            {
              check = (nixosCfg "test-server-x86_64").hostSpec.isServer == true;
              msg = "server role should set isServer = true";
            }
            {
              check = (nixosCfg "test-minimal-x86_64").hostSpec.isMinimal == true;
              msg = "minimal role should set isMinimal = true";
            }
          ];
        };
    };
}
