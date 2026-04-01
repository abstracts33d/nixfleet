# NixFleet Demo — Cheat Sheet

**Duration:** 20-25 min + Q&A
**Audience:** Co-founders, investors, technical partners, prospective clients
**Setup:** Terminal 18pt+ dark theme, 3 tabs minimum

---

## Pre-Demo Checklist

```sh
# Run the preparation script (builds and caches everything)
bash demo/pre-demo.sh

# Terminal setup:
#   Tab 1: demo commands (main)
#   Tab 2: control plane (starts during Act 5)
#   Tab 3: agent (starts during Act 5)
```

---

## Act 0: The Problem (2 min) — Talking Points Only

**No terminal.** This is the business framing. Set the context before showing anything.

### Three crises

1. **Reproducibility crisis** — "Your 200 servers were supposed to be identical. After 18 months of patches, hotfixes, and manual tweaks, no two are the same. Your Ansible playbooks describe what should happen — but the result depends on what happened before. This is called configuration drift, and it's inevitable with current tools."

2. **Sovereignty crisis** — "Your fleet management lives in someone else's cloud. Jamf, Intune, AWS Systems Manager — your ability to deploy depends on their availability. Your data is subject to the US Cloud Act. If they change their pricing or shut down, you're stuck."

3. **Regulatory crisis** — "NIS2 hits 15,000 French entities by end of 2027. Fines up to 10 million euros. Personal liability for executives. The directive demands traceability, rapid recovery, supply chain security. With current tools, meeting each obligation costs 30-80k euros per year in separate tooling. Most SMEs can't afford that."

### Transition

> "What if these three problems had a single architectural answer? That's what we're going to show you."

---

## Act 1: Nix in 5 Minutes (4 min) — Hands-On Introduction

**Goal:** Show that Nix is real, mature, and immediately useful — not an academic curiosity.

### 1.1 — Run any package from the internet, instantly

```sh
# No install needed. This fetches cowsay from nixpkgs and runs it.
nix run nixpkgs#cowsay -- "Infrastructure as a pure function"
```

> "100,000+ packages in nixpkgs — the largest package repository in the world. Bigger than Debian, Homebrew, or any other. You can run any of them without installing anything."

```sh
# Another example: a specific version of Python, isolated, no conflicts
nix run nixpkgs#python3 -- --version
```

### 1.2 — A reproducible development environment

```sh
# Enter the NixFleet dev shell — all dependencies declared, reproducible
nix develop
```

> "Every developer on the team gets the exact same tools, same versions, same configuration. No 'works on my machine'. A new hire runs `nix develop` and is ready in 60 seconds."

```sh
# Show what's in the shell
which cargo && cargo --version
which alejandra && alejandra --version
```

### 1.3 — What a flake looks like

```sh
# The simplest possible Nix flake — from the official documentation
cat <<'FLAKE'
{
  description = "A very basic flake";

  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs?ref=nixos-unstable";
  };

  outputs = { self, nixpkgs }: {
    packages.x86_64-linux.hello = nixpkgs.legacyPackages.x86_64-linux.hello;
    packages.x86_64-linux.default = self.packages.x86_64-linux.hello;
  };
}
FLAKE
```

> "A flake is a pure function. Inputs on the left — here just nixpkgs. Outputs on the right — here a single package. The beauty: `flake.lock` pins every input to an exact cryptographic hash. Same lock file = same result, always, everywhere, on any machine."

```sh
# Show NixFleet's own lock file — every dependency pinned by hash
jq '.nodes.nixpkgs.locked' flake.lock
```

### 1.4 — NixOS: an entire OS as a function

```sh
# This is a complete NixOS host definition — 10 lines
cat examples/standalone-host/flake.nix
```

> "One function — `mkHost` — takes a hostname, a platform, and some flags. It returns a complete NixOS system: kernel, services, users, network, disk layout. Deploy with `nixos-anywhere`, update with `nixos-rebuild`. Standard NixOS tooling — no custom scripts."

### 1.5 — Atomic rebuilds and instant rollback

> "When NixOS rebuilds, it creates a new generation — an immutable snapshot. Switching between generations is atomic: either the whole system activates, or nothing changes. Rollback to any previous generation: instant, guaranteed to work, because the previous generation was never modified."

```sh
# Show the deployment commands (don't run — just explain)
echo "# Fresh install from scratch:"
echo "nixos-anywhere --flake .#hostname root@192.168.1.50"
echo ""
echo "# Update an existing machine:"
echo "sudo nixos-rebuild switch --flake .#hostname"
```

> "Every rebuild creates a numbered generation. You can list them, and switch to any one — not just the previous."

```sh
# List all generations on this machine (run live)
sudo nix-env --list-generations --profile /nix/var/nix/profiles/system
```

> "Each line is a generation — a complete, immutable snapshot of the OS. Now let me show how you move between them."

```sh
# Rollback to the previous generation:
sudo nixos-rebuild switch --rollback

# Switch to a specific generation (e.g., generation 42):
sudo nix-env --switch-generation 42 --profile /nix/var/nix/profiles/system
sudo /nix/var/nix/profiles/system/bin/switch-to-configuration switch

# Or simply reboot — the boot menu lists all generations
```

> "This is not backup/restore. Every generation is a complete, immutable snapshot of the entire OS. Switching between them is instant and atomic — like changing a pointer. The previous generation was never modified, so it's guaranteed to work."

---

## Act 2: Fleet Definition (3 min) — "One File, Entire Fleet"

```sh
# The NixFleet test fleet — 5 hosts in one file
cat modules/fleet.nix
```

> "Organization defaults are a `let` binding — just Nix. Each host calls `mkHost` with its flags. `isImpermanent` enables root filesystem wipe on reboot. `isServer` activates server-specific hardening. `isMinimal` strips the base packages. No DSL, no YAML, no framework ceremony. Just Nix."

```sh
# List all hosts in the fleet
nix eval .#nixosConfigurations --apply 'x: builtins.attrNames x' --json | jq .
```

> "5 hosts, each with different characteristics, all sharing the same testDefaults. In a real fleet, replace testDefaults with your organization's config — username, timezone, SSH keys, locale."

### hostSpec flags

```sh
# Show the hostSpec options
grep -A3 'isImpermanent\|isServer\|isMinimal\|isVm' modules/fleet.nix | head -20
```

> "hostSpec flags replace the old concept of roles. Instead of 'this is a server role', you say 'this host is a server, it's minimal, it's impermanent'. Scopes — plain NixOS modules — activate automatically based on these flags."

---

## Act 3: Scaling to 50 Machines (2 min) — "Standard Nix"

```sh
# 50 edge devices from a template — standard Nix, no framework function
cat examples/batch-hosts/fleet.nix
```

> "50 identical edge devices, each created by mapping `mkHost` over a list. Plus named hosts alongside. This is `builtins.map` — standard Nix. No `mkBatchHosts`, no special framework abstraction. If you know Nix, you know how to do this."

```sh
# A client fleet with org defaults
cat examples/client-fleet/fleet.nix
```

> "Acme Corp fleet: a developer workstation and a production server. Organization defaults in a let binding. Each host overrides what it needs. A new client, from zero to fleet definition: 15 minutes."

---

## Act 4: The Rust Stack (2 min) — "Three Binaries, Zero Dependencies"

```sh
# The fleet management CLI
nix run .#nixfleet -- --help
```

> "One CLI for everything: deploy, status, rollback, host management. Talks to the control plane over HTTP."

```sh
# The control plane
nix run .#control-plane -- --help
```

> "Axum server in Rust. Tracks desired state vs actual state for every machine. Audit trail for every mutation. SQLite today, PostgreSQL in enterprise tier."

```sh
# The fleet agent
nix run .#nixfleet-agent -- --help
```

> "Static Rust binary on every managed machine. Polls the control plane for its desired generation. If there's a mismatch, it rebuilds. If the health check fails, it rolls back automatically. No SSH needed — the agent pulls, it doesn't get pushed to."

---

## Act 5: Live Fleet (5 min) — CLIMAX

**This is the moment. Take your time.**

> The demo API key is `demo-key`. The spawn-fleet script seeds it automatically.

### Start the fleet

```sh
# One command: control plane + 2 mock agents + API key seeded
bash demo/spawn-fleet.sh start
```

> "Control plane running on port 8080 with API key authentication. Two agents polling in dry-run mode. The agents will show auth warnings in their logs — in production they'd authenticate via mTLS client certificates. For this demo, we drive the fleet through the operator API."

### Check fleet status

```sh
# Fleet status via API (auth required)
curl -s -H "Authorization: Bearer demo-key" http://127.0.0.1:8080/api/v1/machines | jq .
```

> "Two machines registered. Current generation: none yet. Desired generation: none. Last seen: just now. The control plane knows every machine and its state. Note the API key — every request is authenticated and logged."

### Deploy a generation

```sh
# Set a desired generation for host 1
curl -s -X POST http://127.0.0.1:8080/api/v1/machines/demo-host-01/set-generation \
  -H "Authorization: Bearer demo-key" \
  -H "Content-Type: application/json" \
  -d '{"hash": "/nix/store/abc123-nixos-system-demo-host-01"}'
```

> *Watch Tab 3* — "The agent detects the mismatch between current and desired. In dry-run, it stops before applying. In production, it would `nixos-rebuild switch`, run health checks, and report back — or roll back automatically if the health check fails."

### Fleet-wide status

```sh
# Status again — see the desired generation set
curl -s -H "Authorization: Bearer demo-key" http://127.0.0.1:8080/api/v1/machines | jq .
```

### Audit trail

```sh
# Every mutation is logged — who did what, when
curl -s -H "Authorization: Bearer demo-key" http://127.0.0.1:8080/api/v1/audit | jq .
```

> "Every write operation is recorded with the actor identity, timestamp, and target. This is the NIS2 traceability obligation — satisfied by the architecture itself."

### Using the CLI

```sh
# Same thing via the CLI
nix run .#nixfleet -- status
```

> "The CLI is a thin wrapper over the API. Same data, better formatting."

### Clean up

```sh
bash demo/spawn-fleet.sh stop
```

---

## Act 6: Validation Pyramid (3 min) — "125 Tests, Three Tiers"

### Tier 1 — Eval tests (instant)

```sh
# 6 eval checks — validate config correctness without building anything
nix flake check --no-build
```

> "Six eval tests verify hostSpec defaults, SSH hardening, username overrides, locale, timezone, authorized keys, password files. They run in seconds because they only evaluate the Nix expressions — no builds needed."

### Tier 2 — Rust tests

```sh
# Agent, control plane, CLI, shared types — all tested
cargo test --workspace --quiet 2>&1 | tail -5
```

> "125 tests across the workspace. Agent state machine, control plane API, CLI argument parsing, shared data types."

### Tier 3 — VM tests (mention, don't run)

```sh
# Three VM tests — real NixOS VMs, real assertions
echo "Available VM tests:"
echo "  vm-core        — core NixOS module validation"
echo "  vm-minimal     — minimal host configuration"
echo "  vm-nixfleet    — agent + control plane integration"
echo ""
echo "Run: nix run .#test-vm -- -h web-02"
echo "(Takes ~5 min — skipping for demo)"
```

> "VM tests boot real NixOS virtual machines and run assertions inside them. The `vm-nixfleet` test boots a VM with the agent pre-configured, starts the control plane, and verifies the full polling cycle. We skip them here for time, but they run in CI on every PR."

---

## Act 7: NIS2 Compliance by Construction (2 min) — The Business Close

> "Let me show you why everything you just saw matters for NIS2."

### Traceability — obligation 1

```sh
# Every infrastructure change is a signed Git commit
git log --oneline --graph -15
```

> "NIS2 requires full traceability of all changes to the information system. For a NixFleet organization, every change is a Git commit — who changed what, when, and why. Cost of a separate SIEM for traceability: 30k euros per year. Cost with NixFleet: zero — it's the normal workflow."

### Supply chain security — obligation 2

```sh
# Every dependency pinned by cryptographic hash
jq '.nodes.nixpkgs.locked' flake.lock
```

> "NIS2 requires supply chain security. The `flake.lock` pins every dependency — including all transitive dependencies — to an exact hash. The SBOM is generated automatically. No separate tool, no manual integration."

### Rapid recovery — obligation 3

> "NIS2 requires recovery within 24 hours after an incident. With NixFleet: rollback to the previous generation in under 90 seconds. Not a disaster recovery plan — a mechanism built into the architecture."

### The cost equation

> "A typical SME of 100 machines pays 100-200k euros per year for Ansible + AWX + SIEM + SBOM tools + CMDB separately. NixFleet Pro tier: 6-36k euros per year. The compliance is superior — because it's provable, not just declared. And the organization retains full sovereignty over its infrastructure."

---

## Timing Summary

| Act | Content | Duration | Cached? |
|-----|---------|----------|---------|
| 0 | The Problem (talking points) | 2 min | — |
| 1 | Nix in 5 Minutes | 4 min | ~2s |
| 2 | Fleet Definition | 3 min | ~2s |
| 3 | Scaling to 50 Machines | 2 min | ~1s |
| 4 | The Rust Stack | 2 min | ~2s |
| 5 | **Live Fleet** | 5 min | ~5s |
| 6 | Validation Pyramid | 3 min | ~3s |
| 7 | NIS2 Compliance | 2 min | ~1s |
| **Total** | | **~23 min** | |

---

## Q&A Preparation

| Question | Answer |
|----------|--------|
| "How does it compare to Ansible?" | Fundamental paradigm difference. Ansible describes instructions — the result depends on existing state, drift is inevitable. NixFleet declares desired state — the result is mathematically determined. Rollback is instant and guaranteed, not "re-run the old playbook and hope." |
| "What about Windows?" | NixOS for all managed machines. For mixed fleets: NixOS servers and workstations under NixFleet, Windows devices via existing MDM. The value prop is strongest for Linux-heavy infrastructure. |
| "NIS2 compliance — how exactly?" | Five obligations satisfied natively: traceability (Git history), rapid recovery (<90s rollback), supply chain (flake.lock + auto SBOM), asset inventory (nixosConfigurations), business continuity (previous generation = instant recovery plan). No separate tools needed. |
| "How many machines can it handle?" | Agent-based architecture: each agent polls independently, so thousands. No SSH bottleneck. The control plane is a stateless Axum server that can scale horizontally. |
| "Open source?" | MIT license for the framework and agent. AGPL-3.0 for the control plane. Enterprise features (multi-tenant, RBAC, compliance reporting, dashboard) under a self-hosted commercial license. |
| "What if NixFleet disappears?" | Every machine is a standard NixOS system. The configuration lives in your Git repo. `nixos-rebuild` and `nixos-anywhere` are native NixOS tools — they work without NixFleet. Zero lock-in by design. |
| "What's the learning curve?" | Nix has a real learning curve — the language and module system take time. That's why we sell consulting alongside the product: we do the migration, train the team, and provide ongoing support. The ROI comes from eliminating 3-5 separate tools, not from day-one autonomy. |
| "Who else uses NixOS in production?" | European Space Agency, CERN, Shopify, Replit, Target, Hercules CI. The Sovereign Tech Fund invested 226k euros in the Nix ecosystem in 2023. It's mature — created in 2003, 20+ years of development. |
| "Pricing?" | Community tier free for <10 machines. Pro tier 499-2,999 euros/month for 10-200 machines. Enterprise and Sovereign tiers negotiated. No public pricing for enterprise — we start with a pilot engagement. |
| "What's built today vs what's planned?" | Built: mkHost API, 3 Rust binaries (agent, CP, CLI), 143 tests, eval + VM tests, multi-platform (NixOS + macOS), mTLS authentication, API key RBAC, TLS-only control plane, audit log with CSV export, DB migrations via refinery. Next: open source launch (Phase 3). Planned: dashboard, enterprise features. |

---

## If Something Goes Wrong

| Failure | Recovery |
|---------|----------|
| `nix` command hangs | Pre-demo cache is cold. Show cached output from `/tmp/last-validate-output.txt` |
| Control plane won't start | Check port 8080 not in use: `lsof -i :8080`. Kill and restart. |
| Agent won't connect | Verify CP is running: `curl http://127.0.0.1:8080/api/v1/machines`. Check agent log: `cat /tmp/nixfleet-demo/demo-host-01.log` |
| Eval tests fail | Run `nix flake check --no-build 2>&1` and read the error. Likely a missing file or stale cache. |
| Cargo tests fail | `cargo test --workspace 2>&1 | tail -20` for the specific failure. |
| Everything fails | Switch to the business pitch docs in `docs/business/rendered/` — open `01-what-is-nix.html` through `06-nixfleet-manifest.html` in a browser. The demo becomes a guided walkthrough of the rendered documents. |
