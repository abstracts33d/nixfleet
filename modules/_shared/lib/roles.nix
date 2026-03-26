# Built-in role definitions.
#
# Framework roles set only framework-level hostSpec flags.
# Fleet-specific flags (isDev, isGraphical, useNiri, etc.) should be
# set by the consuming fleet's own role definitions or host overrides.
let
  mkRole = import ./mk-role.nix;
in {
  workstation = mkRole {
    name = "workstation";
    hostSpecDefaults = {
      isImpermanent = true;
    };
  };
  server = mkRole {
    name = "server";
    hostSpecDefaults = {
      isServer = true;
    };
  };
  minimal = mkRole {
    name = "minimal";
    hostSpecDefaults = {
      isMinimal = true;
    };
  };
  vm-test = mkRole {
    name = "vm-test";
    hostSpecDefaults = {
      isImpermanent = true;
    };
  };
  edge = mkRole {
    name = "edge";
    hostSpecDefaults = {
      isServer = true;
      isMinimal = true;
    };
  };
  darwin-workstation = mkRole {
    name = "darwin-workstation";
    hostSpecDefaults = {
      isDarwin = true;
    };
  };
}
