# Workflow Git

Conventions pour les branches, les messages de commit, les pull requests et l’intégration continue du projet **oxidrive**.

---

## Branches

| Type | Convention | Usage |
|------|------------|--------|
| Principale | `main` | Toujours déployable ou « mergeable » ; protégée en revue obligatoire si le dépôt le permet. |
| Fonctionnalité | `feature/<nom-court>` | Ex. `feature/drive-changes-pagination`, `feature/markdown-index`. |
| Correctif | `fix/<nom-court>` | Ex. `fix/oauth-refresh-race`, `fix/redb-lock-timeout`. |

Éviter les commits directs sur `main` lorsque la politique du dépôt impose des PR. Les branches de release (`release/x.y`) peuvent être ajoutées plus tard si besoin.

---

## Commits

Adopter un style **proche de Conventional Commits** (préfixe en minuscules + description) :

| Préfixe | Exemple | Quand l’utiliser |
|---------|---------|------------------|
| `feat:` | `feat: ajouter la commande status` | Nouvelle fonctionnalité visible utilisateur ou API publique. |
| `fix:` | `fix: corriger le debounce du watcher` | Correction de bug. |
| `docs:` | `docs: compléter l’arbre de décision` | Documentation uniquement. |
| `refactor:` | `refactor: extraire le client Drive` | Restructuration sans changement de comportement voulu. |
| `test:` | `test: couvrir le cas CleanupMetadata` | Ajout ou correction de tests. |
| `chore:` | `chore: mettre à jour les dépendances` | Maintenance (CI, deps, scripts). |

**Bonnes pratiques** :

- Un commit = une intention **lisible** ; éviter les « WIP » sur `main`.
- Corps de message optionnel mais utile pour expliquer le *pourquoi* (contexte, trade-off).
- Référencer une issue ou un ticket (`Closes #123`) quand c’est pertinent.

---

## Pull requests

1. **Titre** : clair, en français ou anglais selon la convention du dépôt (rester cohérent avec l’historique).
2. **Description** : objectif, résumé des changements, points de revue (risques perf, compat config).
3. **Taille** : préférer des PR **petites à moyennes** pour faciliter la relecture.
4. **Tests** : indiquer ce qui a été exécuté localement (`cargo test`, `cargo clippy`).
5. **Breaking changes** : les signaler explicitement dans la description et, si besoin, dans le changelog.

---

## CI/CD

Objectif typique pour le pipeline (GitHub Actions, GitLab CI, etc.) :

| Étape | Commande / action |
|-------|-------------------|
| Format | `cargo fmt --check` |
| Lint | `cargo clippy --all-targets -- -D warnings` |
| Tests | `cargo test` |
| Build release (optionnel) | `cargo build --release` sur les cibles supportées |

Les badges du README peuvent pointer vers ce pipeline une fois configuré. Les releases binaires peuvent être produites par un job dédié (matrice OS/arch) après tag de version.

Pour le style de code détaillé, voir [code-style.md](code-style.md).
