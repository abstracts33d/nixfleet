# Git login config — user/email/signing needs ~/.gitconfig (wrapper flags alone aren't enough).
# Core settings match wrappers/git.nix for consistency.
{
  config,
  lib,
  ...
}: let
  hS = config.hostSpec;
in {
  programs.git = {
    enable = true;
    ignores = ["*.swp"];
    signing = lib.mkIf (hS.gpgSigningKey != null) {
      format = lib.mkDefault "openpgp";
      signByDefault = lib.mkDefault true;
      key = hS.gpgSigningKey;
    };
    lfs.enable = true;
    settings = {
      user = {
        name = hS.githubUser;
        email = hS.githubEmail;
      };
      init.defaultBranch = "main";
      core = {
        editor = "nvim";
        autocrlf = "input";
      };
      color.ui = true;
      pull.rebase = true;
      push.autoSetupRemote = true;
      rebase.autoStash = true;
    };
  };
}
