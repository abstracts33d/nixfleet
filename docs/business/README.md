# NixFleet — Infrastructure Souveraine pour l'Europe

**La premiere plateforme commerciale europeenne de gestion de flotte NixOS.**

NixFleet transforme les garanties mathematiques de NixOS — reproductibilite, atomicite, tracabilite — en un produit enterprise auto-hebergeable, conforme NIS2 par construction.

---

## La these en bref

L'infrastructure informatique europeenne fait face a trois crises simultanees :

1. **La crise de la reproductibilite** — Les outils actuels (Ansible, Puppet, Chef) ne peuvent pas garantir qu'un systeme deploye aujourd'hui sera identique a celui deploye demain. La derive de configuration est inevitable.

2. **La crise de la souverainete** — Les plateformes dominantes (Jamf, Intune, AWX) font de votre infrastructure une dependance de leur cloud. Votre capacite a deployer depend de leur disponibilite.

3. **La crise reglementaire** — La directive NIS2 impose a plus de 15 000 entites francaises des obligations de tracabilite, de reprise rapide, et de securite de la chaine d'approvisionnement. L'echeance est fin 2027. Les amendes atteignent 10M EUR ou 2% du chiffre d'affaires mondial. Les dirigeants sont personnellement responsables.

NixFleet repond a ces trois crises avec une seule architecture.

---

## Table des matieres

| Document | Contenu |
|----------|---------|
| [01 — Qu'est-ce que Nix ?](01-what-is-nix.md) | Nix comme langage, gestionnaire de paquets, et systeme d'exploitation |
| [02 — Pourquoi Nix ?](02-why-nix.md) | Le changement de paradigme : de l'imperatif au declaratif |
| [03 — Les problemes que nous resolvons](03-problems-we-solve.md) | Souverainete, securite, reproductibilite, conformite NIS2 |
| [04 — Marche cible](04-target-market.md) | Qui nous ciblons et pourquoi c'est le bon moment |
| [05 — Le produit](05-the-product.md) | Ce que NixFleet est, comment il fonctionne, modele commercial |

### Documents rendus (HTML, presentations)

| # | Document | Design | Contenu |
|---|----------|--------|---------|
| 01 | [Qu'est-ce que Nix ?](rendered/01-what-is-nix.html) | Pitch | Introduction visuelle a Nix |
| 02 | [Problemes & Solutions](rendered/02-problems-we-solve.html) | Pitch | Vue detaillee des problemes resolus |
| 03 | [Marche & Produit](rendered/03-target-and-product.html) | Pitch | Analyse marche et presentation produit |
| 04 | [Analyse strategique](rendered/04-synthese-fcs.html) | Pitch | Les 6 facteurs cles de succes |
| 05 | [Architecture technique](rendered/05-nixfleet-architecture.html) | Technique | Composants, protocoles, garanties |
| 06 | [Manifeste fondateur](rendered/06-nixfleet-manifest.html) | Editorial | Vision et engagements |

### Donnees structurees (source de verite)

```
data/
├── fcs.yaml              # 6 facteurs cles de succes
├── business-model.yaml   # Modele open-core, tiers, pricing
├── competitors.yaml      # Paysage concurrentiel
├── roadmap.yaml          # Roadmap technique (phases 0-4)
├── nis2-mapping.yaml     # Correspondance obligations NIS2
└── market.yaml           # Analyse marche EU, verticaux, financement
```

---

## Qu'est-ce que Nix ?

Nix est trois choses a la fois :

- **Un langage** — Un langage fonctionnel pur, concu pour decrire des systemes de maniere deterministe. Pas de variables mutables, pas d'effets de bord. Le meme code produit toujours le meme resultat.

- **Un gestionnaire de paquets** — Le plus grand depot de paquets au monde (100 000+ paquets dans nixpkgs). Chaque paquet est isole, identifie par le hash cryptographique de toutes ses dependances. Deux versions du meme logiciel coexistent sans conflit.

- **Un systeme d'exploitation** — NixOS est un OS Linux entierement configure en Nix. Le systeme complet — noyau, services, utilisateurs, reseau — est declare dans un fichier de configuration. Un rebuild produit un systeme identique, partout, toujours.

> *[En savoir plus : Qu'est-ce que Nix ?](01-what-is-nix.md)*

---

## Pourquoi Nix ?

Parce que Nix change les regles du jeu. La ou les outils traditionnels *decrivent des actions a effectuer* (installer un paquet, modifier un fichier), Nix *declare l'etat final desire*. Cette difference n'est pas cosmetique — elle change fondamentalement ce qui est garanti :

| Propriete | Outils traditionnels | NixOS |
|-----------|---------------------|-------|
| Reproductibilite | Depend de l'etat existant | Garantie mathematiquement |
| Rollback | Manuel, risque | Atomique, instantane, fiable |
| Derive de configuration | Inevitable | Impossible par construction |
| Audit | Effort additionnel | Natif (historique Git) |
| Securite supply chain | Outils separes | Integree (flake.lock + SBOM auto) |

> *[En savoir plus : Pourquoi Nix ?](02-why-nix.md)*

---

## Les problemes que nous resolvons

### Souverainete

L'infrastructure ne devrait pas dependre d'un fournisseur cloud americain. NixFleet est integralement auto-hebergeable : moteur core (open source), plan de controle, cache binaire, secrets. Si NixFleet disparait, votre flotte continue de fonctionner.

### Securite

Le Nix Store est adresse par hash SHA-256 : un binaire modifie est physiquement impossible a substituer. L'impermanence efface tout etat non declare au redemarrage, eliminant la persistance des malwares. Le SBOM est genere automatiquement depuis flake.lock.

### Reproductibilite

Le meme `flake.nix` produit le meme systeme, bit pour bit, sur n'importe quelle machine, a n'importe quel moment. Ce n'est pas "la meme configuration" — c'est le meme systeme, identifie par le meme hash cryptographique.

### Conformite NIS2

La directive NIS2 (15 000 entites francaises, echeance fin 2027, amendes jusqu'a 10M EUR) impose tracabilite, reprise rapide, securite de la chaine d'approvisionnement. Pour une organisation NixFleet, ces obligations sont satisfaites par defaut :

| Obligation NIS2 | Approche classique | NixFleet |
|-----------------|-------------------|----------|
| Tracabilite des modifications | SIEM + outils separes (+30k EUR/an) | Chaque changement = commit Git signe |
| Reprise apres incident en 24h | Runbooks manuels, incertain | Rollback atomique < 90 secondes |
| Securite chaine d'approvisionnement | Outils SBOM separes | SBOM auto depuis flake.lock |
| Inventaire des actifs | CMDB couteuse, souvent inexacte | Inventaire complet dans nixosConfigurations |

> *[En savoir plus : Les problemes que nous resolvons](03-problems-we-solve.md)*

---

## Marche cible

**Cible primaire :** PME et ETI europeennes (50-500 employes) soumises a NIS2, sans equipe compliance dediee.

**Pourquoi c'est un bon fit :**

- **Pression reglementaire** — NIS2 cree une demande structurelle avec une deadline dure (fin 2027) et des sanctions personnelles pour les dirigeants
- **Vide commercial** — Aucun concurrent EU direct sur le segment "gestion de flotte NixOS enterprise"
- **Timing** — Les organisations se preparent maintenant (plateforme ANSSI ouverte depuis novembre 2025)
- **Douleur reelle** — Les outils existants (Ansible + compliance separee) coutent 3-5x plus cher pour un resultat inferieur

**Verticaux prioritaires :** Recherche/HPC, startups tech, finance reglementee, collectivites, telecoms/energie.

**Geographies :** France (15 000 entites NIS2), Allemagne (mandat souverainete), Pays-Bas (forte communaute NixOS), Belgique (institutions EU), Suisse, Nordiques.

> *[En savoir plus : Marche cible](04-target-market.md)*

---

## Le produit

NixFleet est une plateforme de gestion de flotte NixOS composee de :

- **Un framework Nix** — `mkHost` API pour declarer des machines dans un `flake.nix`, avec scopes, hostSpec, et modules NixOS partageables
- **Un control plane Rust** — Serveur central qui connait l'etat de chaque machine, orchestre les deploiements, et maintient un journal d'audit
- **Un agent Rust** — Binaire statique sur chaque machine, polling autonome, rollback automatique sur echec de health check
- **Une CLI Rust** — Interface operateur pour piloter la flotte

### Modele commercial : open-core

| Tier | Prix | Cible |
|------|------|-------|
| **Community** | Gratuit | < 10 machines, equipes techniques |
| **Pro** | 499-2 999 EUR/mois | 10-200 machines, PME |
| **Enterprise** | 50k-500k EUR/an | 200+ machines, grands comptes |
| **Sovereign** | Sur mesure | Gouvernement, defense, air-gapped |

### Differenciateurs vs existant

| | Ansible/Puppet | Jamf/Intune | Colmena | **NixFleet** |
|---|---|---|---|---|
| Reproductibilite | Non | Non | Oui | **Oui** |
| Souverainete | Partielle | Non | Oui | **Oui** |
| Support commercial | Oui | Oui | Non | **Oui** |
| Rollback atomique | Non | Non | Non | **Oui** |
| NIS2 natif | Non | Non | Non | **Oui** |
| UI/Dashboard | AWX | Oui | Non | **Pro+** |

> *[En savoir plus : Le produit](05-the-product.md)*

---

## Six facteurs cles de succes

1. **Reproductibilite declarative absolue** — Meme flake → meme systeme, bit pour bit, toujours
2. **Souverainete et independance totale** — Integralement auto-hebergeable, zero dependance externe
3. **Rollback atomique et resilience** — Rollback flotte < 90 secondes, zero etat intermediaire
4. **Securite structurelle** — SHA-256 store, impermanence, SBOM automatique
5. **Conformite NIS2 par construction** — Tracabilite, reprise, supply chain — natifs
6. **Reduction des couts** — 3-5x moins cher que Ansible + AWX + compliance separee

---

*NixFleet · Infrastructure Souveraine · Europe · 2026*
