# treefmt-nix wiring for the framework's own `nix fmt`.
#
# Internal-only: this module gives nixfleet itself a `formatter.<system>`
# output (consumed by `nix fmt`, `.githooks/pre-commit`, CI). The
# framework no longer EXPORTS formatter as a flakeModule for consumer
# fleets — fleets bring their own treefmt config.
#
# Picked up by `import-tree ./modules` automatically.
{inputs, ...}: {
  imports = [inputs.treefmt-nix.flakeModule];

  perSystem = {...}: {
    treefmt = {
      projectRootFile = "flake.nix";
      programs = {
        alejandra.enable = true; # Nix formatter
        shfmt.enable = true; # Shell formatter
        deadnix.enable = true; # Dead code detection
      };
    };
  };
}
