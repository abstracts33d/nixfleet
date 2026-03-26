# Portable dev environment — `nix run .#shell` from any machine.
# Bundles zsh with all configured CLI tools AND their configs in PATH.
# Config files are sourced from _config/ (same source as HM) — DRY.
# Plugins match HM zsh.nix: fzf-tab, vi-mode, history-substring-search,
# syntax-highlighting, autosuggestions.
# For LOCAL NixOS/Darwin, HM manages the login shell (core/_home/zsh.nix).
{inputs, ...}: let
  wlib = inputs.wrapper-modules.lib;
  shellWrapper = wlib.wrapModule ({
    pkgs,
    config,
    ...
  }: {
    package = pkgs.zsh;

    extraPackages = with pkgs; [
      # Editors
      neovim
      helix

      # Shell tools
      starship
      tmux
      bat
      btop
      zellij
      fastfetch

      # File management
      eza
      fd
      fzf
      yazi
      tree
      jq
      yq
      ripgrep

      # Git
      git
      gh

      # Network
      curl
      wget
      zoxide

      # Zsh plugins (same as HM zsh.nix zplug/syntaxHighlighting/autosuggestion)
      zsh-fzf-tab
      zsh-vi-mode
      zsh-history-substring-search
      zsh-syntax-highlighting
      zsh-autosuggestions
    ];

    # --- Bundled config files (from _config/, same source as HM) ---
    constructFiles = {
      zshrc = {
        content = builtins.concatStringsSep "\n" [
          (builtins.readFile ../_config/zsh/wrapperrc.zsh)

          # Plugin sourcing (matches HM programs.zsh.zplug/syntaxHighlighting/autosuggestion)
          ''
            # Plugins — sourced from Nix store (same plugins as HM zsh.nix)
            source ${pkgs.zsh-fzf-tab}/share/fzf-tab/fzf-tab.plugin.zsh
            source ${pkgs.zsh-vi-mode}/share/zsh-vi-mode/zsh-vi-mode.plugin.zsh
            source ${pkgs.zsh-history-substring-search}/share/zsh-history-substring-search/zsh-history-substring-search.zsh
            source ${pkgs.zsh-syntax-highlighting}/share/zsh-syntax-highlighting/zsh-syntax-highlighting.zsh
            source ${pkgs.zsh-autosuggestions}/share/zsh-autosuggestions/zsh-autosuggestions.zsh
          ''

          (builtins.readFile ../_config/zsh/aliases.zsh)
          (builtins.readFile ../_config/zsh/functions.zsh)
        ];
        relPath = ".zshrc";
      };
      starship_config = {
        content = builtins.readFile ../_config/starship.toml;
        relPath = "starship.toml";
      };
      gitconfig = {
        content = builtins.readFile ../_config/gitconfig;
        relPath = "gitconfig";
      };
    };

    # --- Environment variables pointing tools to their bundled configs ---
    env = {
      STARSHIP_CONFIG = config.constructFiles.starship_config.path;
      GIT_CONFIG_GLOBAL = config.constructFiles.gitconfig.path;
      BAT_STYLE = "changes,header";
      # ZDOTDIR makes zsh read our bundled .zshrc
      ZDOTDIR = "${pkgs.writeTextDir ".zshrc" config.constructFiles.zshrc.content}";
    };

    flags."-i" = true;
  });
in {
  perSystem = {pkgs, ...}: {
    packages.shell = shellWrapper.wrap {inherit pkgs;};
  };
}
