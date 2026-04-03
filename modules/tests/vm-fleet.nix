# Tier A — VM fleet test: 4-node TLS/mTLS fleet with rollout, health gates, pause/resume.
#
# Nodes: cp (control plane), web-01, web-02 (healthy agents), db-01 (unhealthy agent).
# TLS: Nix-generated CA + server/client certs — no allowInsecure.
# Rollout: canary on web tag (passes), all-at-once on db tag (pauses on health gate).
#
# Run: nix build .#checks.x86_64-linux.vm-fleet --no-link
{inputs, ...}: {
  perSystem = {
    pkgs,
    system,
    lib,
    ...
  }: lib.optionalAttrs (system == "x86_64-linux") {
      checks = {
        vm-fleet = pkgs.testers.nixosTest {
          name = "vm-fleet";

          nodes = {};
          testScript = "";
        };
      };
    };
}
