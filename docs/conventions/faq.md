# FAQ

Common answers about oxidrive and synchronization with Google Drive.

## Which Google Workspace formats are supported?

When exporting from Drive, native types are generally converted as follows:

- **Google Docs** → `.docx`
- **Google Sheets** → `.xlsx`
- **Google Slides** → `.pptx`
- **Google Drawings** → `.svg`

Exact details may depend on the sync version and options; refer to the sync documentation for edge cases.

## Are deleted files really deleted?

**No**—on Google Drive, deleted items go to the **trash** and remain **recoverable for about 30 days** (depending on account / admin policy). Permanent deletion only happens after emptying the trash or after expiration.

## Can I sync multiple Drive folders?

**Not yet** in the “multiple roots” mode people often expect. For now: **one instance / configuration per folder** (config file and optionally separate service).

## How do I ignore certain files?

Set **`ignore_patterns`** in `config.toml` (glob or patterns supported by the project) to exclude paths or file names from synchronization.

## What is the maximum file size?

**No limit imposed by oxidrive** itself; **Google Drive API** and account limits apply (chunked uploads, storage quotas, etc.).

## How can I see what will happen before syncing?

Run a **dry run**:

```bash
oxidrive sync --dry-run
```

No permanent changes are applied; the output shows what would be done.

## Does oxidrive work on Windows?

**Yes.** Building, syncing, and the **`service`** subcommand all work on Windows. The service backend uses **Windows Task Scheduler** (`schtasks.exe`) to register oxidrive as a logon task. The **`sync`**, **`setup`**, and **`service`** commands work as on other platforms.

## How do I update oxidrive?

- **From source**: rebuild / reinstall with Cargo, for example  
  `cargo install --path .` or `cargo install --git …` depending on your workflow.
- **Binary**: download the latest version from the project repository **Releases**.

Check the release notes for configuration or schema changes.
