{...}: {
  flake.modules.nixos.bluetooth = {
    config,
    lib,
    ...
  }: let
    hS = config.hostSpec;
  in {
    config = lib.mkIf hS.hasBluetooth {
      hardware.bluetooth = {
        enable = true;
        powerOnBoot = true;
      };
      services.blueman.enable = true;
    };
  };
}
