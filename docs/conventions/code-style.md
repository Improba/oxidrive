# Conventions de code

Ce document fixe les règles de style et de qualité pour le dépôt **oxidrive** (Rust).

---

## Gestion des erreurs

- Utiliser **`thiserror`** pour définir un type d’erreur principal (ex. `OxidriveError`) avec des variantes explicites et des messages stables côté utilisateur quand c’est pertinent.
- Les fonctions publiques et la plupart des fonctions internes retournent **`Result<T, E>`** (ou un alias `crate::error::Result<T>`) plutôt que de paniquer.
- **Éviter `unwrap()` et `expect()`** en code de production ; réserver ces appels aux tests, aux invariants documentés, ou aux cas où l’échec est structurellement impossible (avec commentaire bref si nécessaire).
- Propager les erreurs avec **`?`** ; ajouter du contexte (`map_err`, chaînage d’erreurs) lorsque la pile d’appels seule ne suffit pas à diagnostiquer.

---

## Nommage

- **Fonctions, variables, modules** : `snake_case`.
- **Types, traits, enums** : `CamelCase` (PascalCase).
- **Constantes** : `SCREAMING_SNAKE_CASE`.
- Préférer des noms **verbeux mais clairs** pour les fonctions qui effectuent des effets (`download_file`, `open_database`) plutôt que des abréviations obscures.

---

## Documentation

- Tout **item public** (crate, modules `pub`, structs, enums, traits, fonctions, champs publics significatifs) doit avoir une **documentation `rustdoc`** (`///`) expliquant le rôle, les invariants importants et, si utile, un exemple court.
- Les modules peuvent commencer par un commentaire `//!` de module lorsque le regroupement mérite une introduction.
- Garder les commentaires **alignés sur le code** : mettre à jour ou supprimer un commentaire devenu faux.

---

## Tests

- Les tests unitaires vivent **dans le même fichier** que le code testé, sous `#[cfg(test)] mod tests { ... }`.
- Préférer des **tests ciblés** par fonction ou par cas de la matrice de sync (comme dans `decision.rs`) plutôt que de gros tests monolithiques.
- Pour les dépendances réseau ou le FS, utiliser des **doubles** (`tempfile`, `wiremock`, etc.) lorsque c’est faisable pour garder la CI rapide et déterministe.

---

## Logging

- Utiliser le crate **`tracing`** (`tracing::info!`, `debug!`, `warn!`, `error!`) plutôt que `println!` pour tout ce qui concerne le diagnostic ou le suivi d’exécution.
- Choisir le **niveau** de façon cohérente :
  - **error** : échec empêchant une opération ou une sync ; nécessite l’attention de l’utilisateur.
  - **warn** : situation anormale mais récupérable (retry, fichier ignoré, dépassement de quota soft).
  - **info** : jalons utilisateur (début/fin de sync, nombre de fichiers traités).
  - **debug** / **trace** : détails pour le développement ou le support (requêtes, chemins, états intermédiaires).
- Respecter la configuration **`RUST_LOG`** et les flags CLI (`--verbose`, `--quiet`) exposés via `tracing-subscriber`.

---

## Concurrence

- Le runtime async par défaut est **Tokio** (multi-thread) pour les opérations réseau et l’orchestration.
- Les accès à **redb** (et plus généralement tout I/O disque bloquant lourd) doivent éviter de bloquer indéfiniment le runtime : encapsuler dans **`tokio::task::spawn_blocking`** (ou équivalent documenté) lorsque l’appel est synchrone et potentiellement lent.
- Partager l’état entre tâches avec des primitives sûres (`Arc`, canaux, types `Send`/`Sync` appropriés) ; documenter toute contrainte de thread si un type n’est pas `Sync`.

---

## Formatage et clippy

- Le code doit passer **`cargo fmt`** sans diff.
- **`cargo clippy`** avec `-D warnings` est la cible sur les PR (voir le workflow Git).

Pour le flux Git et la revue de code, voir [git-workflow.md](git-workflow.md).
