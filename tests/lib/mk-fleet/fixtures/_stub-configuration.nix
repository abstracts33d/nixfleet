# tests/lib/mk-fleet/fixtures/_stub-configuration.nix
#
# Minimal stub that looks like a nixosConfiguration enough to satisfy
# the `host.configuration` invariant without needing to evaluate NixOS.
{}: {
  config.system.build.toplevel = {
    outPath = "/nix/store/0000000000000000000000000000000000000000-stub";
    drvPath = "/nix/store/0000000000000000000000000000000000000000-stub.drv";
  };
}
