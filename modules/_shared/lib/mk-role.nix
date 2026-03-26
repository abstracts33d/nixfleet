# Role factory. Returns a typed attrset of hostSpec defaults.
# Roles are decoupled from orgs — reusable across organizations.
{}: {
  name,
  hostSpecDefaults ? {},
  modules ? [],
}:
assert builtins.isString name;
assert builtins.isAttrs hostSpecDefaults; {
  inherit name hostSpecDefaults modules;
  _type = "nixfleet-role";
}
