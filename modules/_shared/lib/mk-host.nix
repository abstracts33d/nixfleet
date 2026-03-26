# Host descriptor. Builds the attrset that mkFleet consumes.
# Does NOT build the NixOS/Darwin system — mkFleet does that.
{}: {
  hostName,
  platform,
  org,
  role ? null,
  hostSpecValues ? {},
  hardwareModules ? [],
  extraModules ? [],
  extraHmModules ? [],
  stateVersion ? "24.11",
  isVm ? false,
  vmHardwareModules ? null,
}:
assert org._type == "nixfleet-org";
assert role == null || role._type == "nixfleet-role"; {
  inherit
    hostName
    platform
    org
    role
    hostSpecValues
    hardwareModules
    extraModules
    extraHmModules
    stateVersion
    isVm
    vmHardwareModules
    ;
  isDarwin = builtins.elem platform ["aarch64-darwin" "x86_64-darwin"];
  _type = "nixfleet-host";
}
