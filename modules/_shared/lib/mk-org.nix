# Organization factory. Returns a typed attrset consumed by mkFleet.
{}: {
  name,
  description ? "",
  hostSpecDefaults ? {},
  secretsPath ? null,
  roles ? {},
  nixosModules ? [],
  darwinModules ? [],
  hmModules ? [],
}:
assert builtins.isString name;
assert builtins.isAttrs hostSpecDefaults; {
  inherit name description hostSpecDefaults secretsPath nixosModules darwinModules hmModules;
  customRoles = roles;
  _type = "nixfleet-org";
}
