# Built-in role definitions.
{}: let
  mkRole = import ./mk-role.nix {};
in {
  workstation = mkRole {
    name = "workstation";
    hostSpecDefaults = {
      isDev = true;
      isGraphical = true;
      isImpermanent = true;
      useNiri = true;
    };
  };
  server = mkRole {
    name = "server";
    hostSpecDefaults = {
      isServer = true;
      isDev = false;
      isGraphical = false;
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
      isDev = false;
      isGraphical = true;
      isImpermanent = true;
      useNiri = true;
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
      isDev = true;
      isGraphical = true;
    };
  };
}
