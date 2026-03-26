# NixFleet Framework Module (importApply-compatible)
#
# This is the core framework flakeModule. It is designed to work with
# flake-parts importApply for the 2-repo split:
#
#   flakeModule = importApply ./flake-module.nix { frameworkInputs = inputs; };
#
# In monorepo mode, the wrapper at modules/flake-module.nix calls this
# with frameworkInputs = null (falls back to the flake's own inputs).
{frameworkInputs ? null}: {
  inputs,
  config,
  lib,
  ...
}: let
  # Framework inputs: injected (extracted repo) or flake's own (monorepo)
  fwInputs =
    if frameworkInputs != null
    then frameworkInputs
    else inputs;

  # NixFleet library with framework inputs baked in
  nixfleetLib = import ./default.nix {
    inputs = fwInputs;
    inherit config lib;
  };
in {
  # NixFleet library via module option (for flake-parts consumers)
  options.nixfleet.lib = lib.mkOption {
    type = lib.types.attrs;
    default = nixfleetLib;
    readOnly = true;
    description = "NixFleet library (mkFleet, mkOrg, mkRole, mkHost, mkBatchHosts, mkTestMatrix)";
  };

  # Framework deferred modules
  imports = [
    # Module namespace declaration
    ../../module-options.nix

    # Core
    ../../core/nixos.nix
    ../../core/darwin.nix
    ../../core/home.nix

    # Scopes
    ../../scopes/base.nix
    ../../scopes/catppuccin.nix
    ../../scopes/nix-index.nix
    ../../scopes/impermanence.nix
    ../../scopes/graphical/nixos.nix
    ../../scopes/graphical/home.nix
    ../../scopes/dev/nixos.nix
    ../../scopes/dev/home.nix
    ../../scopes/desktop/niri.nix
    ../../scopes/desktop/hyprland.nix
    ../../scopes/desktop/gnome.nix
    ../../scopes/display/greetd.nix
    ../../scopes/display/gdm.nix
    ../../scopes/hardware/bluetooth.nix
    ../../scopes/darwin/homebrew.nix
    ../../scopes/darwin/karabiner.nix
    ../../scopes/darwin/aerospace.nix

    # NixFleet services
    ../../scopes/nixfleet/agent.nix
    ../../scopes/nixfleet/control-plane.nix

    # perSystem packages (agent, control-plane, CLI binaries)
    ../../agent-package.nix
  ];
}
