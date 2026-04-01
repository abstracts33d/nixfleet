# Marche Cible

*Qui nous ciblons, pourquoi c'est le bon moment, et pourquoi c'est un bon fit.*

---

## Un marche cree par decret

La directive NIS2, entree en vigueur en octobre 2024, cree un marche qui n'existait pas auparavant. En France, la Loi Resilience (transposition NIS2) a ete adoptee par le Senat en mars 2025. La conformite complete est exigee fin 2027.

### L'echelle du changement

| | NIS1 (avant) | NIS2 (maintenant) |
|---|---|---|
| Entites concernees en France | ~300 | **15 000+** |
| Secteurs | Infrastructures critiques | + PME, collectivites, sante, education, numerique |
| Amendes maximales | Limitees | **10M EUR ou 2% CA mondial** |
| Responsabilite dirigeants | Non | **Oui, personnelle** |
| Securite supply chain | Non exigee | **Obligatoire** |

Ce n'est pas un marche d'adoption volontaire. C'est un marche d'**obligation legale** avec une **deadline dure** et des **sanctions personnelles**. Les organisations ne choisissent pas d'investir — elles y sont contraintes par la loi.

---

## Cible primaire : PME et ETI europeennes sous NIS2

### Profil type

- **Taille :** 50-500 employes
- **Infrastructure :** 10-200 machines (serveurs, postes, edge)
- **Equipe IT :** 2-15 personnes, pas d'equipe compliance dediee
- **Situation :** Soumise a NIS2, utilise actuellement Ansible/scripts manuels/rien de structure, pas de plan de conformite clair
- **Budget IT :** 100k-1M EUR/an
- **Douleur :** "On sait qu'on doit se mettre en conformite, on ne sait pas par ou commencer, et on n'a pas les moyens d'embaucher un RSSI + acheter 5 outils separes"

### Pourquoi c'est un bon fit

1. **Douleur reelle et urgente** — La deadline NIS2 approche, les amendes sont severes, les dirigeants sont personnellement responsables
2. **Budget contraint** — Ils ne peuvent pas se payer Ansible + AWX + SIEM + SBOM + CMDB separement. NixFleet offre tout en un.
3. **Pas d'equipe compliance** — NixFleet rend la conformite native, pas un projet supplementaire
4. **Taille de flotte adaptee** — 10-200 machines, le sweet spot du tier Pro
5. **Sensibilite souverainete** — Les PME/ETI europeennes sont de plus en plus conscientes de la dependance aux outils US

---

## Verticaux prioritaires

### Tier 1 — Marches d'entree (annee 1-2)

#### Recherche & HPC

| | |
|---|---|
| **Driver** | Reproductibilite scientifique + NixOS deja utilise dans certains centres |
| **Cibles FR** | CNRS, CEA, INRIA, universites, centres de calcul |
| **Entree** | NixOS deja connu, la valeur est immediate et technique |
| **Taille de flotte** | 50-500+ machines |

La recherche est le marche d'adoption naturel : NixOS y est deja present, la reproductibilite est une valeur comprise, et les equipes sont techniques. C'est le meilleur terrain pour construire des references.

#### Startups et scale-ups tech

| | |
|---|---|
| **Driver** | Cout + efficacite + SOC2/ISO 27001 |
| **Cibles** | ~15 000 entreprises tech EU |
| **Entree** | Self-serve (tier Community puis Pro), DevOps-friendly |
| **Taille de flotte** | 10-100 machines |

Les startups tech sont le canal de croissance organique : equipes techniques, adoption bottom-up, sensibilite au cout, besoin de compliance (SOC2 pour vendre aux grands comptes). Le tier Community sert de porte d'entree.

### Tier 2 — Marches a forte valeur (annee 2-3)

#### Finance reglementee

| | |
|---|---|
| **Driver** | DORA + NIS2 + exigences ACPR/AMF |
| **Cibles FR** | ~1 200 entites (banques, assurances, fintechs) |
| **Entree** | POC pilote oriente compliance |
| **Taille de flotte** | 50-500 machines |

La finance combine pression reglementaire maximale (NIS2 + DORA) et budgets significatifs. L'entree se fait par un pilote oriente compliance, pas par l'adoption technique.

#### Telecoms et energie

| | |
|---|---|
| **Driver** | Infrastructures critiques NIS2, entites essentielles |
| **Cibles FR** | ~500 entites essentielles |
| **Entree** | Partenariats avec integrateurs |
| **Taille de flotte** | 100-1000+ machines |

### Tier 3 — Marches institutionnels (annee 3+)

#### Collectivites et Etat

| | |
|---|---|
| **Driver** | NIS2 + doctrine cloud souverain ANSSI |
| **Cibles FR** | ~8 000 entites EE/EI |
| **Entree** | Marches publics, qualification ANSSI |
| **Taille de flotte** | Variable |

Le secteur public est le plus grand marche en volume mais le plus lent a penetrer (marches publics, qualification ANSSI). C'est un objectif a moyen terme qui necessite d'abord des references dans le prive.

---

## Geographies prioritaires

### France — Marche de lancement

- **15 000 entites NIS2** — le plus grand marche NIS2 par pays en EU
- **ANSSI** comme regulateur actif (plateforme de pre-enregistrement ouverte depuis novembre 2025)
- **Doctrine cloud de confiance** qui favorise les solutions souveraines
- **Ecosysteme Nix** present (meetups, contributeurs nixpkgs)

### Allemagne — Deuxieme marche

- **BSI** comme regulateur, mandat souverainete numerique fort
- **NIS2UmsuCG** (transposition allemande) en cours
- **Large base entreprise** industrielle et technologique
- **Sensibilite souverainete** historiquement elevee

### Benelux et Nordiques — Expansion naturelle

- **Pays-Bas** — Forte penetration communaute NixOS, marche tech mature
- **Belgique** — Siege des institutions EU, regulations en avance
- **Nordiques** — Haute maturite numerique, conscience souverainete

### Suisse — Marche premium

- **nDSG** (protection des donnees, adjacent NIS2)
- **Finance et pharma** — secteurs a haute valeur
- **Neutralite** — sensibilite extreme a la souverainete

---

## Paysage concurrentiel

### Vide documente

Il n'existe **aucun concurrent direct** sur le segment "gestion de flotte NixOS enterprise en Europe". C'est a la fois une validation (personne n'a capture ce marche) et une opportunite (le marche est ouvert).

| Acteur | Perimetre | Faiblesse exploitable |
|--------|-----------|----------------------|
| **Ansible / Puppet** | Config management imperatif | Derive inevitable, NIS2 couteux |
| **Jamf / Intune** | MDM cloud proprietaire | Lock-in US, RGPD complexe, cher |
| **Determinate Systems** | Nix DX pour developpeurs | Pas de fleet management, pas EU |
| **Colmena / NixOps** | Outils communautaires NixOS | Pas de support, pas d'UX, pas de SLA |
| **Fleet.dm** | MDM open source endpoint | Non NixOS, pas de reproductibilite |
| **Crystal Forge** | POC communautaire Nix compliance | Solo dev, pas de deployment execution |

### Fosse defensif (moat)

L'avantage concurrentiel de NixFleet repose sur quatre couches qui se renforcent :

1. **Avantage paradigmatique** — Le modele fonctionnel de NixOS ne peut pas etre "ajoute" a Ansible. Un concurrent devrait reconstruire toute sa stack.
2. **Capital communautaire** — L'ecosysteme Nix a une memoire longue. Les contributions upstream (nixpkgs, modules) construisent une credibilite durable.
3. **Certifications EU** — ANSSI/BSI prennent 12-24 mois a obtenir. Une fois acquises, c'est une barriere a l'entree significative.
4. **Integration profonde** — Quand la flotte entiere est declaree dans un flake, le cout de migration est organisationnel et cognitif, pas seulement technique.

### Fenetre d'opportunite : 18 mois

La fenetre entre maintenant (mars 2026) et la deadline NIS2 (fin 2027) est le moment optimal :
- Les organisations **commencent a chercher** des solutions
- **Aucun concurrent direct** n'est etabli
- La **pression reglementaire** cree de l'urgence
- Les **budgets compliance** sont en cours d'allocation

Apres 2027, le marche se consolide et les barrieres a l'entree augmentent. Etre present maintenant est critique.

---

## Sources de financement EU

| Source | Type | Pertinence |
|--------|------|------------|
| **BPI France** | Subvention / pret | Deep tech, souverainete |
| **NGI (Next Generation Internet)** | Subvention EU | Infrastructure open source |
| **Horizon Europe** | Subvention EU | Souverainete numerique, cybersecurite |
| **Sovereign Tech Fund** | Fonds EU | Deja investi 226k EUR dans l'ecosysteme Nix (2023) |

---

*[Precedent : Les problemes que nous resolvons](03-problems-we-solve.md) · [Retour au sommaire](README.md) · [Suivant : Le produit](05-the-product.md)*
