# Extension point for paid nixfleet-platform modules.
{lib, ...}: {
  options.nixfleet.extensions = lib.mkOption {
    type = lib.types.attrsOf lib.types.anything;
    default = {};
    description = "Extension namespace for nixfleet-platform paid modules";
  };
}
