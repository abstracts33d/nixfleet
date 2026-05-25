# Negative regression: operator declares nixpkgs.hostPlatform that
# disagrees with the mkHost `platform` dispatch hint. The platformCheck
# seq MUST throw; tryEval catches it and the fixture returns "ok".
{
  inputs,
  lib,
}: let
  mkHost = import ../../../lib/mk-host.nix {inherit inputs lib;};
  result = builtins.tryEval (
    let
      built = mkHost {
        hostName = "mismatch-host";
        platform = "x86_64-linux";
        fleetResolved = {effectiveHealthChecks = {};};
        modules = [
          {nixpkgs.hostPlatform = "aarch64-linux";}
        ];
      };
    in
      built.config.nixpkgs.hostPlatform.system
  );
in
  if result.success
  then throw "expected eval failure for platform mismatch, got success: ${toString result.value}"
  else "ok"
