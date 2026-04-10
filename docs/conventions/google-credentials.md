# Configuration des identifiants Google (OAuth)

Guide pour créer un projet Google Cloud et brancher oxidrive avec `client_id` / `client_secret`.

## Étape 1 : Créer un projet Google Cloud

1. Ouvrir [Google Cloud Console](https://console.cloud.google.com/).
2. Menu projet → **Nouveau projet** (ou sélectionner un projet existant).
3. Donner un nom explicite (ex. « oxidrive sync ») et créer le projet.

## Étape 2 : Activer l’API Google Drive

1. Dans le projet, menu **APIs & Services** → **Library** (Bibliothèque).
2. Rechercher **Google Drive API**.
3. Cliquer sur **Enable** (Activer).

Sans cette activation, les appels oxidrive échoueront côté Google.

## Étape 3 : Écran de consentement OAuth

1. **APIs & Services** → **OAuth consent screen**.
2. Choisir **External** (ou Internal si compte Workspace restreint à l’organisation).
3. Renseigner les champs obligatoires (nom de l’app, email de support, etc.).
4. En mode test : ajouter votre compte (et ceux des testeurs) dans **Test users**.

Tant que l’app n’est pas en production vérifiée, seuls les utilisateurs de test peuvent se connecter.

## Étape 4 : Créer les identifiants OAuth 2.0 (Desktop)

1. **APIs & Services** → **Credentials** → **Create credentials** → **OAuth client ID**.
2. Type d’application : **Desktop app** (ou équivalent « Desktop »).
3. Nommer le client (ex. « oxidrive desktop ») et créer.

Google affiche le **Client ID** et le **Client secret**.

## Étape 5 : Renseigner `config.toml`

Copier `client_id` et `client_secret` dans la section OAuth de votre `config.toml`, selon les clés attendues par le projet (noms exacts dans l’exemple fourni avec oxidrive).

Ne pas commiter ce fichier s’il contient des secrets.

## Étape 6 : Finaliser avec `oxidrive setup`

```bash
oxidrive setup
```

Suivre le flux navigateur : consentement, puis enregistrement du jeton (souvent dans `token.json` à côté de la config). Une fois terminé, `sync` peut utiliser ces identifiants.

## Sécurité

- **Ne jamais committer** `client_secret`, `token.json` ni une config avec secrets dans le dépôt.
- Ajouter au **`.gitignore`** au minimum : `token.json`, fichiers `config.local.toml` ou secrets, selon votre arborescence.
- En cas de fuite : révoquer le client OAuth dans la console et en recréer un nouveau.

Pour un usage personnel, le type **Desktop** et des utilisateurs de test suffisent ; la publication en production nécessite souvent une validation Google plus lourde.
