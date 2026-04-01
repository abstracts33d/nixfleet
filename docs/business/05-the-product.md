# Le Produit

*Ce que NixFleet est, comment il fonctionne, et le modele commercial.*

---

## En une phrase

NixFleet est une **plateforme de gestion de flotte NixOS** qui orchestre les garanties mathematiques de NixOS — reproductibilite, atomicite, tracabilite — a l'echelle d'une flotte d'entreprise, avec un control plane centralise, une authentification robuste, et un journal d'audit conforme NIS2.

---

## Architecture

NixFleet est compose de quatre briques, toutes open source ou auto-hebergeables :

```
┌──────────────────────────────────────────────────────────────┐
│                     Operateur (CLI)                          │
│              Pilote la flotte, lance les deploiements        │
└──────────────────────┬───────────────────────────────────────┘
                       │ API keys (RBAC)
                       ▼
┌──────────────────────────────────────────────────────────────┐
│                   Control Plane (Rust)                       │
│    Etat central · orchestration · audit trail · API REST     │
│    Authentification mTLS + API keys · HTTPS obligatoire      │
└─────────┬────────────────────────────────┬───────────────────┘
          │ mTLS                           │ mTLS
          ▼                                ▼
┌─────────────────────┐      ┌─────────────────────┐
│   Agent (Rust)      │      │   Agent (Rust)      │
│   Machine A         │      │   Machine B         │  ... × N
│   Polling autonome  │      │   Polling autonome  │
│   Rollback auto     │      │   Rollback auto     │
└─────────────────────┘      └─────────────────────┘
          │                            │
          ▼                            ▼
┌─────────────────────┐      ┌─────────────────────┐
│   NixOS (flake)     │      │   NixOS (flake)     │
│   Configuration     │      │   Configuration     │
│   declarative       │      │   declarative       │
└─────────────────────┘      └─────────────────────┘
```

### Le framework Nix — `mkHost`

Le coeur de NixFleet cote Nix est une seule fonction : `mkHost`. Elle prend une specification de machine et produit un `nixosSystem` ou `darwinSystem` standard :

```
mkHost {
  name = "web-prod-01";
  system = "x86_64-linux";
  hostSpec = {
    isServer = true;
    isMinimal = false;
  };
  modules = [ ... ];
}
```

Pas de DSL custom a apprendre. Pas de surcouche proprietaire. Le resultat est un `nixosConfiguration` standard, deployable avec les outils natifs NixOS (`nixos-rebuild`, `nixos-anywhere`). Si un client veut quitter NixFleet, il garde sa configuration — elle fonctionne sans NixFleet.

### Le Control Plane — cerveau de la flotte

Le control plane est un serveur Rust (Axum) qui :

- **Connait l'etat de chaque machine** — generation active, derniere communication, sante
- **Orchestre les deploiements** — assigne des configurations, suit l'avancement
- **Maintient un journal d'audit** — chaque action est tracee avec l'identite de l'acteur
- **Expose une API REST** — pour la CLI, un futur dashboard, et les integrations

Authentification :
- **mTLS** entre agents et control plane (certificats client TLS)
- **API keys** pour les operateurs (hashees, scopees par permission : read-only, deploy, admin)
- **HTTPS obligatoire** en production (l'agent refuse HTTP)

### L'Agent — autonomie et resilience

L'agent est un binaire Rust statique installe sur chaque machine. Son fonctionnement :

1. **Polling** — L'agent interroge periodiquement le control plane pour connaitre sa configuration attendue
2. **Comparaison** — Il compare la generation active avec la generation attendue
3. **Application** — Si elles different, il applique la nouvelle configuration via `nixos-rebuild`
4. **Health check** — Apres le deploiement, il execute des verifications de sante
5. **Rollback automatique** — Si le health check echoue, il revient automatiquement a la generation precedente

Ce modele **polling** (l'agent tire les mises a jour) est fondamentalement different du modele **push** (SSH) utilise par Ansible/Colmena :

| | Push (Ansible, SSH) | Pull (NixFleet agent) |
|---|---|---|
| Firewall | Necessite SSH entrant sur chaque machine | Seul le CP necessite un port entrant |
| Disponibilite | Si le deploiement echoue a mi-chemin, etat indetermine | L'agent reessaie au prochain cycle |
| Echelle | Lent avec des centaines de machines (connexions SSH sequentielles) | Chaque agent opere independamment |
| Autonomie | Depend de la disponibilite de la machine de deploiement | L'agent fonctionne meme si le CP est temporairement indisponible |

### La CLI — interface operateur

La CLI permet aux operateurs de :
- Lister les machines et leur etat
- Assigner des configurations
- Declencher des deploiements
- Consulter le journal d'audit
- Exporter les rapports de conformite

---

## Ce qui est implemente aujourd'hui

NixFleet n'est pas une vision — c'est un produit fonctionnel avec securite production-ready :

| Composant | Statut |
|-----------|--------|
| Framework Nix (`mkHost`) | Fonctionnel, API simplifiee |
| Control plane Rust | Fonctionnel, SQLite, HTTPS, mTLS |
| Agent Rust | Fonctionnel, polling + rollback, TLS |
| CLI Rust | Fonctionnel, deploy + status + rollback |
| Auth mTLS agent-CP | Implemente (rustls, certificats client) |
| API keys operateur | Implemente (SHA-256, RBAC : readonly/deploy/admin) |
| HTTPS obligatoire | Implemente (agent refuse HTTP en production) |
| Journal d'audit | Implemente (API + export CSV, identite acteur) |
| Migrations DB | Implemente (refinery, 3 migrations versionnees) |
| Tests | 143 tests Rust + 6 eval + 3 VM |
| Deploiement NixOS | `nixos-anywhere` + `nixos-rebuild` operationnels |
| macOS | `darwin-rebuild` operationnel |
| Documentation | Mdbook, architecture, guides |

---

## Modele commercial : open-core

### Philosophie

Le moteur core est et restera **open source**. La valeur commerciale est dans l'orchestration enterprise : multi-tenant, RBAC granulaire, compliance reporting, dashboard, support.

Le client ne depend jamais de NixFleet. S'il quitte, il garde :
- Sa configuration Nix (fonctionne avec les outils natifs NixOS)
- Son historique Git complet
- Ses machines NixOS operationnelles

### Tiers

| Tier | Prix | Machines | Cible | Fonctionnalites cles |
|------|------|----------|-------|---------------------|
| **Community** | Gratuit | < 10 | Equipes techniques, evaluation | Core engine, CLI, Apache 2.0 |
| **Pro** | 499-2 999 EUR/mois | 10-200 | PME sous NIS2 | Dashboard, audit logs, RBAC, cache binaire, support email |
| **Enterprise** | 50k-500k EUR/an | 200+ | Grandes entreprises | SSO/SAML, SLA garanti, on-prem CP, support dedie, NIS2 compliance package |
| **Sovereign** | Sur mesure (100k+ EUR) | Variable | Gouvernement, defense | Air-gapped, source code escrow, certification ANSSI/BSI, deploiement on-site |

### Split open-source vs enterprise

| Open source (MIT/AGPL) | Enterprise (licence self-hosted) |
|------------------------|----------------------------------|
| `mkHost` API, core modules | Control plane multi-tenant |
| Agent + CLI | RBAC granulaire |
| Auth mTLS + API keys | Compliance reporting NIS2 (PDF, preuves) |
| Audit log basique | Export CSV/PDF structure |
| SQLite | PostgreSQL + backup auto |
| Drift detection par hash | Drift avance (fichiers, services, secrets) |
| Rollback 1 generation | Rollback N generations + dry-run |
| Logs JSON | Dashboard web + Prometheus metrics |

### Services complementaires

| Service | Description |
|---------|-------------|
| **Audit NIS2** | Evaluation infrastructure vs obligations, rapport gaps + plan remediation |
| **Pilote** | Audit + deploiement NixFleet sur 5-10 machines, preuve de concept |
| **Migration** | Pilote + migration progressive depuis Ansible/Puppet, formation equipe |
| **Formation** | Nix/NixOS pour equipes infra, certification |

---

## Differenciateurs vs competition

### Vs Ansible / Puppet (config management imperatif)

| | Ansible/Puppet | NixFleet |
|---|---|---|
| Paradigme | Imperatif (instructions) | Declaratif (etat final) |
| Reproductibilite | Depend de l'etat existant | Garantie mathematique |
| Derive | Inevitable | Impossible |
| Rollback | Manuel, risque | Atomique, < 90s |
| NIS2 | Outils separes necessaires | Natif |
| Cout total (200 machines) | Ansible + AWX + SIEM + SBOM + CMDB | Tier Pro NixFleet |

### Vs Jamf / Intune (MDM cloud)

| | Jamf/Intune | NixFleet |
|---|---|---|
| Souverainete | Cloud US, Cloud Act | 100% auto-hebergeable, EU |
| Lock-in | Total | Zero (NixOS standard) |
| Reproductibilite | Non | Oui |
| Prix (200 postes) | ~40-100k EUR/an | Tier Pro ~6-36k EUR/an |
| Conformite RGPD | Complexe (donnees US) | Simple (tout sur votre infra) |

### Vs Colmena / NixOps (outils communautaires)

| | Colmena/NixOps | NixFleet |
|---|---|---|
| Support | Communaute | SLA, support dedie |
| UX | CLI basique | Dashboard + CLI |
| Audit trail | Non | Oui |
| Agent autonome | Non (push SSH) | Oui (polling + rollback auto) |
| Control plane | Non | Oui |
| NIS2 | Non | Natif |

---

## Stack technique

| Composant | Technologie |
|-----------|-------------|
| Control plane | Rust (Axum) |
| Agent | Rust (binaire statique) |
| CLI | Rust |
| Cache binaire | Attic (S3-compatible) |
| Secrets | agenix / sops-nix |
| Configuration | NixOS flakes |
| Base de donnees | SQLite (community) / PostgreSQL (enterprise) |
| Frontend (Phase 4) | SvelteKit |
| Licence core | MIT (framework/agent) / AGPL-3.0 (control plane) |

---

## Roadmap produit

| Phase | Statut | Description |
|-------|--------|-------------|
| **Phase 0 — Simplification** | Terminee | API `mkHost` propre, migration complete |
| **Phase 1 — Hardening** | Terminee | mTLS, API keys (SHA-256, RBAC), TLS-only, audit log + CSV export, refinery migrations |
| **Phase 2 — Open Source** | **Prochain** | Repo public, docs, landing page, communaute |
| **Phase 3 — Infrastructure** | Planifie | Modules Attic + microvm |
| **Phase 4 — Enterprise** | Planifie | Consulting NIS2, pilotes, licence enterprise |

---

*[Precedent : Marche cible](04-target-market.md) · [Retour au sommaire](README.md)*
