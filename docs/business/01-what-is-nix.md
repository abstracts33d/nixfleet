# Qu'est-ce que Nix ?

*Une introduction non technique a la technologie qui fonde NixFleet.*

---

## Trois technologies, un ecosysteme

Le mot "Nix" designe trois choses distinctes qui fonctionnent ensemble. Comprendre cette distinction est essentiel pour saisir pourquoi NixFleet est possible — et pourquoi aucun outil concurrent ne peut reproduire ses garanties.

---

## 1. Nix, le langage

Nix est un **langage de programmation fonctionnel pur**. Concretement, cela signifie :

- **Pas de variables mutables** — Une valeur declaree ne change jamais. Il n'y a pas de "mise a jour" d'une variable.
- **Pas d'effets de bord** — Le resultat d'une expression Nix depend uniquement de ses entrees, jamais de l'etat du systeme ou du moment de l'execution.
- **Determinisme** — La meme expression Nix, evaluee sur n'importe quelle machine, produit toujours le meme resultat.

### Pourquoi ca compte pour une entreprise

Ces proprietes ne sont pas des curiosites academiques. Elles signifient que la description de votre infrastructure est **mathematiquement reproductible**. Quand un ingenieur ecrit une configuration Nix, le resultat n'est pas une approximation — c'est une garantie. Deux machines qui partagent la meme configuration sont identiques, bit pour bit, aujourd'hui ou dans trois ans.

### Analogie

Imaginez une recette de cuisine ou chaque ingredient est pese au milligramme pres, ou la temperature est controlee au degre pres, et ou le resultat est garanti identique a chaque fois que vous la suivez. C'est ce que le langage Nix fait pour l'infrastructure informatique.

---

## 2. Nix, le gestionnaire de paquets

Au-dessus du langage, Nix est un **gestionnaire de paquets universel** — le plus grand au monde :

### Les chiffres

- **100 000+ paquets** dans nixpkgs, le depot central (plus que Debian, Homebrew, ou tout autre gestionnaire)
- **Mise a jour continue** — nixpkgs recoit des milliers de contributions par semaine
- **Multi-plateforme** — Linux, macOS, et bientot plus

### Ce qui le rend unique

Contrairement a apt (Debian), yum (RedHat), ou Homebrew (macOS), le gestionnaire Nix resout trois problemes fondamentaux :

**Isolation des paquets.** Chaque paquet est installe dans un chemin unique, determine par le hash SHA-256 de toutes ses dependances (y compris les dependances de ses dependances). Deux versions du meme logiciel coexistent sans conflit. Il est physiquement impossible qu'une mise a jour casse un autre logiciel.

**Reproductibilite.** Un fichier `flake.lock` epingle la version exacte de chaque paquet et de chaque dependance transitive. Ce verrou garantit que le meme ensemble de logiciels est installe, partout, toujours.

**Rollback instantane.** Chaque operation cree une nouvelle "generation". Revenir a l'etat precedent est instantane — il suffit d'activer la generation precedente. Pas de desinstallation, pas de nettoyage, pas de risque.

### Les devshells : un environnement de developpement reproductible

Un des usages les plus immediats de Nix en entreprise est le **devshell** (environnement de developpement). Au lieu de demander a chaque developpeur d'installer manuellement Python 3.11, PostgreSQL 15, et Redis 7, un fichier Nix declare ces dependances :

```
# Conceptuellement (simplifie)
Environnement de developpement :
  - Python 3.11.8
  - PostgreSQL 15.6
  - Redis 7.2.4
  - Outils : formatters, linters, tests
```

Un nouveau developpeur qui rejoint l'equipe execute une seule commande et obtient exactement le meme environnement que tous ses collegues. Plus de "ca marche sur ma machine". Plus de journees perdues a installer des dependances.

---

## 3. NixOS, le systeme d'exploitation

NixOS est un **systeme d'exploitation Linux entierement configure en Nix**. C'est la ou la puissance du langage et du gestionnaire de paquets atteint son plein potentiel.

### Tout est declare

Dans un systeme Linux traditionnel (Ubuntu, Debian, RHEL), la configuration est **dispersee** : des fichiers dans `/etc/`, des paquets installes a la main, des services configures via des commandes, des reglages accumules au fil du temps. L'etat reel du systeme est le produit de tout ce qui s'est passe depuis son installation. Personne ne peut le reproduire exactement.

Dans NixOS, **tout le systeme est declare dans un seul fichier de configuration** :

- Le noyau Linux et ses modules
- Les services systeme (SSH, pare-feu, serveur web, base de donnees)
- Les utilisateurs et leurs permissions
- La configuration reseau
- Les paquets installes
- Le partitionnement disque (via disko)

### Ce que ca change

| Propriete | Linux traditionnel | NixOS |
|-----------|-------------------|-------|
| Installer un nouveau serveur | Heures de configuration manuelle | Une commande, 10 minutes |
| Reproduire un serveur a l'identique | Pratiquement impossible | Garanti par construction |
| Revenir en arriere apres un probleme | Restauration backup, risquee | Rollback atomique, 90 secondes |
| Savoir ce qui tourne sur un serveur | Audit manuel, souvent incomplet | Tout est dans la configuration |
| Deriver de la configuration prevue | Inevitable apres quelques mois | Impossible par construction |

### L'impermanence : la securite par l'oubli

NixOS permet une fonctionnalite unique appelee **impermanence** : au redemarrage, tout l'etat non explicitement declare est efface. Le systeme de fichiers root est remis a zero. Seuls les fichiers et repertoires explicitement declares comme persistants survivent.

Cela signifie :
- Un malware qui s'installe dans le systeme est **automatiquement supprime** au prochain redemarrage
- Toute modification non autorisee est **effacee**
- L'etat reel du systeme correspond **toujours** a sa declaration

---

## L'ecosysteme Nix en un regard

```
┌─────────────────────────────────────────────────┐
│                    NixOS                         │
│         Systeme d'exploitation complet           │
│    (noyau, services, reseau, utilisateurs)       │
├─────────────────────────────────────────────────┤
│              Gestionnaire Nix                    │
│   100 000+ paquets · isolation · rollback        │
│     devshells · flake.lock · generations         │
├─────────────────────────────────────────────────┤
│              Langage Nix                         │
│   Fonctionnel pur · deterministe · sans effets   │
│     de bord · reproductibilite mathematique      │
└─────────────────────────────────────────────────┘
```

Chaque couche herite des garanties de la couche inferieure. Le systeme d'exploitation est reproductible parce que le gestionnaire de paquets est reproductible. Le gestionnaire de paquets est reproductible parce que le langage est deterministe. C'est une chaine de garanties ininterrompue, du code source jusqu'au systeme deploye.

---

## Maturite et adoption

Nix n'est pas une technologie experimentale :

- **Cree en 2003** par Eelco Dolstra a l'Universite d'Utrecht (these de doctorat)
- **20+ ans de developpement** continu
- **Utilise en production** par des organisations comme l'Agence Spatiale Europeenne, CERN, Shopify, Replit, Target, Hercules CI
- **Communaute active** de milliers de contributeurs (nixpkgs est le depot le plus actif de GitHub)
- **Finance par le Sovereign Tech Fund** de l'UE (226k EUR en 2023 pour l'ecosysteme Nix)
- **NixCon** — conference annuelle dediee, en Europe

La technologie est mature. Ce qui manquait, c'est un produit commercial qui la rende accessible a l'echelle d'une flotte d'entreprise. C'est exactement ce que NixFleet construit.

---

*[Retour au sommaire](README.md) · [Suivant : Pourquoi Nix ?](02-why-nix.md)*
