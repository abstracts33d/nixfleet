# NixFleet Product

Distilled knowledge about the NixFleet business model and roadmap.

## What NixFleet Is

An open-core NixOS fleet management framework. This repository serves as the reference implementation -- the framework API (`modules/_shared/lib/`) plus a reference fleet. Framework consumers create their own fleet repo consuming NixFleet as a flake input.

## Business Model: Open-Core

| Tier | Price | Target | Key features |
|------|-------|--------|-------------|
| **Community** | Free | <10 machines | Core CLI, Apache 2.0 |
| **Pro** | EUR 499-2,999/mo | 10-200 machines | Dashboard UI, audit logs, RBAC, binary cache, email support |
| **Enterprise** | EUR 50k-500k/yr | 200+ machines | SSO/SAML, SLA, on-prem control plane, NIS2 compliance |
| **Sovereign** | Custom, EUR 100k+ ACV | Government, defense | Air-gapped, source escrow, ANSSI/BSI certification |

Services (capped at 30% revenue): migration from Ansible/Puppet, fleet onboarding, training, managed cache.

## Technical Roadmap

| Phase | Name | Status | Key deliverables |
|-------|------|--------|-----------------|
| **0** | Reference Implementation | Done | Flake-parts architecture, multi-host, impermanence, testing pyramid, 15 agents, 17 skills |
| **1** | Generalization | In progress | S1 (mkFleet/mkOrg — done), S2 (roles — done), S8 (open-core licensing) |
| **2** | Agent + Cache | In progress | S3 (Rust fleet agent — MVP done), S5 (Attic binary cache) |
| **3** | Control Plane | In progress | S4 (Rust Axum control plane — MVP done), S7 (NIS2 compliance) |
| **4** | Commercialization | Planned | S6 (air-gap deployment), EU certifications |

## Tech Stack

- **Config**: NixOS flakes (flake-parts, deferred modules, hostSpec, scopes)
- **Agent**: Rust (tokio, clap, reqwest, rusqlite)
- **Dashboard**: Go (embed.FS, gorilla/websocket, goldmark)
- **Cache**: Attic (S3-compatible, EU-hosted)
- **Secrets**: Framework-agnostic (agenix for reference fleet, Vault for Pro tier)
- **Frontend**: SvelteKit (Phase 3)

## Fundraising

Pre-seed, EUR 1.5M ask. Allocation: engineering 40%, open-source community 25%, EU certifications 20%, GTM 15%.

## Architecture Decisions

- **Framework API**: `mkFleet` / `mkOrg` / `mkRole` in `modules/_shared/lib/`
- **Repo strategy**: `nixfleet/` (Apache 2.0) + `nixfleet-platform/` (proprietary) + client repos
- **Secrets**: Framework-agnostic via `hostSpec.secretsPath`
- **Multi-org**: Per-repo isolation, not multi-tenant
- **Extensions**: `options.nixfleet.extensions.*` for paid platform modules
