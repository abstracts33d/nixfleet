# NixFleet Business

**Declarative NixOS fleet management for European enterprises.**

This section contains business strategy, API specs, and research documents.

## Structure

```
business/
├── data/                          # Structured YAML — source of truth
│   ├── fcs.yaml                   # 6 key success factors
│   ├── business-model.yaml        # Open-core tiers, pricing, fundraising
│   ├── competitors.yaml           # Competitive landscape
│   ├── roadmap.yaml               # Technical roadmap (phases 0-4)
│   ├── nis2-mapping.yaml          # NIS2 obligation mapping
│   └── market.yaml                # EU market analysis, verticals, funding
├── rendered/                      # Generated/authored documents (read-only)
│   ├── 01-synthese-fcs-v3.html    # Strategic analysis
│   ├── 02-nixfleet-pitch-v3.pptx  # Investor pitch deck
│   ├── 03-nixfleet-architecture-v3.html  # Technical architecture
│   └── 04-nixfleet-manifeste-v3.html     # Founding manifesto
├── specs/                         # API specifications
│   └── mk-fleet-api.md            # mkFleet, mkOrg, mkHost, mkRole API reference
└── research/                      # Design research and analysis
    ├── two-repo-split-flake-parts.md
    ├── framework-vs-overlay-separation.md
    └── client-needs-per-tier.md
```

**YAML files in `data/` are the source of truth.** The rendered documents in `rendered/` are presentation artifacts derived from this data.

## Core Thesis

Six equal-weight value propositions:
1. **Deterministic reproducibility** — same flake -> same system, bit for bit
2. **Total sovereignty** — fully self-hosted, Apache 2.0 core, no vendor lock-in
3. **Atomic rollback** — <90s fleet-wide rollback, no partial state
4. **Structural security** — SHA-256 store, impermanence, auto SBOM
5. **NIS2 compliance by construction** — traceability, recovery, supply chain security as architecture byproducts
6. **Cost reduction** — 3-5x cheaper than Ansible + AWX + separate compliance tools

## Business Model

Open-core (Apache 2.0):
- **Community** — Free, <10 machines
- **Pro** — EUR 499-2,999/mo, 10-200 machines, dashboard + RBAC + audit
- **Enterprise** — EUR 50k-500k/yr, 200+ machines, SSO + SLA + on-prem
- **Sovereign** — Custom, air-gapped, ANSSI/BSI certification support

## Implemented Features (S1+S2 done)

The framework is live, not a vision:
- 16 hosts declared in a single `fleet.nix` via `mkFleet` = NixFleet's fleet definition model
- Organizations (`mkOrg`) and roles (`mkRole`) = NixFleet's multi-tenant, role-based composition
- `mkBatchHosts` and `mkTestMatrix` = NixFleet's fleet scaling primitives
- The architecture (flake-parts, deferred modules, hostSpec, scopes) = NixFleet's config model
- The automation layer (agents, skills, hooks, MCP) = NixFleet's development tooling
- The testing pyramid (eval, VM, smoke) with 18 eval checks = NixFleet's quality assurance model
- `options.nixfleet.extensions` namespace = NixFleet's extension point for paid modules

## Target Market

- **Primary:** EU enterprises subject to NIS2 (15,000+ French entities alone)
- **Stack:** Rust control plane + NixOS flakes + Attic binary cache
- **Verticals:** Government, finance, healthcare, energy, HPC
