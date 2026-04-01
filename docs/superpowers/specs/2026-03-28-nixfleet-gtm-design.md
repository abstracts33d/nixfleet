# NixFleet Go-To-Market — Solo Founder

**Date** : 2026-03-28
**Contexte** : Solo founder, bootstrap, pas de réseau infra critique EU

## Prerequisite

La simplification du Nix layer (`2026-03-31-nixfleet-simplification-design.md`) doit être réalisée AVANT la Phase 2 (open-source). On publie avec l'API mkHost propre, pas le DSL mkFleet legacy.

## Vision

Transformer NixFleet d'un prototype personnel en produit commercialisable, financé par du consulting NIS2. Positionnement : infrastructure souveraine, reproductible et auditable pour la conformité NIS2 en Europe.

## Modèle de distribution

- **Open-source** : framework (mkHost + core modules) + agent + CLI (MIT pour framework/agent, AGPL pour control plane)
- **Licence enterprise self-hosted** : control plane avancé (multi-tenant, compliance reporting, RBAC, PostgreSQL)
- Pas de SaaS — le client possède tout

## Différenciateurs vs Colmena/deploy-rs/Ansible

- Agent polling autonome (vs push SSH)
- Control plane centralisé avec état des machines (vs stateless)
- Rollback automatique sur health check failure (vs manuel)
- Tooling standard NixOS (nixos-rebuild, nixos-anywhere) — pas de DSL custom à apprendre
- Multi-plateforme NixOS + macOS via un seul `mkHost`

## État actuel

Prototype solide : 3 binaires Rust (agent, CP, CLI), 125 tests, démo 18min. Nix layer en cours de simplification (voir `2026-03-31-nixfleet-simplification-design.md`). Gaps critiques : auth absente, TLS absent, single-tenant, SQLite sans migrations, pas de metrics.

---

## Phase 1 — Hardening (4-6 semaines)

**Objectif** : Control plane déployable sur réseau réel.

### 1.1 — Auth : mTLS agent↔CP + API keys opérateur

- Agents s'authentifient via certificat client TLS (mTLS)
- Opérateurs via API key `Authorization: Bearer <key>`
- API keys hashées en DB, scopées par permission (read-only, deploy, admin)
- Chaque mutation loggée avec identité (machine_id ou api_key_id + timestamp)

### 1.2 — TLS obligatoire

- Control plane écoute HTTPS uniquement (axum-rustls ou reverse proxy)
- Agent refuse `http://` en production (flag `--allow-insecure` pour dev)
- Certificats via agenix ou Let's Encrypt

### 1.3 — Audit log

- Table `audit_events` : timestamp, actor, action, target, detail
- Chaque opération d'écriture crée un événement
- Endpoint `GET /api/v1/audit` avec filtres
- Export CSV pour compliance NIS2

### 1.4 — DB migrations

- Intégrer `refinery` (migrations SQL versionnées)
- Migrations appliquées automatiquement au démarrage
- Prépare les ajouts futurs (multi-tenant, metrics)

**Hors scope** : multi-tenant, Prometheus, PostgreSQL, drift avancé, push mechanism, tests E2E.

---

## Phase 2 — Open-source (1-2 semaines)

**Objectif** : Rendre nixfleet visible et crédible.

### 2.1 — Repo public

- Passer `github:abstracts33d/nixfleet` en public
- README orienté utilisateur : quoi, pourquoi, quickstart
- `CONTRIBUTING.md` minimal
- Licence : AGPL-3.0 (control plane), MIT (framework/agent)
- Nettoyer secrets/paths perso résiduels

### 2.2 — Documentation

- `docs/getting-started.md` : CP + 2 agents en 15 minutes
- `docs/architecture.md` : schéma, state machine, protocole
- `docs/fleet-definition.md` : API mkHost + structure fleet repo + référence hostSpec

### 2.3 — Landing page minimale

- Page unique (GitHub Pages) : pitch, architecture, lien repo, "Contact pour pilote"

### 2.4 — Présence communauté

- Post NixOS Discourse
- Post Hacker News quand quickstart propre
- Proposer talk NixCon 2026

---

## Phase 4A — Offre consulting NIS2 (2-3 semaines)

**Objectif** : Packager l'expertise en offre vendable.

### 4A.1 — Positionnement

> "Audit et mise en conformité NIS2 de l'infrastructure Linux — reproductibilité, traçabilité, supply chain security"

On vend la conformité NIS2, NixFleet est l'outil. Le client achète le résultat.

### 4A.2 — Offres tiered

| Offre | Contenu | Prix indicatif |
|-------|---------|----------------|
| **Audit** | Évaluation infra vs NIS2, rapport gaps + plan remédiation | 5-10k€ |
| **Pilote** | Audit + NixFleet sur 5-10 machines, preuve reproductibilité | 15-30k€ |
| **Migration** | Pilote + migration progressive, formation, support 3 mois | 50k€+ |

### 4A.3 — Prospects accessibles

PME tech/infra EU (50-500 employés) sous NIS2 sans équipe compliance :
- Hébergeurs / cloud providers EU
- SaaS B2B données sensibles (santé, finance, RH)
- Startups deeptech avec infra bare metal

Canal : LinkedIn (posts NIS2), communauté NixOS (Phase 2 Open Source), événements (FOSDEM, NixCon, meetups).

### 4A.4 — Livrables

- One-pager PDF : "Votre infrastructure est-elle prête pour NIS2 ?"
- Profil LinkedIn positionné NIS2 + infra souveraine
- Template rapport d'audit réutilisable

---

## Phase 4B — Premiers pilotes (ongoing)

**Objectif** : Chaque mission = déploiement NixFleet réel.

### 4B.1 — Déroulement type

1. **Audit** (1-2 sem) : cartographie infra, gaps NIS2, périmètre pilote
2. **Déploiement** (2-4 sem) : CP chez client, agents sur 5-10 machines, fleet.nix
3. **Validation** (1-2 sem) : reproductibilité, audit trail, test rollback
4. **Rapport** (1 sem) : documentation conformité, recommandations, proposition suite

### 4B.2 — Valeur de chaque pilote

- Feedback produit réel
- Case study
- 15-30k€ de revenus
- Réseau (chaque client en connaît d'autres)

### 4B.3 — Boucle produit

Gaps des pilotes → backlog. Si 2+ clients demandent la même chose → feature enterprise.

### 4B.4 — Objectif

3 pilotes en 6 mois → 3 case studies, ~60-90k€, liste claire de features enterprise.

---

## Phase 4C — Enterprise product (6-12 mois)

**Objectif** : Lancer la licence self-hosted.

### 4C.1 — Trigger

- 3+ pilotes réalisés
- 2+ features demandées par 2+ clients
- Revenus consulting suffisants pour 2-3 mois de dev pur

### 4C.2 — Split open-source / enterprise

| Open-source (MIT/AGPL) | Enterprise (licence self-hosted) |
|-------------------------|----------------------------------|
| mkHost API, core modules, agent, CLI | Control plane multi-tenant |
| Auth mTLS + API keys | RBAC granulaire |
| Audit log basique | Compliance reporting NIS2 (PDF, preuves) |
| SQLite | PostgreSQL + backup auto |
| Drift detection par hash | Drift avancé (fichiers, services, secrets) |
| Rollback 1 step | Rollback N generations + dry-run |
| Logs JSON | Dashboard web + Prometheus metrics |

### 4C.3 — Pricing

- Licence annuelle par machine : ~50-100€/machine/an (dégression volume)
- Support premium optionnel : SLA, téléphone, assistance déploiement
- Pas de pricing public au début — négocié par client

### 4C.4 — Maturité

10-20 clients enterprise × 50 machines moyennes = 25k-100k€ ARR + consulting récurrent. Suffisant pour valider et décider : scale (hiring, funding) ou lifestyle business profitable.
