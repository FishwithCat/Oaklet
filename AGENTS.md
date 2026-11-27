# Commit Rules

- **Atomic Commits**: Each commit must represent a single, logically independent change. Do not combine unrelated changes in one commit.
- **Repository Conventions First**: Follow the repository's existing commit conventions and commitlint configuration. Use Conventional Commits as the fallback when no project-specific rules exist.
- **Explicit Approval Before Committing**: Generate and validate commit messages, but never execute `git commit` without explicit user authorization.

# Database Migrations

- Schema changes are SQL files in `packages/core/migrations/`, named `V<number>__<description>.sql`. `V0__baseline.sql` is the baseline; do not put schema SQL in Rust strings.
- Never edit, rename, or delete a migration that may have been applied. Add the next numbered migration instead; checksums are verified at startup.
- `packages/core/build.rs` embeds the files at compile time, and `packages/core/src/store/migrations.rs` applies pending migrations at startup, one transaction per migration.
- The runner never drops tables. A pre-V0 database is refused with a clear error; reset it manually from the shell (delete `oaklet.db`) before first running this build.
- Run `cargo test` and `cargo clippy --all-targets` in `packages/core` after changing a migration or the runner. The in-house runner is intentional; do not replace it with a migration framework as part of an unrelated change.

# UI Style

- **Minimize borders and background fills**: separate content with spacing, typography, alignment, and subtle dividers before adding a border or a colored surface. Do not wrap ordinary settings rows, messages, or status text in bordered or tinted cards.
- Reserve borders and backgrounds for controls and states that need them: inputs, focus rings, selection, dialogs/popovers and their backdrop, warnings/errors, and code or diff blocks.
- Use the existing theme tokens (`--border`, `--panel`, `--panel-muted`, `--accent-soft`, `--hover-overlay`, `--subtle-overlay`) instead of hard-coded colors, and do not introduce one-off card treatments.
- When unsure, remove the container: a heading plus well-spaced content is usually enough.
