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
  "--trust-file"
  (lib.escapeShellArg (toString cfg.trustFile))
  "--manifest-freshness-window-secs"
  (toString cfg.manifestFreshnessWindowSecs)
]
++ lib.optionals (cfg.renewalThresholdFraction != null) [
  "--renewal-threshold-fraction"
  (toString cfg.renewalThresholdFraction)
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
  "--ssh-host-key-file"
  (lib.escapeShellArg cfg.sshHostKeyFile)
]
