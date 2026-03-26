# Shell wrapper zshrc — used by the portable `nix run .#shell` package.
# Keep in sync with core/_home/zsh.nix (HM version of same config).
# Aliases and functions are sourced from their own files at the end.

# History
HISTSIZE=10000000
SAVEHIST=10000000
HISTFILE=${HISTFILE:-$HOME/.zsh_history}
setopt INC_APPEND_HISTORY HIST_FIND_NO_DUPS HIST_SAVE_NO_DUPS HIST_REDUCE_BLANKS
setopt HIST_IGNORE_SPACE HIST_IGNORE_DUPS HIST_IGNORE_ALL_DUPS
setopt HIST_EXPIRE_DUPS_FIRST EXTENDED_HISTORY SHARE_HISTORY
export HISTIGNORE="pwd:ls:cd"

# Vi mode
bindkey -v

# Environment
export EDITOR=nvim
export VISUAL=nvim
export LANG=en_US.UTF-8
export LC_ALL=$LANG
export GPG_TTY=$(tty)
export BROWSER=firefox
export WORKSPACE=$HOME/Dev
export GITHUB_USERNAME=abstracts33d

# Starship prompt
eval "$(starship init zsh)"

# Zoxide
eval "$(zoxide init zsh)"

# History substring search keybinds
bindkey '^[[A' history-substring-search-up
bindkey '^[OA' history-substring-search-up
bindkey '^[[B' history-substring-search-down
bindkey '^[OB' history-substring-search-down

# Autosuggestion accept
bindkey '\e\r' autosuggest-accept

# Greeting
if [ -z "$TMUX" ]; then
  fastfetch
else
  echo ' Loaded '
fi
