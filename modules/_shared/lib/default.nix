# Public API of the NixFleet framework library.
# This file IS the future nixfleet/ repo's entry point.
{
  inputs,
  config,
  lib,
}: {
  mkOrg = import ./mk-org.nix {};
  mkRole = import ./mk-role.nix {};
  mkHost = import ./mk-host.nix {};
  mkFleet = import ./mk-fleet.nix {inherit inputs config lib;};
  mkBatchHosts = import ./mk-batch-hosts.nix {};
  mkTestMatrix = import ./mk-test-matrix.nix {inherit lib;};
  builtinRoles = import ./roles.nix {};
  extensionsModule = ./extensions-options.nix;
}
