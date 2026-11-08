# Commit Rules

- **Atomic Commits**: Each commit must represent a single, logically independent change. Do not combine unrelated changes in one commit.
- **Repository Conventions First**: Follow the repository's existing commit conventions and commitlint configuration. Use Conventional Commits as the fallback when no project-specific rules exist.
- **Explicit Approval Before Committing**: Generate and validate commit messages, but never execute `git commit` without explicit user authorization.