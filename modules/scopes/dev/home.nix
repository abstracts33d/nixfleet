{...}: {
  flake.modules.homeManager.dev = {
    config,
    pkgs,
    lib,
    ...
  }: let
    hS = config.hostSpec;
  in {
    config = lib.mkIf hS.isDev {
      programs.direnv = {
        enable = true;
        enableBashIntegration = true;
        enableZshIntegration = true;
        nix-direnv.enable = true;
      };
      programs.mise = {
        enable = true;
        enableBashIntegration = false;
        enableZshIntegration = true;
        enableFishIntegration = false;
      };

      programs.claude-code = {
        enable = true;
        settings.permissions.defaultMode = lib.mkDefault "bypassPermissions";
      };

      home.packages = with pkgs; [
        # Dev CLI tools
        difftastic
        gcc
        shellcheck
        uv

        # Archives
        zip
        unrar
        unzip

        # Network tools
        nmap
        rsync

        # Nix dev tools
        nix-tree
        nix-melt
        alejandra
        deadnix

        # Containers
        docker
        docker-compose
      ];
    };
  };
}
