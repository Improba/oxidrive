# Git workflow

Conventions for branches, commit messages, pull requests, and continuous integration for the **oxidrive** project.

---

## Branches

| Type | Convention | Usage |
|------|------------|--------|
| Main | `main` | Always deployable or “mergeable”; protected with mandatory review if the repository allows. |
| Feature | `feature/<short-name>` | E.g. `feature/drive-changes-pagination`, `feature/markdown-index`. |
| Fix | `fix/<short-name>` | E.g. `fix/oauth-refresh-race`, `fix/redb-lock-timeout`. |

Avoid direct commits on `main` when repository policy requires PRs. Release branches (`release/x.y`) can be added later if needed.

---

## Commits

Adopt a style **close to Conventional Commits** (lowercase prefix + description):

| Prefix | Example | When to use |
|--------|---------|-------------|
| `feat:` | `feat: add the status command` | New user-visible feature or public API. |
| `fix:` | `fix: correct watcher debounce` | Bug fix. |
| `docs:` | `docs: complete the decision tree` | Documentation only. |
| `refactor:` | `refactor: extract the Drive client` | Restructuring without intended behavior change. |
| `test:` | `test: cover the CleanupMetadata case` | Adding or fixing tests. |
| `chore:` | `chore: update dependencies` | Maintenance (CI, deps, scripts). |

**Best practices**:

- One commit = one **readable** intent; avoid “WIP” on `main`.
- Optional message body but useful to explain the *why* (context, trade-off).
- Reference an issue or ticket (`Closes #123`) when relevant.

---

## Pull requests

1. **Title**: clear, in French or English per repository convention (stay consistent with history).
2. **Description**: goal, summary of changes, review points (perf risks, config compatibility).
3. **Size**: prefer **small to medium** PRs to ease review.
4. **Tests**: state what was run locally (`cargo test`, `cargo clippy`).
5. **Breaking changes**: call them out explicitly in the description and, if needed, in the changelog.

---

## CI/CD

Typical goal for the pipeline (GitHub Actions, GitLab CI, etc.):

| Step | Command / action |
|------|------------------|
| Format | `cargo fmt --check` |
| Lint | `cargo clippy --all-targets -- -D warnings` |
| Tests | `cargo test` |
| Release build (optional) | `cargo build --release` on supported targets |

README badges can point to this pipeline once configured. Binary releases can be produced by a dedicated job (OS/arch matrix) after a version tag.

For detailed code style, see [code-style.md](code-style.md).
