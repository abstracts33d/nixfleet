# Positive regression: operator declares nixpkgs.hostPlatform matching
# the mkHost `platform` dispatch hint. The post-build platformCheck seq
# is a no-op; readback returns the declared system.
{
  inputs,
  lib,
}: let
  mkHost = import ../../../lib/mk-host.nix {inherit inputs lib;};
  built = mkHost {
    hostName = "match-host";
    platform = "x86_64-linux";
    fleetResolved = {effectiveHealthChecks = {};};
    modules = [
      {nixpkgs.hostPlatform = "x86_64-linux";}
    ];
  };
  sys = built.config.nixpkgs.hostPlatform.system;
in
  if sys == "x86_64-linux"
  then "ok"
  else throw "platform readback mismatch: got '${sys}'"
