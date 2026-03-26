# Batch host generator. Takes a template + instances, returns a list of mkHost outputs.
# Template is a partial mkHost attrset. Each instance provides hostName + optional overrides.
let
  mkHost = import ./mk-host.nix;
in
  {
    template,
    instances,
    ...
  }:
    map (
      instance:
        mkHost (
          template
          // instance
          // {
            hostSpecValues = (template.hostSpecValues or {}) // (instance.hostSpecValues or {});
          }
        )
    )
    instances
