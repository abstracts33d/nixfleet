# NixOS service module for the NixFleet fleet agent.
#
# Linux-only. Poll-only agent that reads a trust-root declaration from
# /etc/nixfleet/agent/trust.json and talks to the control plane over
# mTLS. Reload model is restart-only (docs/trust-root-flow.md §7.1) —
# nixos-rebuild switch changes the etc entry content, systemd restarts,
# the binary re-reads on startup.
#
# Legacy options (tags, healthChecks, metricsPort, dryRun,
# allowInsecure, cacheUrl, healthInterval) were removed.
# The agent is intentionally minimal; health, metrics, and cache
# concerns live outside the agent binary.
#
# Option declarations are shared with the launchd supervisor in
# `./_agent-options.nix` (imported below) — the wire is identical
# across platforms; only the supervisor differs.
#
# Auto-included by mkHost (disabled by default).
{
  config,
  lib,
  pkgs,
  ...
}: let
  cfg = config.services.nixfleet-agent;
  nixfleet-agent = cfg.package;

  # Materialise config.nixfleet.trust into the proto::TrustConfig
  # JSON shape (crates/nixfleet-proto/src/trust.rs). schemaVersion = 1
  # is required per docs/trust-root-flow.md §7.4 — binaries refuse to
  # start on unknown versions.
  #
  # Shared trust.json payload — see ./_trust-json.nix for shape rationale
  # and the orgRootKey ed25519 promotion that matches proto::TrustConfig.
  trustConfig = import ./_trust-json.nix {trust = config.nixfleet.trust;};
  trustJson = pkgs.writers.writeJSON "trust.json" trustConfig;
in {
  imports = [./_agent-options.nix];

  config = lib.mkMerge [
    (lib.mkIf cfg.enable {
      environment.etc."nixfleet/agent/trust.json".source = trustJson;

      systemd.services.nixfleet-agent = {
        description = "NixFleet Fleet Management Agent";
        wantedBy = ["multi-user.target"];
        after = ["network-online.target" "nix-daemon.service"];
        wants = ["network-online.target"];
        startLimitIntervalSec = 0;

        # Agent shells out to:
        # - `nix-store --realise <path>` (closure-hash verify
        #   pre-switch, fetches via attic + checks substituter sigs)
        # - `nix-env --profile /nix/var/nix/profiles/system --set <path>`
        #   (point system profile at the new closure)
        # - `<store-path>/bin/switch-to-configuration switch` (run the
        #   target closure's own activation script — absolute path, no
        #   PATH dep)
        # - `nix-env --profile … --rollback` + the symmetric
        #   switch-to-configuration on rollback
        #
        # Bypasses `nixos-rebuild` entirely. The agent doesn't need it
        # because the closure is pre-built (CI built + signed it, the
        # CP shipped its hash, the agent just realises and activates).
        # Sidesteps `nixos-rebuild-ng`'s evolving CLI surface — the
        # 26.05 Python rewrite renamed `--system` to `--store-path` and
        # tries to evaluate `<nixpkgs/nixos>` on `--rollback`, both of
        # which broke the agent on lab during the first real dispatch
        # round-trip. switch-to-configuration's contract is stable
        # across NixOS releases.
        path = [config.nix.package pkgs.systemd];

        environment =
          {
            # Nix writes its metadata cache (narinfo lookups, eval cache, etc.)
            # to $XDG_CACHE_HOME (default: ~/.cache). Point it at the agent's
            # StateDirectory so the cache persists on impermanent hosts instead
            # of being wiped on every reboot.
            XDG_CACHE_HOME = "/var/lib/nixfleet/.cache";
          }
          // lib.optionalAttrs (cfg.tags != []) {
            NIXFLEET_TAGS = lib.concatStringsSep "," cfg.tags;
          };

        serviceConfig = {
          Type = "simple";
          ExecStart = lib.concatStringsSep " " (import ./_agent-args.nix {
            inherit lib cfg;
            package = nixfleet-agent;
          });
          Restart = "always";
          RestartSec = 30;
          # StateDirectory creates the path under /var/lib with
          # root:root 0700 by default. Aligning with cfg.stateDir
          # (basename only, since StateDirectory= is relative to
          # /var/lib) keeps the systemd-managed lifecycle while
          # giving the binary the absolute path it expects.
          StateDirectory =
            if lib.hasPrefix "/var/lib/" cfg.stateDir
            then lib.removePrefix "/var/lib/" cfg.stateDir
            else "nixfleet-agent";

          # The agent is a privileged system manager: it runs
          # switch-to-configuration which modifies /boot, /etc, /home, /root,
          # bootloader, kernel, systemd units, etc. Sandboxing blocks these
          # operations (subprocess inherits the agent's namespace).
          # Threat model is equivalent to `sudo nixos-rebuild switch` as a
          # daemon - no sandboxing applied.
          NoNewPrivileges = true;
        };
      };
    })

    # Persistence: contribute the agent's state dir to the
    # framework-level persistence list. The active persistence
    # implementation (impermanence / ZFS rollback / …) reads
    # `nixfleet.persistence.directories` and applies its mechanism;
    # this module just declares the need.
    (lib.mkIf cfg.enable {
      nixfleet.persistence.directories = ["/var/lib/nixfleet"];
    })
  ];
}
