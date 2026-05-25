# Example: a Sécurix-hardened endpoint composed via NixFleet's `mkFleet`.
#
# Sécurix (https://github.com/arcanesys/securix) is an ANSSI-hardened
# NixOS for government laptops — lanzaboote secure-boot, agenix, disko,
# auditd, the full ANSSI-BP-028 module tree. This example demonstrates
# that Sécurix's NixOS modules compose cleanly inside NixFleet: declare
# the endpoint as a single-host fleet via `mkFleet`, point
# `nixosArgs.modules` at Sécurix's exposed module attributes, get a
# `nixosConfigurations.lab-endpoint` you can `nixos-anywhere`-deploy.
#
# The framework stays oblivious to ANSSI, hardware SKUs, lanzaboote,
# etc. — Sécurix drops in like any other NixOS module set.
#
# Build:   nix build .#nixosConfigurations.lab-endpoint.config.system.build.toplevel
# Deploy:  nixos-anywhere --flake .#lab-endpoint root@<ip>
# VM test: nix run .#build-vm -- -h lab-endpoint
#          nix run .#start-vm -- -h lab-endpoint --display gtk --ram 4096
#
# Before booting: replace the placeholder SSH key with your own:
#   sed -i 's|ssh-ed25519 NixfleetExampleKeyReplaceWithYourOwn|'"$(cat ~/.ssh/id_ed25519.pub)"'|g' host.nix
{
  description = "Sécurix endpoint composed via NixFleet mkFleet";

  inputs = {
    nixfleet.url = "github:arcanesys/nixfleet";
    # Sécurix's flake wrapper lives on feat/flake-cleanup until merged
    # to main; exposes nixosModules.{securix-base, securix-hardware.<sku>}.
    securix.url = "github:arcanesys/securix/feat/flake-cleanup";
    securix.inputs.nixpkgs.follows = "nixfleet/nixpkgs";
    nixpkgs.follows = "nixfleet/nixpkgs";
    flake-parts.follows = "nixfleet/flake-parts";
  };

  outputs = inputs:
    inputs.flake-parts.lib.mkFlake {inherit inputs;} {
      systems = ["x86_64-linux"];

      # Single-host fleet via mkFleet (RFC-0004 §2.2): declare hosts +
      # channels + rolloutPolicies; the wrapper iterates `hosts` and
      # calls `mkHost` per host with `fleetResolved` pre-bound from
      # the resolved fleet topology.
      flake.fleet = inputs.nixfleet.lib.mkFleet {
        hosts.lab-endpoint = {
          platform = "x86_64-linux";
          channel = "stable";
          tags = [];
          # Per-host mkHost arguments passed through by the framework.
          nixosArgs = {
            hostSpec = {
              timeZone = "Europe/Paris";
              locale = "fr_FR.UTF-8";
              # Matches Sécurix's default; otherwise both modules set
              # `console.keyMap` at the same priority and merge fails.
              keyboardLayout = "fr";
            };
            modules = [
              # Canonical platform declaration. The mkFleet `platform`
              # field above is a dispatch hint; this is the value mkHost
              # asserts against post-build (lib/mk-host.nix platformCheck).
              {nixpkgs.hostPlatform = "x86_64-linux";}

              # Sécurix base - bundles lanzaboote + agenix + disko + the
              # full ANSSI module tree (anssi, bastion, vpn, pam,
              # auditd, ...).
              inputs.securix.nixosModules.securix-base

              # SKU hardware profile. Pick from: e14-g7, elitebook645g11,
              # elitebook850g8, latitude5340, t14g6, x9-15, x280.
              # Omit on VM - vm-overrides.nix neutralizes the hardware bits.
              inputs.securix.nixosModules.securix-hardware.t14g6

              # Host-specific: operators + securix.self metadata + agent.
              ./host.nix

              # VM-only overrides (disable Secure Boot + LUKS, set up disko).
              # Drop this import for a real-hardware deploy.
              ./vm-overrides.nix
            ];
          };
        };

        # Single-channel single-policy: this example is a one-host
        # endpoint, not a managed fleet. mkFleet still requires the
        # channel + policy declarations even for N=1; the operator
        # path's minimum boilerplate.
        channels.stable = {
          rolloutPolicy = "all-at-once";
          signingIntervalMinutes = 60;
          freshnessWindow = 1440;
        };

        rolloutPolicies.all-at-once = {
          strategy = "all-at-once";
          waves = [
            {
              selector.all = true;
              soakMinutes = 0;
            }
          ];
        };
      };

      # Surface the built nixosSystem at the conventional path so the
      # `build` / `deploy` commands in the header comment work as-is.
      flake.nixosConfigurations =
        inputs.self.fleet.nixosConfigurations;

      perSystem = {
        pkgs,
        system,
        ...
      }: {
        apps = inputs.nixfleet.lib.mkVmApps {inherit pkgs;};

        # Minimal installer ISO with a placeholder root SSH key — needed
        # by `build-vm` (which uses ISO + nixos-anywhere). Replace the key
        # with your own; or, for a real-hardware deploy, skip this and
        # drive nixos-anywhere directly with any installer.
        packages.iso = let
          isoSystem = inputs.nixpkgs.lib.nixosSystem {
            modules = [
              "${inputs.nixpkgs}/nixos/modules/installer/cd-dvd/installation-cd-minimal.nix"
              {
                nixpkgs.hostPlatform = system;
                users.users.root.openssh.authorizedKeys.keys = [
                  "ssh-ed25519 NixfleetExampleKeyReplaceWithYourOwn"
                ];
                services.openssh.enable = true;
                services.openssh.settings.PermitRootLogin = "prohibit-password";
                services.qemuGuest.enable = true;
                services.spice-vdagentd.enable = true;
              }
            ];
          };
        in
          isoSystem.config.system.build.isoImage;
      };
    };
}
