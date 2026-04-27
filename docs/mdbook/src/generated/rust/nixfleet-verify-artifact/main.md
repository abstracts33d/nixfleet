# `nixfleet_verify_artifact`

`nixfleet-verify-artifact` — thin CLI wrapping
`nixfleet_reconciler::verify_artifact`.

Harness scaffold per `docs/phase-2-entry-spec.md §6`. Exists purely so
the Phase 2 signed-roundtrip scenario can call `verify_artifact` from
a shell-friendly entry point before Stream C's v0.2 agent takes over
the same call site. Retire this crate once the agent inlines
`verify_artifact` internally.

Exit codes (per spec §6):
- 0 — artifact verified
- 1 — verify error (stderr carries the `VerifyError` variant + detail)
- 2 — argument / I/O / parse error

