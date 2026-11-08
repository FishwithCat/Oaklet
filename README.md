# Oaklet

A local code agent with a Tauri 2 desktop shell: React/TypeScript UI, Rust agent
runtime. Inspired by the host-side loop, scoped tools, and explicit approvals in
[DeepSeek Harness](https://github.com/deepseek-ai/deepseek-harness), kept small
enough to read in one sitting.

## What it does

- Chats with any OpenAI-compatible `chat/completions` API (DeepSeek by default).
- Streams assistant text and model reasoning, renders Markdown replies, and
  folds thinking and tool calls into compact, expandable process rows.
- Tools: `read_file`, `write_file`, `edit_file`, `list_dir`, `run_command`.
- Asks before every shell command; the approval card shows the exact command.
- Confines file tools to the chosen workspace and refuses writes inside `.git`.
- Stores the API key in the app config directory (`0600` on Unix), never in the
  repository.

## Layout

- `desktop/` — React/TypeScript UI (Vite)
- `packages/core/` — Rust backend (Tauri app, agent loop, tools)

Tools live in `packages/core/src/tools/`. Each tool owns its schema,
system-prompt guidance, pending-call summary, approval policy, timeout, and
body; the registry projects the model-facing schemas and the execution pipeline
owns lookup, approval, deadlines, and result normalization. Adding a tool does
not touch the agent loop or the chat UI.

The Rust side owns the conversation for a turn. It streams model deltas to the
UI over Tauri events, executes tools locally, and returns the updated message
history when the turn ends. The UI never executes files or commands itself.

## Development

```sh
cd desktop
pnpm install
pnpm tauri dev
```

Frontend only: `pnpm dev`. The browser has no Tauri IPC, so this is only useful
for styling. Build frontend: `pnpm build`. Rust checks:

```sh
cd packages/core
cargo test
cargo clippy --all-targets
```

## Configuration

Open **Settings** in the app:

| Setting   | Default                     |
| --------- | --------------------------- |
| API key   | none                        |
| Base URL  | `https://api.deepseek.com`  |
| Model     | `deepseek-flash`            |
| Workspace | your home directory         |

For another OpenAI-compatible provider, set its base URL (for example
`https://api.openai.com/v1`) and model. For a fully local setup with
[Ollama](https://ollama.com), use `http://localhost:11434/v1` and a model such
as `qwen2.5-coder`.

## Safety model

- File tools resolve paths against the canonical workspace root, reject `..`
  escapes, symlinked escapes, and `.git` writes.
- `run_command` runs through the user's shell in the workspace and blocks on an
  in-app approval before starting. It times out after 120 seconds and caps
  captured output.
- The agent can still run any command the user approves, including commands that
  touch files outside the workspace. Read the approval card before allowing it.

## Status

Initial MVP. The conversation is in memory (restarting the app clears it), and
there is no diff view, syntax highlighting, or context compaction yet.
