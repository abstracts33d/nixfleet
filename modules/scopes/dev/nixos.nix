{...}: {
  flake.modules.nixos.dev = {
    config,
    pkgs,
    lib,
    ...
  }: let
    hS = config.hostSpec;
  in {
    config = lib.mkIf hS.isDev {
      environment.systemPackages = with pkgs; [
        postgresql
        vscode
      ];

      # --- Docker ---
      virtualisation.docker = {
        enable = true;
        logDriver = "json-file";
      };

      # --- Impermanence: system-level dev persist paths ---
      environment.persistence."/persist/system".directories = lib.mkIf hS.isImpermanent [
        "/var/lib/docker"
        "/var/lib/postgresql"
      ];
    };
  };

  flake.modules.homeManager.devPersistence = {
    lib,
    osConfig,
    ...
  }: let
    hS = osConfig.hostSpec;
  in {
    config = lib.mkIf (hS.isDev && hS.isImpermanent) (lib.optionalAttrs (!hS.isDarwin) {
      home.persistence."/persist".directories = [
        ".docker"
        ".npm"
        ".cargo"
        ".cache/pip"
        ".cache/yarn"
        ".local/share/mise"
        ".cache/mise"
        ".cache/direnv"
        ".local/share/direnv"
        ".config/pgcli"
        ".claude"
      ];
    });
  };
}
