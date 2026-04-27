# `nixfleet_agent::checkin_state`

System introspection for checkin body assembly.

Reads what the agent reports about itself: closure hash, pending
generation, boot ID. All file I/O is `std::fs::*` — these are
tiny reads of /run + /proc, no async needed.

## Items

### 🔒 `const CURRENT_SYSTEM`

Path to the symlink pointing at the currently active system
closure. Reading it as a symlink target gives us the store
path; the basename of that path IS the closure_hash on the
wire (the same shape the CP populates into `EvaluatedTarget.
closure_hash` and `fleet.resolved.hosts[h].closureHash`). The
agent must report the FULL basename, not the 32-char hash
prefix — `dispatch::decide_target` does string-equality on the
two values; a hash-prefix-vs-full-basename mismatch means
dispatch always returns Decision::Dispatch even when the host
is on the declared closure.


### 🔒 `const BOOTED_SYSTEM`

Path to the symlink pointing at the system that booted. When
this differs from `/run/current-system`, the host has a pending
generation queued for next reboot.


### 🔒 `const BOOT_ID_PATH`

Linux's per-boot UUID. Stable for a single boot; rotates on
reboot. Used by the CP to detect that a host actually rebooted
(e.g. correlated with `pendingGeneration` clearing on next
checkin).


### 🔓 `fn current_closure_hash`

Read `/run/current-system`'s symlink target and extract the
store-path closure hash (the 32-char nix-store hash before the
`-` separator). Returns the full store path on platforms where
the symlink target shape doesn't match the expected pattern, so
the agent still reports something rather than failing the
checkin.


### 🔒 `fn booted_closure_hash`

Same as [`current_closure_hash`] for `/run/booted-system`. The
caller compares the two to decide whether to populate
`pendingGeneration`.


### 🔒 `fn closure_hash_from_path`

Extract the closure-hash identifier from a `/nix/store/<basename>`
path. Returns the full basename (e.g.
`2zlnf66xlf35xwm7150kx05q93cwp8jk-nixos-system-lab-…`), NOT the
32-char hash prefix. The basename is the wire identifier shared
across the proto: `EvaluatedTarget.closure_hash` (CP → agent),
`fleet.resolved.hosts[h].closureHash` (CI → CP), and
`CheckinRequest.current_generation.closure_hash` (agent → CP)
all carry it in the same shape. `dispatch::decide_target` does
string-equality between them; any normalisation drift here
means converged hosts look diverged forever.

Falls back to the full path string if the shape doesn't match,
so the field is always populated.


### 🔓 `fn boot_id`

Read `/proc/sys/kernel/random/boot_id`. The file is a single
UUID + newline; we trim and return.


### 🔓 `fn current_generation_ref`

Build the `currentGeneration` GenerationRef. `channel_ref` is
always `None` in PR-3 — the agent doesn't know its channel until
PR-4 wires the projection.


### 🔓 `fn pending_generation`

Build the `pendingGeneration` PendingGeneration when
`/run/booted-system` differs from `/run/current-system`. Returns
`Ok(None)` when they match (no pending), `Err` only on read
failures of either symlink.


### 🔓 `fn uptime_secs`

Wall-clock seconds since the agent process started. The caller
passes the start `Instant` (captured in `main` before the poll
loop starts).


