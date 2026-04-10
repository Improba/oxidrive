# Troubleshooting

Common errors and how to fix them.

## Error: inotify max_user_watches

On Linux, file watching relies on inotify. If the kernel limit is too low, you may see an error related to `max_user_watches`.

**Immediate fix** (until the next reboot):

```bash
echo 524288 | sudo tee /proc/sys/fs/inotify/max_user_watches
```

**Permanent fix**: add a sysctl entry, for example in `/etc/sysctl.d/99-inotify.conf`:

```bash
fs.inotify.max_user_watches=524288
```

Then apply:

```bash
sudo sysctl --system
```

## OAuth2 error: expired token

OAuth2 access tokens expire. If refresh fails or the session is invalid:

1. Run the authentication flow again: `oxidrive setup`.
2. Check permissions on `token.json` (read/write for the user running oxidrive).

## Sync stuck / no files transferred

1. Confirm that `drive_folder_id` in the config points to the intended Drive folder.
2. Run with verbose logging: `oxidrive sync -vv` (or the CLI equivalent) to see where it stalls.
3. Check network connectivity (firewall, proxy, DNS).

## Unresolved conflict

oxidrive uses a configurable **conflict policy** (for example: prefer one source, rename, etc.). When a conflict occurs, behavior depends on that policy:

- Some strategies **rename** one of the files automatically (suffix or derived name) to avoid overwriting the other copy.
- See the configuration docs for the active policy and `rename` / local vs Drive resolution options.

If a conflict still shows as “unresolved” in the logs, compare the two versions (timestamps, content) and adjust the policy or resolve manually on disk or in Drive.

## systemd service fails to start

For a **user** unit:

```bash
systemctl --user status oxidrive
journalctl --user -u oxidrive
```

Logs often show a path error, permissions issue, or missing environment (variables needed for OAuth).

## Permission error on sync_dir

The sync directory (`sync_dir`) must be **readable and writable** by the user running oxidrive (or the service).

Check owner and permissions, for example:

```bash
ls -la /path/to/sync_dir
```

Fix with `chown` / `chmod` if needed (without exposing the folder to other users more than necessary).

## Google API rate limit (403 / 429)

Google Drive enforces quotas. oxidrive usually applies **retries with exponential backoff** for transient errors.

To reduce pressure on the API:

- Lower `max_concurrent_uploads` and `max_concurrent_downloads` in the configuration.
- Avoid large bursts of tiny files when possible, or spread the load.

If the issue persists, check the Google Cloud console (quotas, detailed errors) and limits for your account / project type.
