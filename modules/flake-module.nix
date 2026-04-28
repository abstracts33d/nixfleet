# NixFleet Framework Export
#
# Auto-imported by import-tree. Exposes the framework API.
#
# Exports:
#   flake.lib              — mkHost / mkFleet / mkVmApps / mergeFleets / withSignature
#   flake.nixosModules.nixfleet-core — for consumers who want raw modules
#                            (without mkHost)
#   flake.scopes.*         — pluggable impls of framework-declared contracts
#                            (persistence, keyslots, gitops, secrets). Fleets
#                            opt in by importing flake.scopes.<family>.<impl>.
{
  inputs,
  lib,
  ...
}: let
  nixfleetLib = import ../lib {inherit inputs lib;};
in {
  options.nixfleet.lib = lib.mkOption {
    type = lib.types.attrs;
    default = nixfleetLib;
    readOnly = true;
    description = "NixFleet library (mkHost / mkFleet / mkVmApps / ...)";
  };

  config.flake = {
    lib = nixfleetLib;

    nixosModules.nixfleet-core = ./core/_nixos.nix;

    # Pluggable impls of framework-declared contracts. Each entry is a
    # NixOS module (or a path to one) the consumer fleet imports
    # explicitly. Sibling entries are alternative impls of the same
    # contract — fleets pick exactly one per family.
    scopes = {
      persistence = {
        impermanence = ../impls/persistence/impermanence.nix;
      };
      keyslots = {
        tpm = ../impls/keyslots/tpm;
      };
      # GitOps source-URL builders — pure data, used by
      # services.nixfleet-control-plane.channelRefsSource.
      # `gitea` shares the Forgejo API verbatim.
      gitops = {
        forgejo = import ../impls/gitops/forgejo.nix;
        gitea = import ../impls/gitops/forgejo.nix;
      };
      # Identity-path resolution for agenix/sops/... backends.
      # Single canonical resolution; not plural.
      secrets = ../impls/secrets;
    };
  };
}
