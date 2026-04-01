# NixFleet Go-To-Market — Avec Co-founder

**Date** : 2026-03-28
**Contexte** : 2 co-founders, bootstrap, pas de réseau infra critique EU
**Différence clé vs solo** : la bande passante doublée permet de paralléliser produit et commercial dès le départ, et d'accélérer significativement chaque phase.

## Vision

Identique au scénario solo : infrastructure souveraine, reproductible et auditable pour NIS2 en Europe. Mais la présence d'un co-founder change la vitesse d'exécution et le partage des rôles.

## Modèle de distribution

Identique : open-source framework + licence enterprise self-hosted.

## Hypothèse co-founder

Le scénario le plus impactant : **un co-founder technique + un co-founder commercial/domaine**. Variantes :

| Configuration | Impact |
|---------------|--------|
| **Tech + Commercial/NIS2** | Optimal — produit et pipeline client avancent en parallèle |
| **Tech + Tech** | Accélère le hardening/features, mais le réseau reste à construire |
| **Tech + Généraliste** | Flexible, la personne s'adapte au besoin le plus urgent |

Le design ci-dessous suppose **Tech + Commercial/NIS2** (best case). Les variantes sont notées quand ça change.

---

## Phase 1 — Hardening + Prospection en parallèle (4-6 semaines)

**Changement vs solo** : les deux tracks démarrent simultanément.

### Track A — Tech (co-founder tech)

Identique au solo :

#### 1.1 — Auth : mTLS agent↔CP + API keys opérateur

- Agents via certificat client TLS (mTLS)
- Opérateurs via API key `Authorization: Bearer <key>`
- API keys hashées en DB, scopées (read-only, deploy, admin)
- Mutations loggées avec identité

#### 1.2 — TLS obligatoire

- HTTPS uniquement, agent refuse `http://` en prod
- Certificats via agenix ou Let's Encrypt

#### 1.3 — Audit log

- Table `audit_events`, endpoint filtrable, export CSV

#### 1.4 — DB migrations

- `refinery` pour migrations SQL versionnées

### Track B — Commercial (co-founder commercial)

Pendant que le hardening avance :

#### 1.5 — Étude de marché terrain

- Identifier 20-30 PME EU sous NIS2 dans les secteurs accessibles (hébergement, SaaS B2B, deeptech)
- 10-15 appels exploratoires : comprendre leurs douleurs NIS2, budget, timeline compliance
- Cartographier : qui décide, quel budget, quels outils actuels, quel niveau d'urgence

#### 1.6 — Positionnement validé

- Tester 2-3 pitchs différents en appel (souveraineté vs compliance vs reproductibilité)
- Identifier le message qui résonne le plus
- Résultat : positionnement validé par de vrais prospects, pas par intuition

#### 1.7 — Pipeline initial

- 3-5 prospects qualifiés prêts à discuter d'un audit/pilote dès que le produit est prêt
- Lettres d'intention ou accords verbaux pour un pilote

**Avantage co-founder** : quand le hardening est terminé, tu as déjà des prospects chauds. En solo, la prospection commence après.

---

## Phase 2 — Open-source + Premiers contacts (2-3 semaines)

**Changement vs solo** : le commercial a déjà un pipeline, l'open-source amplifie.

### Track A — Tech

#### 2.1 — Repo public

- nixfleet en public, README, CONTRIBUTING.md
- Licence : AGPL-3.0 (CP), MIT (framework/agent)

#### 2.2 — Documentation

- Getting started, architecture, API fleet definition

#### 2.3 — Landing page

- Page unique : pitch, architecture, lien repo, "Contact pour pilote"

### Track B — Commercial

#### 2.4 — Activation communauté

- Posts NixOS Discourse + Hacker News (coordonné avec la publication du repo)
- Le commercial pousse le contenu sur LinkedIn, Twitter/X, communautés NIS2

#### 2.5 — Conversion pipeline

- Recontacter les prospects qualifiés en Phase 1 : "le produit est live, voici la démo"
- Planifier 2-3 démos personnalisées
- Objectif : signer le premier audit dans les 2 semaines

#### 2.6 — Partenariats

- Identifier 2-3 cabinets de conseil NIS2/cybersécurité EU qui pourraient recommander NixFleet
- Premier contact : proposer un partenariat de référencement mutuel
- *Note : en config Tech+Tech, cette étape est plus lente mais reste faisable via le réseau NixOS*

---

## Phase 4A — Offre consulting NIS2 (1-2 semaines)

**Changement vs solo** : réduit de 2-3 à 1-2 semaines car la prospection est déjà faite.

### 4A.1 — Positionnement

> "Audit et mise en conformité NIS2 de l'infrastructure Linux — reproductibilité, traçabilité, supply chain security"

Validé par les appels exploratoires de Phase 1.

### 4A.2 — Offres tiered

| Offre | Contenu | Prix indicatif |
|-------|---------|----------------|
| **Audit** | Évaluation infra vs NIS2, rapport gaps + remédiation | 5-15k€ |
| **Pilote** | Audit + NixFleet sur 5-10 machines | 20-40k€ |
| **Migration** | Pilote + migration progressive, formation, support 3 mois | 60k€+ |

*Prix légèrement plus hauts que solo : le co-founder commercial peut négocier et justifier la valeur mieux qu'un tech seul.*

### 4A.3 — Livrables

- One-pager PDF, profil LinkedIn, template rapport d'audit
- **Ajout co-founder** : deck de présentation pour les démos, réponses aux objections courantes documentées

---

## Phase 4B — Pilotes accélérés (ongoing)

**Changement vs solo** : 2 pilotes en parallèle possibles, pipeline continu.

### 4B.1 — Répartition des rôles en pilote

| Étape | Tech | Commercial |
|-------|------|-----------|
| Audit | Évaluation technique infra | Relation client, cadrage scope, pricing |
| Déploiement | CP + agents + fleet.nix | Suivi projet, reporting client |
| Validation | Tests reproductibilité, rollback | Préparation case study, collecte témoignage |
| Rapport | Documentation technique | Proposition commerciale pour la suite |

### 4B.2 — Pipeline continu

Pendant qu'un pilote est en cours, le commercial prospecte le suivant. Pas de temps mort entre pilotes.

### 4B.3 — Objectif

- **5-6 pilotes en 6 mois** (vs 3 en solo)
- ~100-200k€ de revenus
- 5+ case studies
- Liste validée de features enterprise

---

## Phase 4C — Enterprise product (4-8 mois)

**Changement vs solo** : arrive plus tôt (4-8 mois vs 6-12) grâce au volume de pilotes accéléré.

### 4C.1 — Trigger

Identique : 3+ pilotes, 2+ features demandées par 2+ clients, runway suffisant.

### 4C.2 — Split open-source / enterprise

| Open-source (MIT/AGPL) | Enterprise (licence self-hosted) |
|-------------------------|----------------------------------|
| mkFleet DSL, agent, CLI | Control plane multi-tenant |
| Auth mTLS + API keys | RBAC granulaire |
| Audit log basique | Compliance reporting NIS2 (PDF, preuves) |
| SQLite | PostgreSQL + backup auto |
| Drift detection par hash | Drift avancé (fichiers, services, secrets) |
| Rollback 1 step | Rollback N generations + dry-run |
| Logs JSON | Dashboard web + Prometheus metrics |

### 4C.3 — Pricing

- Licence annuelle par machine : ~50-100€/machine/an
- Support premium optionnel
- Le commercial gère le pricing, les négociations, les renouvellements

### 4C.4 — Décision de scale

Avec 2 personnes et 5-6 clients : 100-200k€ ARR réaliste en année 1. Position pour :
- **Lever un seed** (150-500k€) si le marché le justifie — les case studies + revenus = traction prouvée
- **Rester bootstrap** si les revenus suffisent — 2 personnes rentables
- **Premier hire** : un 3ème profil (DevRel / solutions engineer) qui fait le pont entre technique et commercial

---

## Comparaison solo vs co-founder

| Dimension | Solo | Co-founder |
|-----------|------|------------|
| **Time to first pilot** | 8-11 semaines | 6-8 semaines |
| **Pilotes en 6 mois** | ~3 | ~5-6 |
| **Revenus année 1** | 60-90k€ | 100-200k€ |
| **Pipeline** | Séquentiel (build puis vend) | Parallèle (build ET vend) |
| **Risque principal** | Oscillation build/sell, épuisement | Désalignement co-founders, split equity |
| **Crédibilité prospect** | "Un freelance" | "Une équipe dédiée" |
| **Capacité pilote** | 1 à la fois | 2 en parallèle |
| **Décision funding** | Mois 12+ | Mois 8-10 |

## Profil co-founder idéal

- Expérience vente B2B / consulting en cybersécurité ou compliance EU
- Réseau dans les PME tech EU (hébergeurs, SaaS, deeptech)
- Comprend NIS2 / ISO 27001 / souveraineté numérique
- Complémentaire : pas un 2ème développeur, un profil business/domaine
- Aligné : bootstrap-first, pas "levons 2M avant d'avoir un client"

## Où le trouver

- Événements NIS2 / cybersécurité EU (conférences, meetups)
- Communautés startup EU (Station F, French Tech, startups souveraines)
- LinkedIn : profils "NIS2 consultant" ou "cybersecurity advisor" en transition
- Réseau FOSDEM / NixCon : profils hybrides tech + business

## Risques spécifiques co-founder

- **Equity split** : décider tôt (50/50 ou autre), vesting 4 ans, cliff 1 an
- **Désalignement vision** : s'accorder sur bootstrap vs funded, timeline, ambition
- **Dépendance** : ne pas bloquer le produit si le commercial part (et inversement)
- **Overhead** : à 2, il faut se synchroniser — garder ça léger (standup hebdo, pas de process lourd)
