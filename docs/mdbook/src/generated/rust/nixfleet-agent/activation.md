# `nixfleet_agent::activation`

Agent-side activation logic.

The CP issues a closure hash via `CheckinResponse.target`; the
agent's job is to install + boot that closure. Per ARCHITECTURE.md
the agent is the *last line of defense* against a misbehaving
substituter or a tampered CP, so activation runs three checks
around `nixos-rebuild switch`:

1. **Pre-realise**: `nix-store --realise <path>` forces nix to
   fetch from the configured substituter (attic) and validate its
   signature *before* we commit to switching. If the closure isn't
   locally available and substituter trust is misconfigured, this
   fails closed — we never call `nixos-rebuild` against an
   unverifiable path. Also catches "closure-proxy returned a
   valid-looking narinfo for a path that doesn't actually exist
   upstream" (the proxy-fallback path is fundamentally less
   audited than direct attic).
2. **Switch**: `nixos-rebuild switch --system <verified-path>`.
   nix's own substituter signature checks fire here too; the
   pre-realise is belt-and-suspenders.
3. **Post-verify**: read `/run/current-system` (resolve symlink),
   compare basename against the expected closure_hash. If they
   differ — switched to the wrong path, or `--system` got rewritten
   somewhere — refuse to confirm and trigger local rollback.

Pre-realise + post-verify together close the property "the agent
either confirms the *exact* closure the CP told it about, or rolls
back" — without trusting the substituter or the CP to be honest
about which path was activated.

On rebuild failure or post-verify mismatch the caller runs
`nixos-rebuild --rollback` to revert to the previous boot
generation. CP-side magic rollback (deadline expiry → 410 on
`/confirm`) is independent and additive.

All commands run as root via the systemd unit (StateDirectory +
no NoNewPrivileges hardening on the agent unit; the agent is a
privileged system manager by design — see the agent module
comment in modules/scopes/nixfleet/_agent.nix).

## Items

### 🔓 `enum ActivationOutcome`

Outcome of an activation attempt. The agent's main loop maps each
variant to a follow-up action: confirm on `Success`, rollback on
either `SwitchFailed` or `VerifyMismatch`, retry-on-next-tick on
`RealiseFailed` (nothing was switched, nothing to roll back).


### 🔓 `fn activate`

Activate `target` via realise → switch → verify.

`tracing` events at every step give operators a grep-friendly
breadcrumb trail without parsing the systemd journal in JSON. The
`target_closure` field is consistent across all three log lines so
`journalctl | grep target_closure=<hash>` follows one activation
end to end.


### 🔒 `fn realise`

`nix-store --realise <path>` — fetch + verify, return the realised
path from stdout. nix-store prints one path per line; we expect
exactly one (we passed exactly one input).


### 🔒 `fn read_current_system_basename`

Read `/run/current-system` as a symlink and return the basename of
its target. The basename is the closure-hash form the wire and the
CP both speak.


### 🔓 `fn rollback`

Local rollback: revert the system profile one generation back and
run the previous closure's `switch-to-configuration switch`.
Used when:
- `activate()` returned a non-success outcome that requires
  rollback (`SwitchFailed`, `VerifyMismatch`).
- The agent's confirm window expired before the CP acknowledged
  the activation (magic rollback, RFC-0003 §4.2).

`nix-env --rollback` flips the system profile symlink to the
previous generation; `/run/current-system/bin/switch-to-
configuration switch` then re-runs the activation script of the
(now previous) closure. Bypasses `nixos-rebuild` entirely — the
new `nixos-rebuild-ng` (Python rewrite shipped in 26.05) tries
to evaluate `<nixpkgs/nixos>` even on `--rollback`, which fails
in the agent's NIX_PATH-less sandbox.

Idempotent — running rollback twice in a row reverts twice. The
caller is expected to invoke this exactly once per failed
activation.


### 🔓 `fn confirm_target`

POST `/v1/agent/confirm` to acknowledge a successful activation.

Per RFC-0003 §4.2 the agent confirms exactly once after a
successful activation. Returns `ConfirmOutcome` so the activation
loop can react:
- `Acknowledged` (204): nothing else to do.
- `Cancelled` (410): CP says the rollout was cancelled or the
  deadline passed — agent runs `nixos-rebuild --rollback`.
- `Other`: logged; the CP-side rollback timer will catch deadline
  expiry independently.


