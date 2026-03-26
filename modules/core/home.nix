{...}: {
  flake.modules.homeManager.core = {
    imports = [
      ./_home/zsh.nix
      ./_home/git.nix
      ./_home/starship.nix
      ./_home/ssh.nix
      ./_home/keys.nix
      ./_home/neovim.nix
      ./_home/tmux.nix
      ./_home/simple.nix
      ./_home/gpg.nix
    ];
  };
}
