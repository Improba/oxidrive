# Phase 3 — Conversion Google Workspace ↔ formats ouverts

## Objectif

Les fichiers natifs Google (Docs, Sheets, Slides, Drawings) sont **exportés** en formats ouverts localement (.docx, .xlsx, .pptx, .svg) et **réimportés** avec conversion si modifiés localement. Le cycle de conversion est transparent pour l'utilisateur.

---

## Statut actuel : 🟡 PRESQUE COMPLÈTE (intégration engine / E2E)

| Composant | État | Détail |
|-----------|------|--------|
| Détection MIME Google Workspace | ✅ | `is_google_workspace()`, `export_format()` / `export_format_sync()` dans `drive/types.rs` |
| Export via `files.export` | ✅ | `export_file`, `export_file_with_fallback` dans `drive/download.rs` |
| Export via `exportLinks` (>10MB) | ✅ | `export_file_with_fallback` |
| Import avec conversion | ✅ | `upload_with_conversion()` dans `drive/upload.rs` |
| Migration vers OOXML | ✅ | `export_format_sync` : MIME OOXML Docs/Sheets/Slides |
| Table de conversions (store) | ✅ | CRUD + utilisation dans `executor.rs` (`get_conversion` / `upsert_conversion` / `remove_conversion`) |
| Intégration dans decision.rs | ✅ | `determine_action_converted` + tests unitaires |
| Google Drawings → SVG | ✅ | `export_format_sync` → `image/svg+xml` |
| Intégration bout-en-bout engine | 🟡 | Executor : export OOXML + fallback ; `engine.rs` utilise encore `determine_action` (pas `determine_action_converted` partout) — finalisation en cours |

---

## Prérequis

- Phase 1 complétée (sync de base fonctionnelle)
- Compréhension des limites de l'API Drive v3 pour les exports/conversions

---

## Contexte technique

### Le problème des fichiers Google natifs

Les fichiers Google Workspace (Docs, Sheets, Slides) n'ont **pas de contenu binaire téléchargeable**. Ce sont des objets cloud. L'API Drive offre deux mécanismes :

1. **`files.export`** (≤10MB) : exporte vers un format standard
2. **`exportLinks`** (sans limite) : URLs directes d'export obtenues via `files.get?fields=exportLinks`

### Le problème du MD5

Les fichiers Google natifs **n'ont pas de MD5** dans l'API (`md5Checksum` est absent). La détection de changement doit se baser sur `modifiedTime` comparé à la dernière sync. C'est déjà géré dans `decision.rs` via `remote_content_fingerprint()` qui utilise `mtime:{iso}` comme fingerprint de fallback.

### La conversion n'est pas lossless

Le cycle `.gdoc` → `.docx` → `.gdoc` **perd** : suggestions, commentaires liés, smart chips, liens internes Drive, historique de versions. Pour limiter les dégâts :
- Ne reconvertir `.docx` → Google Doc que si le contenu local a **réellement changé** (MD5 du .docx différent de la dernière version exportée)
- Documenter clairement les pertes

---

## Tableau de correspondance des formats

### Code actuel (`drive/types.rs::export_format_sync()`)

| Format Google | Format local cible | MIME d'export | Note |
|---------------|-------------------|---------------|------|
| Google Docs | `.docx` | OOXML Word | Réimportable |
| Google Sheets | `.xlsx` | OOXML Sheet | Réimportable |
| Google Slides | `.pptx` | OOXML Presentation | Réimportable |
| Google Drawings | `.svg` | `image/svg+xml` | Lecture seule côté round-trip |

L’index Markdown (phase 4) peut toujours s’appuyer sur des exports texte via `export_format` / flux dédiés si besoin.

---

## Matrice de tâches

| ID | Tâche | Fichier(s) | Input | Output | Critère de complétion | Dépendances | Complexité | Statut |
|----|-------|-----------|-------|--------|----------------------|-------------|------------|--------|
| **P3-1** | Détection MIME GWS | `src/drive/types.rs` | — | `is_google_workspace(mime) → Option<ExportFormat>` | Test : chaque MIME → bon format | P1-1 | Faible | ✅ |
| **P3-2** | Export via `files.export` | `src/drive/download.rs` | P1-2 | `export_file(drive_id, mime_export)` → bytes | Test mock : export retourne contenu | P1-2 | Faible | ✅ |
| **P3-3** | Export via `exportLinks` (>10MB) | `src/drive/download.rs` | P1-2 | Fallback auto quand `files.export` échoue (limite / erreur) | `export_file_with_fallback` | P1-2 | Moyenne | ✅ |
| **P3-4** | Import avec conversion | `src/drive/upload.rs` | P1-6 | Upload .docx avec `mimeType: vnd.google-apps.document` | Test mock : upload converti, même Drive ID | P1-6 | Faible | ✅ |
| **P3-5** | Table de conversions (store) | `src/store/db.rs`, `src/store/session.rs`, `src/sync/executor.rs` | P0-8 | CRUD + usage dans les chemins upload/export Google | Round-trip store + executor | P0-8 | Faible | ✅ |
| **P3-6** | Intégration decision.rs | `src/sync/decision.rs` | P1-8, P3-5 | `determine_action_converted` : skip si MD5 export identique | Tests unitaires convertis | P1-8, P3-5 | Moyenne | ✅ |
| **P3-7** | Export Google Drawings → SVG | `src/drive/download.rs`, `src/drive/types.rs` | P1-2, P3-1 | MIME `image/svg+xml` | Export Drawing en SVG | P1-2, P3-1 | Faible | ✅ |
| **P3-8** | Intégration download/upload / engine | `src/sync/executor.rs`, `src/sync/engine.rs` | P3-2..P3-7 | Flux sync homogène (décision convertie + exports) | Test E2E : Google Doc → .docx → re-upload | P3-2..P3-7 | Moyenne | 🟡 |

---

## Graphe de dépendances

```
P3-1 (MIME) ──→ P3-2 (export) ──→ P3-3 (export >10MB) ──┐
                                                           │
P3-4 (import conversion) ────────────────────────────────┤
                                                           │
P3-5 (table conversions) ────→ P3-6 (decision.rs) ──────┤
                                                           │
P3-7 (Drawings SVG) ────────────────────────────────────┤
                                                           │
                                                           └──→ P3-8 (intégration E2E)
```

**Parallélisables** : P3-2, P3-4, P3-5, P3-7 sont indépendants une fois P3-1 terminé.

---

## Détail technique

### P3-3 : Fallback exportLinks

```rust
async fn export_file_large(
    client: &DriveClient,
    drive_id: &str,
    export_mime: &str,
    dest: &Path,
) -> Result<(), OxidriveError> {
    // 1. Tenter files.export (simple, limite 10MB)
    match export_file(client, drive_id, export_mime, dest).await {
        Ok(()) => return Ok(()),
        Err(e) if is_size_limit_error(&e) => {
            tracing::warn!("Export exceeded 10MB limit, falling back to exportLinks");
        }
        Err(e) => return Err(e),
    }

    // 2. Fallback : obtenir exportLinks via files.get
    let file_meta = client.request(
        Method::GET,
        &format!("https://www.googleapis.com/drive/v3/files/{}?fields=exportLinks", drive_id),
    ).await?;
    let links: HashMap<String, String> = file_meta.json().await?;
    let url = links.get(export_mime)
        .ok_or_else(|| OxidriveError::drive("No exportLink for requested MIME"))?;

    // 3. Télécharger via l'URL d'export directe
    download_url(client, url, dest).await
}
```

### P3-6 : Intégration decision.rs

Le decision tree doit distinguer 3 types de fichiers :

1. **Fichiers normaux** (ont un MD5) — logique existante
2. **Fichiers Google natifs** (pas de MD5, ont un `modifiedTime`) — fingerprint `mtime:` déjà géré
3. **Fichiers convertis** (un .docx local correspond à un Google Doc distant) — nécessite la table CONVERSIONS

Pour les fichiers convertis :
- `local_changed` = MD5 du .docx local ≠ MD5 du .docx au dernier export
- `remote_changed` = `modifiedTime` du Google Doc > `last_synced_at`
- Si les deux ont changé → conflit (même logique, avec la perte de données documentée)
- Si seul le local a changé → upload avec conversion
- Si seul le remote a changé → re-export

---

## Critères de complétion

- [x] Les Google Docs/Sheets/Slides sont exportés en OOXML via `export_format_sync` + téléchargement
- [x] Modifier un .docx local → re-upload avec conversion, même Drive ID conservé (executor + store conversions)
- [x] Les Google Drawings : export SVG via MIME `image/svg+xml`
- [x] Les documents volumineux : fallback `exportLinks` via `export_file_with_fallback`
- [x] La table CONVERSIONS maintenue dans les chemins executor pertinents
- [x] `determine_action_converted` couvre les cas convertis (tests unitaires)
- [ ] `determine_action_converted` systématiquement utilisé depuis `engine.rs` (🟡 en cours)
- [ ] Tests d'intégration avec mock pour chaque type de conversion
- [ ] Documentation des limites de la conversion (pertes de métadonnées collaboratives) — à approfondir

→ Suivant : [Phase 4 — Index Markdown](phase4-index.md)
