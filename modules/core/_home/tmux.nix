{
  config,
  pkgs,
  ...
}: let
  hS = config.hostSpec;
in {
  home.file.".tmux.conf".source = config.lib.file.mkOutOfStoreSymlink "${hS.home}/.config/tmux/tmux.conf";

  programs.tmux = {
    enable = true;
    shell = "${pkgs.zsh}/bin/zsh";
    sensibleOnTop = false;
    plugins = with pkgs.tmuxPlugins; [
      vim-tmux-navigator
      yank
      prefix-highlight
      better-mouse-mode
      cpu
      {
        plugin = catppuccin;
        extraConfig = ''
          set -g @catppuccin_flavor 'macchiato'
          set -g @catppuccin_window_status_style "rounded"
          set -g @catppuccin_window_default_text "#W"
          set -g @catppuccin_window_current_text " #W"
        '';
      }
      {
        plugin = resurrect;
        extraConfig = ''
          set -g @resurrect-dir '$HOME/.cache/tmux/resurrect'
          set -g @resurrect-capture-pane-contents 'on'
          set -g @resurrect-pane-contents-area 'visible'
        '';
      }
      {
        plugin = continuum;
        extraConfig = ''
          set -g @continuum-restore 'on'
          set -g @continuum-save-interval '5'
        '';
      }
    ];
    terminal = "screen-256color";
    prefix = "C-a";
    keyMode = "vi";
    escapeTime = 1;
    historyLimit = 1000000;
    aggressiveResize = true;
    disableConfirmationPrompt = true;
    newSession = true;
    baseIndex = 1;
    mouse = true;
    focusEvents = true;
    extraConfig = ''
      set -g status-position top
      set -g status-right-length 100
      set -g status-left-length 100
      set -g status-left ""
      set -g status-right "#{E:@catppuccin_status_application}"
      set -agF status-right "#{E:@catppuccin_status_cpu}"
      set -ag status-right "#{E:@catppuccin_status_session}"
      set -ag status-right "#{E:@catppuccin_status_uptime}"
      bind C-l send-keys 'C-l'
      unbind r
      bind r source-file ~/.config/tmux/tmux.conf \; display-message "~/.config/tmux/tmux.conf reloaded"
      bind-key -n Home send Escape "OH"
      bind-key -n End send Escape "OF"
      unbind %
      unbind '"'
      bind s split-window -h -c "#{pane_current_path}"
      bind v split-window -v -c "#{pane_current_path}"
      bind -r H resize-pane -L 5
      bind -r J resize-pane -D 5
      bind -r K resize-pane -U 5
      bind -r L resize-pane -R 5
    '';
  };
}
