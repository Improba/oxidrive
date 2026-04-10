# Arbre de décision de synchronisation

Ce document décrit la **matrice de réconciliation** utilisée par oxidrive : pour chaque chemin relatif, on considère trois vues — **fichier local** (L), **fichier distant** (R), **métadonnées persistées** (M, dernier état connu dans `store`).

La fonction centrale est `determine_action` dans `src/sync/decision.rs` : elle est **purement déterministe** (hors politique de conflit appliquée ensuite dans `conflict`).

---

## Matrice des 12 cas

Les lignes suivantes correspondent aux combinaisons **significatives** couvertes par la logique et les tests (`matrix_1` … `matrix_12`). « Inchangé » signifie : identique au dernier état enregistré dans M (MD5 / mtime local et distant selon les règles du code).

| # | Local (L) | Remote (R) | Meta (M) | Condition / sous-cas | Action |
|---|-----------|------------|----------|----------------------|--------|
| 1 | présent | présent | présent | L et R **inchangés** par rapport à M | **Skip** |
| 2 | présent | présent | présent | L **modifié**, R inchangé | **Upload** (mise à jour du fichier distant lié à M) |
| 3 | présent | présent | présent | R **modifié**, L inchangé | **Download** |
| 4 | présent | présent | présent | L **et** R modifiés | **Conflict** |
| 5 | présent | présent | absent | MD5 égaux (connus des deux côtés) → **Skip** ; sinon (MD5 différents ou MD5 distant absent) → **Conflict** | **Skip** ou **Conflict** |
| 6 | présent | absent | présent | L **inchangé** par rapport à M (fichier supprimé côté Drive) | **DeleteLocal** |
| 7 | présent | absent | présent | L **modifié** (fichier supprimé côté Drive mais édité localement) | **Upload** (recréation distante, `remote_id: None`) |
| 8 | présent | absent | absent | Fichier **nouveau** uniquement local | **Upload** |
| 9 | absent | présent | présent | R **inchangé** par rapport à M (fichier supprimé localement) | **DeleteRemote** |
| 10 | absent | présent | présent | R **modifié** (fichier absent localement mais distant évolué) | **Download** |
| 11 | absent | présent | absent | Fichier **nouveau** uniquement sur Drive | **Download** |
| 12 | absent | absent | présent | Les deux copies ont disparu | **CleanupMetadata** (suppression de l’entrée orpheline en base) |

**Cas limite** : (L absent, R absent, M absent) → **Skip** (rien à faire ; chemin fantôme).

---

## Explication et exemples par cas

### 1 — Rien n’a bougé

Vous avez synchronisé hier ; ni le fichier sur le disque ni la version Drive n’ont changé depuis le dernier enregistrement. **Aucune opération réseau** n’est nécessaire.

### 2 — Vous avez édité en local seulement

Exemple : correction d’un `.txt` ; le MD5/mtime local ne correspond plus à M, alors que Drive est toujours aligné avec la dernière sync. → **Upload** pour pousser votre version.

### 3 — Quelqu’un d’autre (ou vous sur le web) a modifié Drive

Le fichier local est resté comme au dernier sync ; Drive a une nouvelle révision. → **Download** pour réaligner le disque.

### 4 — Conflit d’édition (edit/edit)

Les deux copies ont divergé depuis M. La matrice produit **Conflict** ; la suite dépend de **`conflict_policy`** (`local_wins`, `remote_wins`, `rename` avec suffixe). Exemple : même document ouvert sur deux machines sans sync intermédiaire.

### 5 — Première rencontre L+R sans historique en base

- Si les **MD5** (quand disponibles des deux côtés) sont **identiques**, on considère que le contenu est déjà le même → **Skip** (pas de transfert).
- Si les MD5 **diffèrent** → **Conflict** : oxidrive ne devine pas la vérité sans historique.

### 6 — Suppression sur Drive, fichier local intact

Le fichier a été retiré de Drive mais existe toujours localement **tel qu’au dernier sync**. Politique : refléter la suppression distante → **DeleteLocal** (ou équivalent métier selon les options futures documentées dans le code).

### 7 — Drive supprimé, mais vous avez re-modifié en local

L’entrée M pensait encore à un fichier distant ; le distant n’existe plus ; le local a changé depuis M. → **Upload** pour **recréer** le fichier sur Drive (nouveau fichier côté API si besoin).

### 8 — Nouveau fichier local, jamais vu par oxidrive

Création d’un rapport dans `sync_dir` ; aucune entrée M ni fichier distant. → **Upload**.

### 9 — Vous avez supprimé le fichier localement, Drive inchangé

Alignement : la suppression locale est la source de vérité par rapport à M → **DeleteRemote**.

### 10 — Fichier supprimé localement, mais Drive a été modifié

Exemple : suppression accidentelle locale alors qu’une nouvelle version existe sur Drive. R a changé depuis M → **Download** pour **restaurer** depuis Drive.

### 11 — Nouveau fichier uniquement sur Drive

Colleague a déposé un PDF dans le dossier partagé. → **Download**.

### 12 — Nettoyage de métadonnées

Les deux côtés n’ont plus le fichier, mais M contient encore une entrée (état incohérent résiduel, ou double suppression détectée après coup). → **CleanupMetadata** pour éviter une base polluée.

---

## Fichiers Google natifs (sans MD5)

L’API Google Drive ne fournit **pas toujours** de `md5Checksum` pour les fichiers **nés sur Google** (Docs, Sheets, etc.) ou certains types binaires traités différemment.

Conséquences dans la logique actuelle :

- **Sans M (premier sync)** : si le distant n’a **pas** de MD5, le cas « L+R, pas de meta » tombe en **Conflict** — on ne peut pas prouver l’égalité byte-à-byte avec le local exporté.
- **Avec M** : la détection « remote modifié » s’appuie alors sur **`modifiedTime`** (et l’absence de MD5 côté distant) pour suivre les évolutions, ce qui permet des **Skip** / **Download** cohérents une fois qu’une ligne de métadonnées a été enregistrée après un premier cycle réussi.

Recommandation opérationnelle : après un premier alignement ou export Workspace, laisser au moins un cycle complet mettre à jour **M** pour que les fichiers sans MD5 soient suivis par **horodatage**.

---

## Politique de conflits (`ConflictPolicy`)

Configurable dans `config.toml` :

| Valeur | Comportement typique |
|--------|----------------------|
| `local_wins` | La version **locale** remplace la distante lors d’un conflit explicite. |
| `remote_wins` | La version **Drive** l’emporte ; écrasement ou fusion selon l’implémentation de l’exécuteur. |
| `rename` | Conserver **les deux** copies en renommant l’une (suffixe configuré ; le moteur peut compléter par un horodatage pour garantir l’unicité). |

La matrice **ne choisit pas** entre local et remote pour le cas 4 : elle **signale** `Conflict` ; c’est le module `sync/conflict` qui matérialise la résolution selon la politique.

Pour les conventions de code et les tests associés à cette matrice, voir [../conventions/code-style.md](../conventions/code-style.md).
