# Phase 4 — Index Markdown du contenu des documents

## Objectif

Générer et maintenir un dossier local `.index/` contenant une version **Markdown** du contenu textuel de chaque document synchronisé. Ce dossier permet la **recherche plein texte** locale (`ripgrep`, `fzf`), l'**indexation par des outils d'IA** (RAG, embeddings), et une **prévisualisation légère**.

---

## Statut actuel : ✅ TERMINÉE (P4-1 helper API optionnel)

| Composant | État | Détail |
|-----------|------|--------|
| Extraction .docx → Markdown | ✅ | zip + quick-xml, paragraphes extraits |
| Extraction .xlsx → Markdown | ✅ | Première feuille, tableau Markdown (best-effort) |
| Extraction .pptx → Markdown | ✅ | Slides numérotées, texte `a:t` |
| Extraction .csv → Markdown | ✅ | Tableau Markdown, limite 500 lignes |
| Extraction PDF → Markdown | ✅ | `pdf-extract` dans `index/pdf.rs` |
| Export Markdown via API Drive | 🟡 | Pas de helper dédié `export_as_markdown` ; export via `export_file` + formats existants |
| Générateur d'index | ✅ | `update_index()` dispatch par extension, atomic write |
| Exclusion `.index/` du scan | ✅ | `scan.rs` ignore `.index/` |
| Intégration avec sync engine | ✅ | `update_index` appelé depuis `engine.rs` quand `index_dir` est configuré |
| Passthrough .txt/.md | ✅ | Lecture texte dans `generator.rs` |
| Métadonnées binaires | ✅ | Fiche Markdown pour extensions non gérées (`generator.rs`) |

---

## Prérequis

- Phase 1 complétée (sync fonctionnelle)
- Phase 3 recommandée (pour l'export Markdown natif des Google Docs)

---

## Structure cible

```
~/GoogleDrive/
├── Projets/
│   ├── Proposition.docx      ← fichier synchronisé
│   ├── Budget.xlsx            ← fichier synchronisé
│   └── Photo.png              ← fichier binaire
└── .index/
    └── Projets/
        ├── Proposition.md     ← contenu extrait en Markdown
        ├── Budget.md          ← tableau Markdown des données
        └── Photo.png.md       ← métadonnées uniquement (nom, taille, MIME, date)
```

---

## Sources de conversion

| Type de fichier | Méthode d'extraction | Qualité | Statut |
|----------------|---------------------|---------|--------|
| Google Docs (via API) | Export `text/markdown` natif via Drive API | Haute | 🟡 (via `export_file` / formats ; pas de helper dédié P4-1) |
| `.docx` | Parsing XML interne (`zip` + `quick-xml`) | Moyenne | ✅ |
| `.xlsx` / `.csv` | Extraction cellules → tableau Markdown | Moyenne | ✅ |
| `.pptx` | Texte des slides → sections numérotées | Moyenne | ✅ |
| `.pdf` | `pdf-extract` (best-effort, sans OCR) | Basse | ✅ |
| `.txt` / `.md` | Lecture / passthrough | Haute | ✅ |
| Images / binaires | Fiche métadonnées (nom, taille, date, MIME) | Info | ✅ |

---

## Matrice de tâches

| ID | Tâche | Fichier(s) | Input | Output | Critère de complétion | Dépendances | Complexité | Statut |
|----|-------|-----------|-------|--------|----------------------|-------------|------------|--------|
| **P4-1** | Export Markdown via API | `src/drive/download.rs` | P1-2 | `export_as_markdown(drive_id)` → String | Test mock : Markdown retourné | P1-2 | Faible | 🟡 (l'export `text/markdown` existe via `export_file` + `export_format`, mais pas de helper dédié `export_as_markdown`) |
| **P4-2** | Extraction .docx | `src/index/docx.rs` | crates `zip` + `quick-xml` | `docx_to_markdown(path)` → String | Test : .docx connu → Markdown attendu | P0-1 | Moyenne | ✅ |
| **P4-3** | Extraction .xlsx | `src/index/xlsx.rs` | crates `zip` + `quick-xml` | `xlsx_to_markdown(path)` → String | Test : .xlsx connu → tableau Markdown | P0-1 | Moyenne | ✅ |
| **P4-4** | Extraction .pptx | `src/index/pptx.rs` | crates `zip` + `quick-xml` | `pptx_to_markdown(path)` → String | Test : .pptx connu → sections numérotées | P0-1 | Moyenne | ✅ |
| **P4-5** | Extraction PDF | `src/index/pdf.rs` | `pdf-extract` | `pdf_to_markdown(path)` → String | Test : PDF texte connu → contenu extrait | P0-1 | Moyenne | ✅ |
| **P4-6** | Extraction .csv | `src/index/csv_extract.rs` | crate `csv` | `csv_to_markdown(path)` → String | Test : CSV → tableau Markdown correct | P0-1 | Faible | ✅ |
| **P4-7** | Passthrough .txt/.md + métadonnées binaires | `src/index/generator.rs` | — | .txt/.md → lecture ; binaires → fiche métadonnées | Test : .txt copié, .png → métadonnées | P0-1 | Faible | ✅ |
| **P4-8** | Générateur d'index | `src/index/generator.rs` | P4-1..P4-7 | `update_index(changed_files, index_dir)` | Test : ajout, modif, suppression reflétés | P4-1..P4-7 | Moyenne | ✅ |
| **P4-9** | Exclusion `.index/` de la sync | `src/sync/scan.rs`, config | — | Le scan local ignore `.index/` | Test : fichiers dans `.index/` jamais syncés | P1-7 | Faible | ✅ |
| **P4-10** | Intégration engine | `src/sync/engine.rs` | P4-8, P1-11 | Après un sync, `update_index` appelé si `index_dir` | Sync + index mis à jour | P4-8, P1-11 | Faible | ✅ |

---

## Graphe de dépendances

```
P4-1 (API export) ──────┐
P4-2 (docx) ✅ ─────────┤
P4-3 (xlsx) ✅ ─────────┤
P4-4 (pptx) ✅ ─────────┤
P4-5 (pdf) ✅ ──────────┤
P4-6 (csv) ✅ ──────────┤
P4-7 (txt/md/binaires) ✅ ┤
                          └──→ P4-8 (générateur) ──→ P4-10 (intégration engine)
P4-9 (exclusion) ✅ ─────────────────────────────────┘
```

**Parallélisables** : P4-1 à P4-7 sont **totalement indépendants**.

---

## Détail technique

### P4-5 : Extraction PDF

Options de crates pure Rust pour l'extraction PDF :
- [`pdf-extract`](https://crates.io/crates/pdf-extract) — le plus populaire, basé sur `lopdf`
- [`pdf`](https://crates.io/crates/pdf) — plus bas niveau
- Approche pragmatique : best-effort, certains PDFs (scans, images) ne donneront rien

```rust
pub fn pdf_to_markdown(path: &Path) -> Result<String, OxidriveError> {
    let text = pdf_extract::extract_text(path)
        .map_err(|e| OxidriveError::other(format!("PDF extraction failed: {e}")))?;

    if text.trim().is_empty() {
        // PDF probablement scanné / image
        return Ok(format!("*(PDF sans texte extractible : {})*\n", path.display()));
    }

    Ok(text)
}
```

### P4-7 : Passthrough et métadonnées

```rust
fn index_text_file(src: &Path, dest: &Path) -> Result<(), OxidriveError> {
    // Copie simple pour .txt et .md
    std::fs::copy(src, dest)?;
    Ok(())
}

fn index_binary_metadata(src: &Path, dest: &Path) -> Result<(), OxidriveError> {
    let meta = std::fs::metadata(src)?;
    let ext = src.extension().map(|e| e.to_string_lossy().to_string()).unwrap_or_default();
    let content = format!(
        "# {}\n\n- **Extension** : .{}\n- **Taille** : {} octets\n- **Modifié** : {:?}\n",
        src.file_name().unwrap_or_default().to_string_lossy(),
        ext,
        meta.len(),
        meta.modified()?,
    );
    // Note : atomic_write est async dans utils/fs.rs
    // Pour un usage sync, utiliser tokio::runtime::Handle ou spawn_blocking
    std::fs::write(dest, content.as_bytes())?;
    Ok(())
}
```

### P4-10 : Intégration engine

Après chaque cycle de sync dans `run_sync` :

```rust
// Collecter les chemins modifiés depuis le SyncReport
let changed: Vec<RelativePath> = report.uploaded.iter()
    .chain(report.downloaded.iter())
    .cloned()
    .collect();

// Supprimer les entrées d'index pour les fichiers supprimés
for path in report.deleted_local.iter().chain(report.deleted_remote.iter()) {
    let index_file = index_dir.join(path.as_ref()).with_extension("md");
    if index_file.exists() {
        std::fs::remove_file(&index_file).ok();
        tracing::debug!(?path, "Removed index entry for deleted file");
    }
}

if let Some(index_dir) = &config.index_dir {
    let count = index::update_index(&changed, &config.sync_dir, index_dir).await?;
    tracing::info!(count, "Index updated");
}
```

---

## Mise à jour incrémentale

L'index suit les mêmes événements que la sync :

| Événement sync | Action index |
|---------------|-------------|
| Fichier téléchargé (nouveau ou modifié) | Regénérer le `.md` |
| Fichier uploadé (modifié localement) | Regénérer le `.md` |
| Fichier supprimé (local ou remote) | Supprimer le `.md` correspondant |
| Fichier renommé/déplacé | Renommer/déplacer le `.md` |
| Fichier inchangé (`Skip`) | Rien |

---

## Critères de complétion

- [x] `update_index` est appelé après chaque cycle de sync (si `index_dir` est défini)
- [x] Les Google Docs peuvent être indexés via les exports existants (P4-1 helper optionnel)
- [x] Les .docx/.xlsx/.pptx/.csv sont correctement convertis en Markdown
- [x] Les .pdf sont indexés en best-effort (texte extrait quand possible)
- [x] Les .txt/.md sont lus / passthrough
- [x] Les binaires ont une fiche de métadonnées
- [x] La suppression d'un fichier source supprime le `.md` correspondant (chemins absents dans `update_index`)
- [x] `.index/` est exclu de la synchronisation Drive
- [x] Tests unitaires pour les extracteurs principaux (fichiers de référence selon couverture actuelle)

→ Suivant : [Phase 5 — Polish, service, cross-compilation](phase5-polish.md)
