# `nixfleet_canonicalize`

`nixfleet-canonicalize` — stdin JSON → JCS canonical stdout.

Shell-invocable canonicalizer for Stream A's CI signing
pipeline. Exit codes:
- 0 — canonical bytes written to stdout
- 1 — input was not valid JSON or canonicalization failed
- 2 — I/O error reading stdin or writing stdout

