{...}: {
  flake.modules.homeManager.karabiner = {
    config,
    lib,
    ...
  }: let
    hS = config.hostSpec;
  in {
    config = lib.mkIf hS.isDarwin {
      home.file."${hS.home}/.config/karabiner/karabiner.json".text = builtins.readFile ../../_config/karabiner.json;
    };
  };
}
