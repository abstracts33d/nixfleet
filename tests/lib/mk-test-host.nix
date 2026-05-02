# mkTestHost — wrap nixfleet's mkHost with this fleet's test-rig
# opinions (qemu disk + hardware fixtures, mesa software-GL, spice
# vdagent, DHCP). These are NOT framework concerns: they're how
# nixfleet's own test fleet boots its hosts under qemu, and the
# framework's public mkHost API stays clean by keeping them here.
#
# Consumer fleets that want VM-specific config should compose it via
# their own `modules` argument to mkHost rather than relying on a
# framework-level flag.
#
# Usage from a flake (lib/, tests/, etc.) that's already inside this
# repo:
#
#     mkTestHost = import ./tests/lib/mk-test-host.nix { inherit inputs lib; };
#     mkTestHost {
#       hostName = "vm-01";
#       platform = "x86_64-linux";
#       modules = [ ./modules/hosts/vm-01 ];
#     };
#
# All arguments forward to mkHost unchanged. Only difference: the
# qemu fixtures + driver-tweak module are injected into `modules`
# automatically.
{
  inputs,
  lib,
}: let
  mkHost = import ../../lib/mk-host.nix {inherit inputs lib;};

  qemuTestRigModules = [
    ../fixtures/qemu/disk-config.nix
    ../fixtures/qemu/hardware-configuration.nix
    ({
      lib,
      pkgs,
      ...
    }: {
      services.spice-vdagentd.enable = true;
      networking.useDHCP = lib.mkForce true;
      environment.variables.LIBGL_ALWAYS_SOFTWARE = "1";
      environment.systemPackages = [pkgs.mesa];
    })
  ];
in
  args @ {modules ? [], ...}:
    mkHost (args
      // {
        modules = qemuTestRigModules ++ modules;
        # Pass isVm through for any consumer code that still threads
        # it. The framework no longer reads it, but harmless to forward.
        isVm = true;
      })
