# Platform abstractions for mkVmApps.
#
# Returns the values consumed by lib/vm-scripts/*.nix:
#
#   system / isLinux / isDarwin   — host platform identity
#   lib                            — pkgs.lib (cached)
#   mkScript                       — name -> description -> bash -> flake app attr
#   nixos-anywhere-bin             — absolute path or a stub `exit 1` on unsupported
#   qemuBin / qemuAccel            — `qemu-system-*` binary name + accel flag
#   qemuFirmware                   — OVMF/AAVMF firmware path (lazy: see note)
#   basePkgs / spicePkgs           — pkg sets for makeBinPath
#   sharedHelpers                  — bash helper library (read from vm-helpers.sh)
#
# qemuFirmware references `pkgs.OVMF.fd`, which is marked broken on
# aarch64-darwin. Callers must keep this attribute lazy by gating
# evaluation on `isLinux`; mk-vm-apps.nix does that via
# `lib.optionalAttrs platform.isLinux`.
{inputs}: {pkgs}: let
  system = pkgs.stdenv.hostPlatform.system;
  isLinux = builtins.elem system ["x86_64-linux" "aarch64-linux"];
  isDarwin = builtins.elem system ["aarch64-darwin" "x86_64-darwin"];
  lib = pkgs.lib;

  mkScript = name: description: text: {
    type = "app";
    program = "${pkgs.writeShellScriptBin name text}/bin/${name}";
    meta.description = description;
  };

  nixos-anywhere-bin =
    if inputs.nixos-anywhere.packages ? ${system}
    then "${inputs.nixos-anywhere.packages.${system}.default}/bin/nixos-anywhere"
    else "echo 'nixos-anywhere not available on ${system}'; exit 1";

  qemuBin =
    {
      "x86_64-linux" = "qemu-system-x86_64";
      "aarch64-linux" = "qemu-system-aarch64";
      "aarch64-darwin" = "qemu-system-aarch64";
      "x86_64-darwin" = "qemu-system-x86_64";
    }.${
      system
    } or (throw "unsupported system: ${system}");

  qemuAccel =
    if isLinux
    then "-enable-kvm"
    else if isDarwin
    then "-accel hvf"
    else throw "unsupported system: ${system}";

  qemuFirmware = let
    isAarch64 = builtins.elem system ["aarch64-linux" "aarch64-darwin"];
  in
    if isAarch64
    then "${pkgs.OVMF.fd}/FV/AAVMF_CODE.fd"
    else "${pkgs.OVMF.fd}/FV/OVMF.fd";

  basePkgs = with pkgs; [qemu coreutils openssh nix git];
  spicePkgs = with pkgs; [virt-viewer];
in {
  inherit
    system
    isLinux
    isDarwin
    lib
    mkScript
    nixos-anywhere-bin
    qemuBin
    qemuAccel
    qemuFirmware
    basePkgs
    spicePkgs
    ;
  sharedHelpers = builtins.readFile ./vm-helpers.sh;
}
