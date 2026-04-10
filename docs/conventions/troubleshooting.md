# Dépannage (troubleshooting)

Guide des erreurs fréquentes et des pistes de résolution.

## Erreur : inotify max_user_watches

Sous Linux, le suivi des fichiers (watcher) repose sur inotify. Si la limite du noyau est trop basse, vous pouvez voir une erreur liée à `max_user_watches`.

**Solution immédiate** (jusqu’au prochain redémarrage) :

```bash
echo 524288 | sudo tee /proc/sys/fs/inotify/max_user_watches
```

**Solution permanente** : ajouter une entrée sysctl, par exemple dans `/etc/sysctl.d/99-inotify.conf` :

```bash
fs.inotify.max_user_watches=524288
```

Puis appliquer :

```bash
sudo sysctl --system
```

## Erreur OAuth2 : token expiré

Les jetons d’accès OAuth2 ont une durée de vie limitée. Si le rafraîchissement échoue ou si la session est invalide :

1. Relancer le flux d’authentification : `oxidrive setup`.
2. Vérifier les droits sur `token.json` (lecture/écriture pour l’utilisateur qui lance oxidrive).

## Sync bloqué / aucun fichier transféré

1. Vérifier que `drive_folder_id` dans la configuration pointe bien vers le dossier Drive souhaité.
2. Relancer avec des logs détaillés : `oxidrive sync -vv` (ou équivalent selon la CLI) pour voir où ça bloque.
3. Contrôler la connectivité réseau (pare-feu, proxy, DNS).

## Conflit non résolu

oxidrive applique une **politique de conflit** configurable (par exemple : privilégier une source, renommer, etc.). En cas de conflit, le comportement dépend de cette politique :

- Certaines stratégies **renomment** automatiquement un des fichiers (suffixe ou nom dérivé) pour éviter d’écraser l’autre copie.
- Consultez la documentation de configuration pour la politique active et les options `rename` / résolution côté local vs Drive.

Si un conflit reste « non résolu » dans les logs, comparez les deux versions (horodatage, contenu) et ajustez la politique ou résolvez manuellement sur le disque ou dans Drive.

## Service systemd ne démarre pas

Pour une unité **utilisateur** :

```bash
systemctl --user status oxidrive
journalctl --user -u oxidrive
```

Les journaux indiquent souvent une erreur de chemin, de permissions ou d’environnement (variables manquantes pour OAuth).

## Erreur de permission sur sync_dir

Le répertoire de synchronisation (`sync_dir`) doit être **accessible en lecture et écriture** par l’utilisateur qui exécute oxidrive (ou le service).

Vérifier propriétaire et permissions, par exemple :

```bash
ls -la /chemin/vers/sync_dir
```

Corriger avec `chown` / `chmod` si nécessaire (sans exposer le dossier à d’autres utilisateurs de façon excessive).

## Rate limit Google API (403 / 429)

Google Drive impose des quotas. oxidrive intègre en général des **nouvelles tentatives avec backoff exponentiel** face aux erreurs temporaires.

Pour réduire la pression sur l’API :

- Diminuer `max_concurrent_uploads` et `max_concurrent_downloads` dans la configuration.
- Éviter les grosses salves de petits fichiers si possible, ou étaler la charge.

Si le problème persiste, consulter la console Google Cloud (quotas, erreurs détaillées) et les limites du type de compte / projet.
