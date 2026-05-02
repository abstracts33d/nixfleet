# Darwin service module for the NixFleet fleet agent.
#
# Auto-included by mkHost for Darwin hosts (disabled by default).
# Wires `services.nixfleet-agent` to a launchd system daemon — the
# darwin equivalent of `_agent.nix`'s systemd unit. The agent binary
# itself is the same; only the supervisor differs.
#
# Mirror of `_agent.nix`'s option tree. Shape stays identical so a
# fleet's `services.nixfleet-agent.<...>` settings work unchanged
# regardless of host platform.
#
# v0.1 hacks preserved (each closes a real bug; do not remove
# without re-validating the failure mode):
#
# 1. **15s sleep wrapper** in ProgramArguments. At boot, launchd
#    starts the agent before NTP syncs the system clock and before
#    agenix decrypts the mTLS cert files. Without the sleep the
#    agent races both, gets a "cert not yet valid" error from rustls
#    or "file not found" from the cert path, KeepAlive restarts it,
#    rinse and repeat. 15s is generous; closes the race in practice.
#
# 2. **postActivation `launchctl kickstart`** — nix-darwin's launchd
#    management only reloads services whose plist actually changed.
#    When the binary path inside the plist is unchanged but the
#    closure changed (or environment.etc files changed), the agent
#    process can be killed by activation's mid-run service teardown
#    without launchd reliably restarting it. `launchctl kickstart -k`
#    forces a clean restart at the end of every activation —
#    idempotent, harmless when the agent is already running.
#
# 3. **EnvironmentVariables.PATH** — launchd daemons inherit a
#    minimal PATH (`/usr/bin:/bin:/usr/sbin:/sbin`). The agent
#    shells out to `nix-store --realise`, `nix-env --set/--rollback`,
#    plus the closure's own `activate`/`activate-user` scripts; all
#    need nix on PATH. Determinate Nix installs to
#    `/nix/var/nix/profiles/default/bin`, standard nix-darwin to
#    `/run/current-system/sw/bin` — both are listed.
#
# 4. **WorkingDirectory** must exist BEFORE the daemon starts or
#    launchd returns I/O error (exit 5). The preActivation script
#    creates `/var/lib/nixfleet` so the daemon's WorkingDirectory
#    is valid on first boot.
#
# 5. **No `--health-config`/`--health-interval`/`--metrics-port`/
#    `--db-path`/`--retry-interval`/`--cache-url`/`--dry-run`/
#    `--allow-insecure`** — these v0.1 flags were removed.
#    The v0.2 agent surface is intentionally narrower; health,
#    metrics, and cache concerns live outside the agent binary now.
{
  config,
  lib,
  pkgs,
  ...
}: let
  cfg = config.services.nixfleet-agent;
  nixfleet-agent = cfg.package;

  # Materialise config.nixfleet.trust into proto::TrustConfig JSON
  # shape — same shape as the NixOS module so the wire is identical.
  # Schema version 1 is required (docs/trust-root-flow.md §7.4).
  trustConfig = import ./_trust-json.nix {trust = config.nixfleet.trust;};
  trustJson = pkgs.writers.writeJSON "trust.json" trustConfig;
in {
  imports = [./_agent-options.nix];

  # Darwin-only options. Shared options live in `./_agent-options.nix`.
  options.services.nixfleet-agent = {
    sshHostKeyFile = lib.mkOption {
      type = lib.types.str;
      default = "/etc/ssh/ssh_host_ed25519_key";
      description = ''
        Host SSH ed25519 private key, used to sign ComplianceFailure
        / RuntimeGateError event payloads (root-3).
        Default matches OpenSSH's stock path on darwin (sshd is
        managed by `services.openssh` in nix-darwin or pre-existing
        on macOS hosts).
      '';
    };

    tags = lib.mkOption {
      type = lib.types.listOf lib.types.str;
      default = [];
      description = "Tags reported with each checkin.";
    };
  };

  config = lib.mkIf cfg.enable {
    # Materialise the trust JSON. Same shape as NixOS — agent reads
    # it identically. `.text` (not `.source`) so the file content
    # ships in the system closure rather than as a symlink to a
    # flake source path that may not be present on the deployed
    # host. See docs/mdbook/reference/darwin-platform-notes.md §
    # "environment.etc: .text vs .source".
    environment.etc."nixfleet/agent/trust.json".text = builtins.readFile trustJson;

    # Ensure state + cache + log directories exist before launchd
    # tries to start the agent. nix-darwin uses preActivation /
    # postActivation, not arbitrary named scripts like NixOS.
    system.activationScripts.preActivation.text = ''
      mkdir -p /var/lib/nixfleet
      mkdir -p ${lib.escapeShellArg cfg.stateDir}
      chmod 0700 ${lib.escapeShellArg cfg.stateDir}
      # Activate-script log file used by the agent's setsid-detached
      # `<store>/activate` invocation (see crates/nixfleet-agent/
      # src/activation.rs::fire_switch_darwin). Touched here so the
      # OpenOptions(append) in attach_activate_log can succeed on
      # first boot.
      install -m 0644 /dev/null /var/log/nixfleet-activate.log 2>/dev/null || true
    '';

    # Force-restart the agent after every activation. Defends against
    # the case where the plist contents are unchanged but the binary's
    # closure has shifted, or environment.etc files changed without
    # bumping the plist hash. launchd's KeepAlive only reliably
    # restarts on a clean exit; mid-run kills during activation can
    # leave the daemon in a transient state where KeepAlive doesn't
    # fire. `kickstart -k` forces a clean restart and is idempotent
    # against an already-running daemon.
    system.activationScripts.postActivation.text = lib.mkAfter ''
      echo "restarting nixfleet agent..." >&2
      launchctl kickstart -k system/com.nixfleet.agent 2>/dev/null || true
    '';

    launchd.daemons.nixfleet-agent = {
      serviceConfig = {
        Label = "com.nixfleet.agent";

        # Wrapped in `sh -c "sleep 15 && exec ${args}"`. The sleep
        # gives NTP time to sync (otherwise rustls rejects the CP's
        # cert with "not yet valid") and agenix time to decrypt
        # mTLS cert files. The `exec` replaces sh with the agent so
        # launchd tracks the agent PID directly (KeepAlive sees the
        # right PID).
        ProgramArguments = let
          args = lib.concatStringsSep " " (
            (import ./_agent-args.nix {
              inherit lib cfg;
              package = nixfleet-agent;
            })
            ++ [
              "--ssh-host-key-file"
              (lib.escapeShellArg cfg.sshHostKeyFile)
            ]
          );
        in ["/bin/sh" "-c" "sleep 15 && exec ${args}"];

        # KeepAlive: restart on exit (matches systemd Restart=always).
        # RunAtLoad: start at boot.
        # ThrottleInterval: minimum seconds between restart attempts
        #   (matches systemd RestartSec=30 in spirit; 10s here matches
        #   v0.1 to keep first-boot recovery snappy).
        # ExitTimeOut: seconds to wait for graceful exit before SIGKILL.
        KeepAlive = true;
        RunAtLoad = true;
        ThrottleInterval = 10;
        ExitTimeOut = 10;

        StandardOutPath = "/var/log/nixfleet-agent.log";
        StandardErrorPath = "/var/log/nixfleet-agent.log";

        # Must exist before daemon launches (preActivation creates it).
        WorkingDirectory = "/var/lib/nixfleet";

        EnvironmentVariables =
          {
            # Nix metadata cache → state dir so impermanent darwin
            # hosts (future) don't lose narinfo lookups on reboot.
            XDG_CACHE_HOME = "/var/lib/nixfleet/.cache";
            # Launchd minimal PATH doesn't include nix paths. Cover
            # both Determinate Nix (default profile) and standard
            # nix-darwin (current-system profile).
            PATH = "/nix/var/nix/profiles/default/bin:/run/current-system/sw/bin:/usr/bin:/bin";
          }
          // lib.optionalAttrs (cfg.tags != []) {
            NIXFLEET_TAGS = lib.concatStringsSep "," cfg.tags;
          };
      };
    };
  };
}
