# Pourquoi Nix ?

*Le changement de paradigme qui rend NixFleet possible.*

---

## Le probleme fondamental de l'infrastructure actuelle

Depuis vingt ans, la gestion d'infrastructure repose sur un modele **imperatif** : on donne des instructions au systeme. "Installe ce paquet. Modifie ce fichier. Redemarre ce service." Des outils comme Ansible, Puppet, et Chef automatisent ces instructions — mais ils ne changent pas le paradigme.

Le probleme est structurel : **le resultat d'une instruction depend de l'etat existant du systeme**. Un `apt install nginx` sur une machine fraiche et un `apt install nginx` sur une machine qui tourne depuis deux ans ne produisent pas le meme resultat. Les dependances sont differentes. Les fichiers de configuration ont ete modifies. Des restes d'installations precedentes interferent.

C'est ce que les praticiens appellent la **derive de configuration** — et elle est inevitable avec les outils actuels.

---

## Le paradigme declaratif de Nix

Nix renverse l'approche. Au lieu de decrire *comment* atteindre un etat, on declare *quel* etat on veut :

### Imperatif (Ansible, scripts)

```
1. Installer nginx 1.24
2. Copier le fichier de config dans /etc/nginx/
3. Ouvrir le port 443 dans le pare-feu
4. Demarrer le service nginx
5. Activer le demarrage automatique
```

*Si l'etape 3 echoue, le systeme est dans un etat indetermine. Si le pare-feu a deja une regle conflictuelle, le resultat est imprevisible.*

### Declaratif (NixOS)

```
Le systeme doit avoir :
  - nginx 1.24 avec cette configuration
  - le port 443 ouvert
  - nginx demarre automatiquement
```

*NixOS calcule l'etat final desire et l'applique de maniere atomique. Peu importe l'etat de depart. Le resultat est toujours identique.*

---

## Les cinq garanties de Nix

### 1. Reproductibilite deterministe

Le meme fichier de configuration, evalue sur n'importe quelle machine, produit le meme systeme d'exploitation, bit pour bit. Ce n'est pas une approximation — c'est une propriete mathematique garantie par le hash cryptographique de chaque composant.

**Consequence business :** Quand un auditeur demande "pouvez-vous prouver que vos 200 serveurs sont configures de maniere identique ?", la reponse est un hash. Pas un rapport genere par un outil tiers. Un hash cryptographique unique qui prouve l'identite mathematique.

### 2. Atomicite

Les deployments NixOS sont **atomiques** : soit le nouveau systeme s'active entierement, soit rien ne change. Il n'existe pas d'etat "a moitie deploye". Le basculement entre l'ancien et le nouveau systeme est un changement de pointeur — instantane et reversible.

**Consequence business :** Les fenetres de maintenance passent de heures a secondes. Le risque de deploiement tombe a zero — parce que le pire cas est un rollback instantane vers l'etat precedent valide.

### 3. Rollback instantane

Chaque deploiement cree une "generation" — un snapshot complet et immuable du systeme. Revenir a n'importe quelle generation precedente est instantane et garanti de fonctionner, parce que la generation precedente n'a pas ete modifiee.

**Consequence business :** La NIS2 exige une "reprise apres incident en 24 heures". Avec NixOS, la reprise prend moins de 90 secondes. Ce n'est pas un plan de reprise — c'est un mecanisme integre.

### 4. Tracabilite complete

Parce que chaque configuration est du code dans un depot Git, chaque changement a l'infrastructure est :
- **Versionne** — qui a change quoi, quand
- **Revise** — chaque changement peut passer par une revue de code
- **Reproductible** — on peut reconstruire l'etat exact du systeme a n'importe quel point dans le temps
- **Signe** — chaque commit peut etre signe cryptographiquement

**Consequence business :** La tracabilite NIS2 n'est pas un effort supplementaire — elle est un sous-produit du workflow normal de developpement.

### 5. Securite de la chaine d'approvisionnement

Le fichier `flake.lock` epingle la version exacte et le hash cryptographique de chaque dependance — y compris toutes les dependances transitives. Le SBOM (Software Bill of Materials) est genere automatiquement depuis ce fichier.

**Consequence business :** La securite de la chaine d'approvisionnement, obligation NIS2, est satisfaite par le workflow standard Nix. Pas d'outil tiers a integrer, pas de processus supplementaire a maintenir.

---

## Pourquoi pas seulement Nix ?

Si NixOS fournit toutes ces garanties, pourquoi NixFleet ?

Parce que NixOS est un outil d'infrastructure, pas un produit d'entreprise. NixOS donne les briques — NixFleet construit le batiment :

| Besoin enterprise | NixOS seul | NixFleet |
|-------------------|-----------|----------|
| Deployer 200 machines | Scripts custom, SSH, outillage ad hoc | Control plane centralise, un clic |
| Savoir l'etat de chaque machine | Aucune visibilite centralisee | Dashboard temps reel |
| Journal d'audit pour compliance | Manuel (git log) | Audit trail structure, export CSV |
| Rollback de flotte | Machine par machine, SSH | Rollback flotte entiere, < 90s |
| Authentification operateur | A construire soi-meme | mTLS + API keys, RBAC |
| Support commercial | Communaute uniquement | SLA, support dedie, formation |
| Detection de derive | Impossible a l'echelle | Automatique, alertes |

NixFleet n'ajoute pas les garanties — NixOS les fournit deja. NixFleet les **orchestre, les expose, et les rend operationnelles** a l'echelle d'une flotte d'entreprise.

---

## Le fossile et la fusee

Les outils traditionnels (Ansible, Puppet) ont ete concus dans les annees 2000-2010 pour un monde ou la gestion de configuration etait un probleme d'automatisation. Ils automatisent des commandes imperatives.

Nix a ete concu comme un probleme de **science informatique** — derivation deterministe de systemes a partir de descriptions formelles. Les garanties de Nix ne sont pas des fonctionnalites ajoutees — elles sont des proprietes mathematiques du modele.

La difference entre les deux approches n'est pas quantitative (Nix fait la meme chose mais mieux). Elle est **qualitative** (Nix fait quelque chose de fondamentalement different). C'est cette difference qualitative qui permet a NixFleet d'offrir des garanties qu'aucun outil concurrent ne peut reproduire — parce qu'elles ne peuvent pas etre "ajoutees" a un outil imperatif.

---

*[Precedent : Qu'est-ce que Nix ?](01-what-is-nix.md) · [Retour au sommaire](README.md) · [Suivant : Les problemes que nous resolvons](03-problems-we-solve.md)*
