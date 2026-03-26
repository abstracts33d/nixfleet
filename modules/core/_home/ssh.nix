{config, ...}: let
  hS = config.hostSpec;
in {
  programs.ssh = {
    enable = true;
    enableDefaultConfig = false;
    includes = ["${hS.home}/.ssh/config_external"];
    matchBlocks = {
      "*".addKeysToAgent = "yes";
      "github.com" = {
        identitiesOnly = true;
        identityFile = ["${hS.home}/.ssh/id_ed25519"];
      };
    };
  };
}
