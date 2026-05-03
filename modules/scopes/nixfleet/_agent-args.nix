# Shared CLI-argument assembly for `nixfleet-agent`.
#
# Both supervisor modules — `_agent.nix` (NixOS / systemd) and
# `_agent-darwin.nix` (nix-darwin / launchd) — invoke the same agent
# binary with the same flags except for two platform-specific bits:
# darwin appends `--ssh-host-key-file` and wraps the whole thing in
# `/bin/sh -c "sleep 15 && exec …"` for boot-race resilience.
#
# This helper returns the platform-neutral args as a list; each
# supervisor concatenates whatever it needs on top before joining.
# Single source of truth — protects against per-platform flag drift
# (the prior shape had identical arg-assembly duplicated across both
# modules, which is exactly the audit-A-#6 maintenance trap).
{
  lib,
  cfg,
  package,
}:
  [
    "${package}/bin/nixfleet-agent"
    "--control-plane-url"
    (lib.escapeShellArg cfg.controlPlaneUrl)
    "--machine-id"
    (lib.escapeShellArg cfg.machineId)
    "--poll-interval"
    (toString cfg.pollInterval)
    "--trust-file"
    (lib.escapeShellArg (toString cfg.trustFile))
  ]
  ++ lib.optionals (cfg.tls.caCert != null) [
    "--ca-cert"
    (lib.escapeShellArg cfg.tls.caCert)
  ]
  ++ lib.optionals (cfg.tls.clientCert != null) [
    "--client-cert"
    (lib.escapeShellArg cfg.tls.clientCert)
  ]
  ++ lib.optionals (cfg.tls.clientKey != null) [
    "--client-key"
    (lib.escapeShellArg cfg.tls.clientKey)
  ]
  ++ lib.optionals (cfg.bootstrapTokenFile != null) [
    "--bootstrap-token-file"
    (lib.escapeShellArg cfg.bootstrapTokenFile)
  ]
  ++ [
    "--state-dir"
    (lib.escapeShellArg cfg.stateDir)
    "--compliance-gate-mode"
    (lib.escapeShellArg cfg.complianceGate.mode)
    "--ssh-host-key-file"
    (lib.escapeShellArg cfg.sshHostKeyFile)
  ]
