# Les Problemes que Nous Resolvons

*Quatre crises. Une architecture.*

---

## Vue d'ensemble

Les entreprises europeennes font face a quatre problemes structurels dans la gestion de leur infrastructure informatique. Ces problemes ne sont pas independants — ils se renforcent mutuellement, et les outils actuels les traitent separement, avec des solutions partielles et couteuses.

NixFleet les resout ensemble, parce que la resolution de l'un entraine mecaniquement la resolution des autres. C'est la consequence d'un choix architectural, pas d'un effort d'integration.

---

## 1. Souverainete

### Le probleme

La majorite des outils de gestion d'infrastructure sont des services cloud americains. Jamf, Microsoft Intune, Red Hat Satellite, AWS Systems Manager — tous font de votre capacite a deployer et gerer votre infrastructure une **dependance d'un tiers**.

Concretement :
- Votre configuration vit dans **leur** base de donnees
- Votre historique d'audit est accessible via **leur** API
- Votre capacite a deployer depend de **leur** disponibilite
- Vos donnees sont soumises au **Cloud Act** americain
- Si le fournisseur change ses conditions, augmente ses prix, ou ferme, vous etes bloque

Pour les entites europeennes soumises a NIS2, au RGPD, ou a des doctrines de souverainete (SecNumCloud en France, C5 en Allemagne), cette dependance est un risque reglementaire et operationnel.

### La reponse NixFleet

NixFleet est **integralement auto-hebergeable**. Chaque composant tourne sur votre infrastructure :

| Composant | Solution NixFleet | Dependance externe |
|-----------|-------------------|-------------------|
| Moteur core | Open source (MIT) | Aucune |
| Control plane | Module NixOS, votre serveur | Aucune |
| Cache binaire | Attic sur votre S3 | Aucune |
| Gestion des secrets | agenix / sops-nix | Aucune |
| Historique de configuration | Votre depot Git | Aucune |

**Si NixFleet disparait demain, votre flotte continue de fonctionner.** Chaque machine est un systeme NixOS standard. Les outils natifs NixOS (`nixos-rebuild`, `nixos-anywhere`) suffisent pour operer. C'est une contrainte de conception, pas un argument marketing.

---

## 2. Securite

### Le probleme

La securite de l'infrastructure traditionnelle repose sur des **couches ajoutees** : antivirus, EDR, SIEM, scanners de vulnerabilites, outils SBOM. Ces outils surveillent et documentent — mais ils ne changent pas les proprietes fondamentales du systeme qu'ils protegent.

Le probleme structurel :
- **Derive de configuration** — Apres quelques mois, l'etat reel du systeme ne correspond plus a sa documentation. Des ports sont ouverts, des services non documentes tournent, des fichiers de configuration ont ete modifies manuellement.
- **Persistance des menaces** — Un malware installe dans `/tmp`, `/var`, ou un repertoire utilisateur survit aux mises a jour et parfois aux redemarrages.
- **Chaine d'approvisionnement opaque** — Les dependances exactes d'un systeme en production sont rarement connues avec precision. Les SBOM sont generes par des outils tiers qui approximent.
- **Binaires non verifies** — Sur un systeme traditionnel, un binaire peut etre modifie sans que le systeme le detecte.

### La reponse NixFleet

La securite NixFleet n'est pas une couche ajoutee — elle **emerge de l'architecture** :

**Nix Store adresse par hash SHA-256.** Chaque binaire dans le store est identifie par le hash cryptographique de toutes ses entrees — code source, compilateur, dependances, flags de compilation. Modifier un binaire change son hash, ce qui le rend physiquement impossible a substituer. Ce n'est pas de la verification — c'est de l'immutabilite.

**Impermanence.** Au redemarrage, le systeme de fichiers root est efface. Seuls les fichiers explicitement declares comme persistants survivent. Un malware installe n'importe ou sur le systeme est automatiquement supprime au prochain reboot. La fenetre de persistance est reduite a la duree entre deux redemarrages.

**SBOM automatique.** Le fichier `flake.lock` contient la liste exacte de chaque dependance et son hash. La generation d'un SBOM (CycloneDX ou SPDX) est automatique et exhaustive — pas une approximation.

**Chiffrement integral.** LUKS est disponible de maniere declarative pour le chiffrement disque complet.

**Configuration verifiable.** L'etat du systeme est defini par un fichier de configuration versionne dans Git. Pour verifier qu'un serveur est conforme, il suffit de comparer son hash de generation avec celui attendu.

---

## 3. Reproductibilite

### Le probleme

La reproductibilite est le probleme le plus sous-estime de l'infrastructure. La plupart des organisations ne realisent son importance que lors d'un incident :

- **"Reconstruisez ce serveur a l'identique"** — Impossible si la configuration a derive sur 18 mois
- **"Qu'est-ce qui a change entre hier et aujourd'hui ?"** — Les outils de monitoring detectent les symptomes, pas les causes
- **"Deployez la meme configuration sur les 50 nouveaux serveurs"** — Le playbook Ansible fonctionne sur les machines existantes mais pas sur les nouvelles, parce qu'il depend d'un etat preexistant
- **"Prouvez que ces 200 serveurs sont identiques"** — Impossible a garantir avec des outils imperatifs

La derive de configuration n'est pas un bug — c'est une **propriete inherente** des systemes imperatifs. Chaque intervention manuelle, chaque mise a jour partielle, chaque hotfix accumule de la dette d'etat.

### La reponse NixFleet

La reproductibilite NixOS n'est pas un objectif — c'est une **propriete mathematique** :

**Determinisme.** Le meme `flake.nix` + `flake.lock` produit le meme systeme sur n'importe quelle machine. Ce n'est pas "la meme configuration" — c'est le meme systeme, identifie par le meme hash cryptographique.

**Derive impossible.** Il n'y a pas de concept de "modifier le systeme en dehors de la configuration". L'etat du systeme est une fonction pure de sa declaration. Avec l'impermanence, meme les modifications manuelles sont effacees au redemarrage.

**Reconstruction instantanee.** Reconstruire un serveur a l'identique = appliquer la meme configuration. Le temps de reconstruction est le temps de telechargement des paquets depuis le cache binaire, typiquement quelques minutes.

**Preuve d'identite.** Pour prouver que 200 serveurs sont identiques : ils partagent le meme hash de generation. C'est une preuve cryptographique, pas une assertion.

---

## 4. Conformite NIS2

### Le probleme

La directive NIS2 (EU 2022/2555), transposee en France par la Loi Resilience, entre en application fin 2027. Elle concerne :

- **15 000+ entites francaises** (contre ~300 sous NIS1)
- **Collectivites territoriales, PME, organismes publics**, en plus des grands groupes
- **Amendes jusqu'a 10M EUR ou 2% du chiffre d'affaires mondial**
- **Responsabilite personnelle des dirigeants**

Les obligations centrales :

| Obligation | Ce que NIS2 exige |
|-----------|-------------------|
| **Tracabilite** | Pouvoir prouver chaque modification apportee au systeme d'information |
| **Reprise rapide** | Capacite de reprise apres incident en moins de 24 heures |
| **Chaine d'approvisionnement** | Controler et documenter toutes les dependances logicielles |
| **Inventaire** | Maintenir un inventaire complet et a jour des actifs numeriques |
| **Continuite** | Plans de continuite d'activite testes et operationnels |

Pour les organisations qui utilisent des outils traditionnels, satisfaire ces obligations necessite des **investissements separees** : SIEM pour la tracabilite (+30k EUR/an), outils SBOM pour la supply chain, CMDB pour l'inventaire, plans DR pour la reprise. Le cout total est significatif et le resultat est souvent partiel.

### La reponse NixFleet

Pour une organisation NixFleet, **chaque obligation NIS2 est satisfaite par l'architecture elle-meme** :

| Obligation NIS2 | Approche classique | NixFleet | Statut |
|-----------------|-------------------|----------|--------|
| Tracabilite des modifications SI | SIEM + outils separes, +30k EUR/an | Chaque changement = commit Git signe. Cout : 0 EUR. | Natif |
| Reprise apres incident en 24h | Runbooks manuels, tests annuels, incertain | Rollback atomique < 90 secondes vers generation validee | Natif |
| Securite chaine d'approvisionnement | Outils SBOM separes, integration manuelle | SBOM CycloneDX/SPDX auto depuis flake.lock | Natif |
| Inventaire des actifs numeriques | CMDB couteuse, souvent inexacte | Inventaire complet dans nixosConfigurations | Natif |
| Notification incident < 24h | Difficile a documenter precisement | Etat machine prouvable a tout instant historique | Natif |
| Continuite d'activite | Plans DR complexes, tests annuels | Generation precedente = plan de reprise immediat | Natif |

**La conformite n'est pas un effort additionnel — c'est un produit derive de l'architecture.**

### Le calcul economique

Pour une PME de 100 machines :

| Poste | Approche classique | NixFleet |
|-------|-------------------|----------|
| Outils compliance (SIEM, SBOM, CMDB) | 30-80k EUR/an | Inclus |
| Temps ingenieur compliance | 0.5-1 ETP | ~0.1 ETP |
| Licences MDM/config management | 20-50k EUR/an | Tier Pro NixFleet |
| Audit externe (preparation) | 20-40k EUR | Significativement reduit |
| **Total annuel** | **100-200k EUR** | **30-36k EUR** |

Le facteur de reduction est de **3 a 5x**, sans compromis sur la qualite de la conformite — au contraire, la conformite NixFleet est superieure parce qu'elle est prouvable, pas simplement declaree.

---

## La synergie des quatre reponses

Ces quatre problemes ne sont pas resolus independamment — ils sont resolus par la **meme architecture** :

```
Langage fonctionnel pur (Nix)
    └── Reproductibilite deterministe
         ├── Securite (hash store, impermanence, SBOM)
         ├── Souverainete (auto-hebergeable, pas de dependance)
         └── Conformite NIS2 (tracabilite, reprise, supply chain)
```

Resoudre la reproductibilite resout mecaniquement la securite (binaires verifiables), la tracabilite (historique Git), et la souverainete (pas besoin d'un tiers pour garantir l'etat du systeme). C'est cette synergie qui rend la proposition NixFleet **defensible a long terme et difficile a copier** — parce qu'on ne peut pas "ajouter" la reproductibilite deterministe a un outil imperatif.

---

*[Precedent : Pourquoi Nix ?](02-why-nix.md) · [Retour au sommaire](README.md) · [Suivant : Marche cible](04-target-market.md)*
