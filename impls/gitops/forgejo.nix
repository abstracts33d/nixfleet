# Forgejo (and Gitea, same API) raw-URL builder for the channel-refs
# source. Produces the artifact + signature URLs the framework's
# `services.nixfleet-control-plane.channelRefsSource` consumes.
#
# Pure data — not a NixOS module. Fleets use it by `let cfg =
# inputs.nixfleet.scopes.gitops.forgejo.urlsFor { ... }; in
# services.nixfleet-control-plane.channelRefsSource = {
#   artifactUrl = cfg.artifactUrl;
#   signatureUrl = cfg.signatureUrl;
#   tokenFile = config.age.secrets.cp-channel-refs-token.path;
# };`
{
  # Build {artifactUrl, signatureUrl} for a Forgejo / Gitea host.
  #
  # baseUrl: scheme + host, no trailing slash. e.g. "https://git.example.com"
  # owner:   repo owner / org. e.g. "myorg"
  # repo:    repo name. e.g. "myfleet".
  # ref:     branch or tag. Default "main".
  # path:    repo-relative path to the artifact JSON. Default
  #          "releases/fleet.resolved.json"; the matching ".sig" is
  #          derived automatically.
  urlsFor = {
    baseUrl,
    owner,
    repo,
    ref ? "main",
    path ? "releases/fleet.resolved.json",
  }: let
    base = "${baseUrl}/${owner}/${repo}/raw/branch/${ref}/${path}";
  in {
    artifactUrl = base;
    signatureUrl = "${base}.sig";
  };
}
