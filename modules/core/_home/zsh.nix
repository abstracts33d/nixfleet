# Login shell configuration — sources from _config/ (same source as wrappers/shell.nix).
# This is needed because the login shell is the system zsh, not the wrapped one.
{
  config,
  lib,
  ...
}: let
  hS = config.hostSpec;
in {
  home.file = {
    "${config.xdg.configHome}/zsh/functions.zsh".text = builtins.readFile ../../_config/zsh/functions.zsh;
    "${config.xdg.configHome}/zsh/aliases.zsh".text = builtins.readFile ../../_config/zsh/aliases.zsh;
  };

  programs.zsh = {
    enable = true;
    autocd = true;
    enableCompletion = true;
    syntaxHighlighting.enable = true;
    autosuggestion.enable = true;
    history = {
      size = 10000000;
      save = 10000000;
      ignoreSpace = true;
      ignoreDups = true;
      ignoreAllDups = true;
      expireDuplicatesFirst = true;
      extended = true;
      share = true;
    };
    # sessionVariables is attrsOf str — mkDefault doesn't work here.
    # Org overrides these by setting home.sessionVariables in their hmModules.
    sessionVariables = {
      LANG = hS.locale;
      LC_ALL = hS.locale;
      GPG_TTY = "$(tty)";
      EDITOR = "nvim";
      VISUAL = "nvim";
      BROWSER = "firefox";
      WORKSPACE = "$HOME/Dev";
      GITHUB_USERNAME = hS.githubUser;
    };
    profileExtra = ''
      setopt INC_APPEND_HISTORY
      setopt HIST_FIND_NO_DUPS
      setopt HIST_SAVE_NO_DUPS
      setopt HIST_REDUCE_BLANKS
    '';
    zplug = {
      enable = true;
      plugins = [
        {name = "Aloxaf/fzf-tab";}
        {name = "jeffreytse/zsh-vi-mode";}
        {
          name = "zsh-users/zsh-history-substring-search";
          tags = ["as:plugin"];
        }
      ];
    };
    initContent = let
      zshConfigEarlyInit = lib.mkBefore ''
        if [[ -f /nix/var/nix/profiles/default/etc/profile.d/nix-daemon.sh ]]; then
          . /nix/var/nix/profiles/default/etc/profile.d/nix-daemon.sh
          . /nix/var/nix/profiles/default/etc/profile.d/nix.sh
        fi
        export HISTIGNORE="pwd:ls:cd"
        bindkey -v
        source ~/.config/zsh/aliases.zsh
        source ~/.config/zsh/functions.zsh
      '';
      zshConfig = lib.mkOrder 1000 ''
        bindkey '^[[A' history-substring-search-up
        bindkey '^[OA' history-substring-search-up
        bindkey '^[[B' history-substring-search-down
        bindkey '^[OB' history-substring-search-down
        bindkey '\e\r' autosuggest-accept
        if [ -z "$TMUX" ]; then
          fastfetch
        else
          echo ' Loaded '
        fi
      '';
    in
      lib.mkMerge [zshConfigEarlyInit zshConfig];
  };
}
