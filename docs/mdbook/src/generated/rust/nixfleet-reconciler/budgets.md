# `nixfleet_reconciler::budgets`

Disruption budget evaluation (RFC-0002 §4.2).

## Items

### 🔐 `fn in_flight_count`

Count hosts currently in-flight across all active rollouts.


### 🔐 `fn budget_max`

For a given host, return the tightest (in_flight, max_in_flight) across
all budgets that include the host.


