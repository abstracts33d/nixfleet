# Example fleet definition for a client organization.
#
# This file shows what a typical client fleet.nix looks like.
# It uses config.nixfleet.lib to access the framework API.
{config, ...}: let
  inherit (config.nixfleet.lib) mkFleet mkOrg mkHost mkBatchHosts builtinRoles;

  # --- Organization ---
  acme = mkOrg {
    name = "acme";
    description = "ACME Corp — example NixFleet client";
    hostSpecDefaults = {
      userName = "deploy";
      githubUser = "acme-ops";
      githubEmail = "ops@acme.example.com";
      timeZone = "Europe/Berlin";
      locale = "de_DE.UTF-8";
      keyboardLayout = "de";
      theme = {
        flavor = "mocha";
        accent = "blue";
      };
    };
    # Org-level NixOS modules (applied to every NixOS host)
    nixosModules = [
      # Secrets backend (agenix, sops-nix, etc.)
      # inputs.agenix.nixosModules.default
      # { age.identityPaths = [...]; age.secrets = {...}; }

      # Org-wide NixOS config
      {networking.domain = "acme.example.com";}
    ];
    # Org-level HM modules
    hmModules = [
      ({pkgs, ...}: {
        home.packages = with pkgs; [
          # Org-specific tools
          slack
          zoom-us
        ];
      })
    ];
  };

  # --- Fleet ---
  fleet = mkFleet {
    organizations = [acme];
    hosts =
      # Dev workstations
      [
        (mkHost {
          hostName = "dev-01";
          platform = "x86_64-linux";
          org = acme;
          role = builtinRoles.workstation;
          hardwareModules = [./hardware/dev-01.nix];
        })
      ]
      # Edge servers (batch provisioned)
      ++ (mkBatchHosts {
        template = {
          org = acme;
          role = builtinRoles.edge;
          platform = "x86_64-linux";
          isVm = true;
        };
        instances = [
          {hostName = "edge-berlin-01";}
          {hostName = "edge-berlin-02";}
          {hostName = "edge-munich-01";}
        ];
      });
    extensions = [];
  };
in {
  flake = fleet;
}
