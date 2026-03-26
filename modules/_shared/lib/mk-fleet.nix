# Fleet entry point. Builds nixosConfigurations and darwinConfigurations.
{
  inputs,
  config,
  lib,
}: let
  constructors = import ../mk-host.nix {inherit inputs config;};
in
  {
    organizations,
    hosts,
    extensions ? [],
    ...
  }: let
    # Build org index for validation
    orgIndex = builtins.listToAttrs (map (o: {
        name = o.name;
        value = o;
      })
      organizations);

    # Validate all hosts reference an org in the organizations list
    _validateOrgs = map (h:
      assert builtins.hasAttr h.org.name orgIndex
      || throw "Host '${h.hostName}' references org '${h.org.name}' which is not in the organizations list"; true)
    hosts;

    # Extensions module for nixfleet-platform paid modules
    extensionsModule = ./extensions-options.nix;

    # Reference _validateOrgs to ensure validation is not optimized away
    _validated = builtins.length _validateOrgs;

    # Generate NixOS modules that inject org/role defaults via the module system
    buildHostSpecModules = host: let
      orgModule = {
        hostSpec = lib.mapAttrs (_: v: lib.mkDefault v) host.org.hostSpecDefaults;
      };
      roleModule =
        if host.role != null
        then {hostSpec = lib.mapAttrs (_: v: lib.mkDefault v) host.role.hostSpecDefaults;}
        else {};
      metaModule = {
        hostSpec =
          {
            organization = host.org.name;
          }
          // (lib.optionalAttrs (host.role != null) {
            role = host.role.name;
          })
          // (lib.optionalAttrs (host.org.secretsPath != null) {
            secretsPath = lib.mkDefault host.org.secretsPath;
          });
      };
    in [orgModule roleModule metaModule];

    buildHost = host: let
      specModules = buildHostSpecModules host;
      roleModules =
        if host.role != null
        then host.role.modules
        else [];
      # Inject org/role hostSpec defaults into HM too (HM has its own hostSpec)
      hmSpecModules = specModules;
      # Merge org defaults + role defaults + host-specific values so that
      # early-evaluated fields (e.g. userName, hostName) are always present.
      # Priority: host > role > org (right-hand side wins in //).
      roleDefaults =
        if host.role != null
        then host.role.hostSpecDefaults
        else {};
      effectiveHostSpecValues =
        {hostName = host.hostName;}
        // host.org.hostSpecDefaults
        // roleDefaults
        // host.hostSpecValues;
    in
      if host.isDarwin
      then
        constructors.mkDarwinHost {
          hostSpecValues = effectiveHostSpecValues;
          platform = host.platform;
          extraDarwinModules = [extensionsModule] ++ extensions ++ specModules ++ host.org.darwinModules ++ host.extraModules ++ roleModules;
          extraHmModules = hmSpecModules ++ host.org.hmModules ++ host.extraHmModules;
          stateVersion = host.stateVersion;
        }
      else if host.isVm
      then
        constructors.mkVmHost
        ({
            hostSpecValues = effectiveHostSpecValues;
            platform = host.platform;
            extraNixosModules = [extensionsModule] ++ extensions ++ specModules ++ host.org.nixosModules ++ host.extraModules ++ roleModules;
            extraHmModules = hmSpecModules ++ host.org.hmModules ++ host.extraHmModules;
            stateVersion = host.stateVersion;
          }
          // (lib.optionalAttrs (host.vmHardwareModules != null) {
            hardwareModules = host.vmHardwareModules;
          }))
      else
        constructors.mkNixosHost {
          hostSpecValues = effectiveHostSpecValues;
          platform = host.platform;
          hardwareModules = host.hardwareModules;
          extraNixosModules = [extensionsModule] ++ extensions ++ specModules ++ host.org.nixosModules ++ host.extraModules ++ roleModules;
          extraHmModules = hmSpecModules ++ host.org.hmModules ++ host.extraHmModules;
          stateVersion = host.stateVersion;
        };

    nixosHosts = builtins.filter (h: !h.isDarwin) hosts;
    darwinHosts = builtins.filter (h: h.isDarwin) hosts;
  in
    assert _validated >= 0; {
      nixosConfigurations = builtins.listToAttrs (map (h: {
          name = h.hostName;
          value = buildHost h;
        })
        nixosHosts);

      darwinConfigurations = builtins.listToAttrs (map (h: {
          name = h.hostName;
          value = buildHost h;
        })
        darwinHosts);
    }
