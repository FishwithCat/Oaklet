# Oaklet

Tauri 2 + React + TypeScript desktop app, Rust backend.

## Layout

- `desktop/` — React/TypeScript UI (Vite)
- `packages/core/` — Rust backend (Tauri app, commands)

## Development

```sh
cd desktop
pnpm install
pnpm tauri dev
```

Frontend only: `pnpm dev`. Build frontend: `pnpm build`.
Rust checks: `cd packages/core && cargo check`.

## Commits

Messages follow [Conventional Commits](https://www.conventionalcommits.org/):
`<type>(<scope>): <summary>`, e.g. `feat(core): add greet command`.

A `commit-msg` hook runs commitlint. Install the tooling once from the repo root:

```sh
pnpm install
```

The root `prepare` script points Git at `.githooks/` and enables the
`.gitmessage` template. Check a message manually with
`pnpm exec commitlint --edit <file>`.

## Recommended IDE Setup

- [VS Code](https://code.visualstudio.com/) + [Tauri](https://marketplace.visualstudio.com/items?itemName=tauri-apps.tauri-vscode) + [rust-analyzer](https://marketplace.visualstudio.com/items?itemName=rust-lang.rust-analyzer)
