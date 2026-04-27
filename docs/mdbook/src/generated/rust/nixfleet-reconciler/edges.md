# `nixfleet_reconciler::edges`

Edge predecessor ordering check (RFC-0002 §4.1).

## Items

### 🔐 `fn predecessor_blocking`

If `host`'s in-wave predecessors are NOT all Soaked/Converged, return
the name of the first incomplete predecessor. Otherwise `None`.


