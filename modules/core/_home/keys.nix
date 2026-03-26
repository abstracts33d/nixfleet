{...}: {
  home.file = {
    ".ssh/id_ed25519.pub" = {
      text = builtins.readFile ../../_config/githubPublicKey;
      force = true;
    };
    ".ssh/pgp_github.pub" = {
      text = builtins.readFile ../../_config/githubPublicSigningKey;
      force = true;
    };
  };
}
