# FAQ

Réponses courantes sur oxidrive et la synchronisation avec Google Drive.

## Quels formats Google Workspace sont supportés ?

Lors de l’export depuis Drive, les types natifs sont en général convertis ainsi :

- **Google Docs** → `.docx`
- **Google Sheets** → `.xlsx`
- **Google Slides** → `.pptx`
- **Google Drawings** → `.svg`

Les détails exacts peuvent dépendre de la version et des options de sync ; se référer à la doc de sync pour les cas particuliers.

## Les fichiers supprimés sont-ils vraiment supprimés ?

**Non**, côté Google Drive les éléments supprimés vont en **corbeille** et restent **récupérables environ 30 jours** (selon la politique du compte / administrateur). La suppression définitive n’intervient qu’après vidage de la corbeille ou expiration.

## Puis-je synchroniser plusieurs dossiers Drive ?

**Pas encore** de mode « plusieurs racines » dans une seule instance telle qu’on l’attend souvent. Pour l’instant : **une instance / configuration par dossier** (fichier de config et éventuellement service séparés).

## Comment ignorer certains fichiers ?

Configurer **`ignore_patterns`** dans `config.toml` (glob ou motifs supportés par le projet) pour exclure chemins ou noms de fichiers de la synchronisation.

## Quelle est la taille max d’un fichier ?

**Pas de limite imposée par oxidrive** en soi ; s’appliquent les **limites de l’API Google Drive** et du compte (uploads chunkés, quotas de stockage, etc.).

## Comment voir ce qui va se passer avant de synchroniser ?

Lancer un **essai à blanc** :

```bash
oxidrive sync --dry-run
```

Aucune modification définitive n’est appliquée ; la sortie indique ce qui serait fait.

## oxidrive fonctionne-t-il sous Windows ?

La **compilation** fonctionne sur Windows, mais la sous-commande **`service`** (intégration systemd) **n’est pas supportée** sur cette plateforme. Les commandes **`sync`** et **`setup`** sont utilisables normalement.

## Comment mettre à jour oxidrive ?

- **Depuis les sources** : reconstruire / réinstaller avec Cargo, par exemple  
  `cargo install --path .` ou `cargo install --git …` selon votre flux.
- **Binaire** : télécharger la dernière version depuis les **Releases** du dépôt du projet.

Vérifier les notes de version pour les changements de configuration ou de schéma.
