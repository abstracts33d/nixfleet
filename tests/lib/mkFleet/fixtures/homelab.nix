# tests/lib/mkFleet/fixtures/homelab.nix
#
# Acceptance fixture: imports examples/fleet-homelab/fleet.nix with stub
# nixosConfigurations. Pinned golden at homelab.resolved.json proves the
# homelab example produces a schemaVersion:1 artifact matching RFC-0001 §4.1.
{mkFleet, ...}: let
  stub = import ./_stub-configuration.nix {};
in
  import ../../../../examples/fleet-homelab/fleet.nix {
    self = {
      nixosConfigurations = {
        m70q-attic = stub;
        workstation = stub;
        rpi-sensor-01 = stub;
      };
    };
    nixfleet.lib.mkFleet = mkFleet;
  }
