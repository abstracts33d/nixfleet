# Integration test: validates the flakeModule consumption pattern.
#
# Simulates a client repo importing nixfleet.flakeModules.default.
# Proves that:
# 1. config.nixfleet.lib is accessible
# 2. mkFleet, mkOrg, mkHost produce valid nixosConfigurations
# 3. Deferred modules (core, scopes) are available to hosts
# 4. Framework-level hostSpec options (organization, role, etc.) work through the flakeModule
#
# NOTE: Fleet-specific hostSpec options (isDev, isGraphical, theme, etc.)
# are NOT tested here — those are declared by consuming fleets.
#
# Run: nix build .#checks.x86_64-linux.integration-mock-client --no-link
{
  config,
  inputs,
  ...
}: let
  # Simulate what a client would do after importing flakeModules.default:
  # access the lib via config.nixfleet.lib
  inherit (config.nixfleet.lib) mkFleet mkOrg mkHost builtinRoles;

  # Client defines their org
  mockOrg = mkOrg {
    name = "mock-client";
    hostSpecDefaults = {
      userName = "testuser";
      timeZone = "America/New_York";
      locale = "en_US.UTF-8";
      keyboardLayout = "us";
    };
  };

  # Client defines a minimal fleet
  mockFleet = mkFleet {
    organizations = [mockOrg];
    hosts = [
      (mkHost {
        hostName = "mock-host";
        platform = "x86_64-linux";
        org = mockOrg;
        isVm = true;
        hostSpecValues = {
          isMinimal = true;
        };
      })
      # Host that overrides org defaults
      (mkHost {
        hostName = "mock-override";
        platform = "x86_64-linux";
        org = mockOrg;
        isVm = true;
        hostSpecValues = {
          userName = "override-user";
          timeZone = "Europe/London";
          isMinimal = true;
        };
      })
      # Host with a role
      (mkHost {
        hostName = "mock-role";
        platform = "x86_64-linux";
        org = mockOrg;
        role = builtinRoles.minimal;
        isVm = true;
      })
    ];
  };

  # Assertions
  mockCfg = mockFleet.nixosConfigurations.mock-host;
  overrideCfg = mockFleet.nixosConfigurations.mock-override;
  roleCfg = mockFleet.nixosConfigurations.mock-role;
  assert' = check: msg: {inherit check msg;};
in {
  flake.checks.x86_64-linux.integration-mock-client = let
    assertions = [
      # 1. Fleet produces nixosConfigurations
      (assert' (mockFleet ? nixosConfigurations) "mkFleet returns nixosConfigurations")
      (assert' (mockFleet ? darwinConfigurations) "mkFleet returns darwinConfigurations")

      # 2. Host exists
      (assert' (mockFleet.nixosConfigurations ? mock-host) "mock-host exists in nixosConfigurations")

      # 3. Org defaults propagate
      (assert' (mockCfg.config.hostSpec.organization == "mock-client") "organization from mkOrg propagates")
      (assert' (mockCfg.config.hostSpec.userName == "testuser") "userName from org defaults propagates")

      # 4. Locale/timezone from org defaults
      (assert' (mockCfg.config.time.timeZone == "America/New_York") "timeZone from org defaults reaches NixOS config")
      (assert' (mockCfg.config.i18n.defaultLocale == "en_US.UTF-8") "locale from org defaults reaches NixOS config")

      # 5. Framework scopes activate via hostSpec flags
      (assert' (mockCfg.config.hostSpec.isMinimal == true) "isMinimal flag propagates")

      # 6. Extensions namespace available
      (assert' (mockCfg.config.nixfleet.extensions == {}) "nixfleet.extensions namespace is empty by default")

      # 7. Host-level override of org defaults (mkDefault priority)
      (assert' (overrideCfg.config.hostSpec.userName == "override-user") "host-level userName overrides org default")
      (assert' (overrideCfg.config.hostSpec.organization == "mock-client") "organization stays from org even with host override")
      (assert' (overrideCfg.config.time.timeZone == "Europe/London") "host-level timeZone overrides org default")

      # 8. Role defaults propagate
      (assert' (roleCfg.config.hostSpec.isMinimal == true) "minimal role sets isMinimal via mkDefault")
      (assert' (roleCfg.config.hostSpec.role == "minimal") "role name propagates to hostSpec")
    ];
    failures = builtins.filter (a: !a.check) assertions;
    report =
      if failures == []
      then "All ${toString (builtins.length assertions)} integration assertions passed."
      else builtins.throw "Integration test failures:\n${builtins.concatStringsSep "\n" (map (f: "  FAIL: ${f.msg}") failures)}";
  in
    inputs.nixpkgs.legacyPackages.x86_64-linux.runCommand "integration-mock-client" {} ''
      echo "${report}"
      mkdir -p $out
      echo "${report}" > $out/result
    '';
}
