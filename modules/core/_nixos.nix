# Core NixOS module - framework prerequisites only.
#
# What the framework needs every NixOS host to have:
# - flake-mode nix (the framework is flake-only).
# - hostSpec → standard NixOS option pass-through (hostName, locale,
#   timezone, keymap, xkb).
# - root authorizedKeys + hashed-password from hostSpec — the
#   framework's identity contract.
# - the trust contract schema (so `nixfleet.trust.*` typechecks).
#
# Everything else — substituter lists, GC policy, openssh hardening,
# nixpkgs.config opinions, the universal-tools package set, firewall
# policy — is fleet-side. The framework's own runtime (agent, CP,
# microvm-host) doesn't depend on any of it.
{
  config,
  pkgs,
  lib,
  ...
}: let
  hS = config.hostSpec;
in {
  imports = [../../contracts/trust.nix];

  # --- nix: framework prerequisites only ---
  # nixPath empty because the framework is flake-only.
  # experimental-features = "nix-command flakes" required for any
  # flake-based deployment.
  # `package` is mkDefault so distro forks (Lix, Determinate, ...)
  # can swap without mkForce ceremony.
  nix = {
    nixPath = lib.mkDefault [];
    package = lib.mkDefault pkgs.nix;
    extraOptions = ''
      experimental-features = nix-command flakes
    '';
  };

  # --- identity pass-through from hostSpec ---
  networking.hostName = hS.hostName;
  networking.interfaces = lib.mkIf (hS.networking ? interface) {
    "${hS.networking.interface}".useDHCP = lib.mkDefault true;
  };

  time.timeZone = hS.timeZone;
  i18n.defaultLocale = hS.locale;
  console.keyMap = lib.mkDefault hS.keyboardLayout;
  services.xserver.xkb.layout = lib.mkDefault hS.keyboardLayout;

  # --- root identity (framework contract) ---
  # The framework declares `hostSpec.rootSshKeys` and
  # `hostSpec.rootHashedPasswordFile`; this materialises them onto
  # `users.users.root`. Inert if sshd isn't enabled — fleets that
  # don't want sshd just leave the keys ungranted.
  users.users.root = {
    openssh.authorizedKeys.keys = hS.rootSshKeys;
    hashedPasswordFile =
      lib.mkIf (hS.rootHashedPasswordFile != null)
      hS.rootHashedPasswordFile;
  };
}
