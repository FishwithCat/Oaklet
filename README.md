# Oaklet

A local code agent with a Tauri 2 desktop shell: React/TypeScript UI, Rust agent
runtime. Inspired by the host-side loop, scoped tools, and explicit approvals in
[DeepSeek Harness](https://github.com/deepseek-ai/deepseek-harness), kept small
enough to read in one sitting.

## What it does

- Chats with any OpenAI-compatible `chat/completions` API (DeepSeek by default).
- Streams assistant text and model reasoning, renders Markdown replies, and
  folds thinking and tool calls into compact, expandable process rows.
- Tools: `read_file`, `write_file`, `edit_file`, `list_dir`, `run_command`, `likec4_mcp`.
- A terminal view in the sidebar backed by real PTYs: multiple tabs, interactive
  shells, colors, job control (`Ctrl+C`), and full scrollback, starting in the
  workspace. Select text in chat and click **Send to Terminal** to run it in the
  active tab.
- A source control view split into **Changes** (uncommitted files with their
  diffs) and **Commits** (the current branch, the last 100 commits, and the diff
  of each commit with per-file stats and line numbers). The branch pill switches
  local branches. Its sidebar badge counts files with changes not staged for
  commit.
- An architecture view: discovers existing LikeC4 models in the workspace and
  renders their views. Generation and updates are ordinary, editable chat
  requests handled by the configured agent.
- Asks before every shell command; the approval card shows the exact command
  and can allow all commands in the workspace.
- Confines file tools to the chosen workspace and refuses writes inside `.git`.
- Stores the API key in the app config directory (`0600` on Unix), never in the
  repository.

## Layout

- `desktop/` — React/TypeScript UI (Vite)
- `packages/core/` — Rust backend (Tauri app, agent loop, tools)
- `architecture-helper/` — pinned LikeC4 preview compiler and agent MCP bridge

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

Install the architecture compiler once before opening the Architecture view or
building a bundle:

```sh
cd architecture-helper
pnpm install
```

It requires a local Node.js 22.22.3 or newer runtime. Set `OAKLET_NODE` to a
different executable, or `OAKLET_ARCHITECTURE_HELPER` to a different helper
directory, if the defaults do not fit. When the runtime or helper is missing,
the Architecture view explains the problem and leaves the rest of Oaklet
working.

Frontend only: `pnpm dev`. The browser has no Tauri IPC, so this is only useful
for styling. Build frontend: `pnpm build`. Rust checks:

```sh
cd packages/core
cargo test
cargo clippy --all-targets
```

Helper checks:

```sh
cd architecture-helper
pnpm test
```

Before packaging, make sure `architecture-helper/node_modules` exists; Tauri
embeds that directory as an application resource. The packaged helper is
resolved from the app's resource directory. Development builds fall back to the
helper in the Oaklet checkout.

## Architecture models

The Architecture view scans `config.workspace` for `*.c4` and `*.likec4` files,
honoring git and `.ignore` rules without requiring a Git repository. A JSON
`likec4.config.json` groups its directory tree into one model root, with nested
configurations defining separate projects. Files without an ancestor configuration
share the workspace default model, even when spread across directories, matching
MCP project grouping. If discovery finds independent roots, the view asks which one to
load. **Scan ignored files** is available when a model lives in an ignored
directory.

Opening the view only reads models. If no model exists, **Generate architecture**
offers the default location `architecture/model.c4`; otherwise **Update with
agent** targets the selected model files. Both actions put an editable prompt in
the existing chat composer without discarding its draft. The user still sends,
approves commands, and can cancel through the normal chat flow.

Viewing an existing model never calls the model provider, so it works without an
API key. Generation and updates use the configured provider and existing
approval flow like any other coding request.

The view re-reads on activation, on **Refresh**, and after an agent run finishes
or fails. It does not watch the filesystem, so external edits appear on the next
activation or refresh. Compilation is skipped when the source contents have not
changed. If new sources are invalid, the last valid diagram for the same
workspace and model stays visible, marked as stale, with file and line
diagnostics.

Preview compilation is data-only: Oaklet sends the selected source text to the
pinned helper, which parses it in a virtual workspace and runs Graphviz as
WebAssembly. It does not execute `likec4.config.js`/`.ts`, Vite plugins,
workspace package scripts, or a workspace-supplied executable. JSON configuration
is read as data. Models that need executable configuration are reported but may
need adaptation for this preview.

Oaklet always validates the selected sources again in the pinned loader before
showing a new result. If a workspace already has the LikeC4 CLI installed, an
agent can also validate with `pnpm exec likec4 validate` through the normal
command-approval flow; Oaklet does not install it or run a workspace-provided
compiler automatically.

Limits are deliberate: 500 model files during discovery, 200 source files and
4 MB of source per model load, 1 MB per source file, 60 seconds of compiler time,
and 16 MB of layout output. A load that hits a limit fails with an explanation
instead of showing a partial diagram.

### LikeC4 MCP for the agent

The agent's `likec4_mcp` tool connects to the pinned official `@likec4/mcp`
server (1.60.0). Call it with `{}` to discover tool names and input schemas,
then with `{"tool":"list-projects","arguments":{}}` to find projects. Other
tools query elements, views, and relationships using those schemas.

It uses the same Node runtime and installed `architecture-helper` dependencies
as the preview; no global CLI, server port, or runtime package download is needed.
Each call loads the current workspace afresh, so edits are visible on the next
call. Calls use MCP initialization, tool discovery, and tool invocation over
local stdio streams inside one helper process. Cancellation or the 60-second
deadline stops that process. Missing dependencies and MCP errors become tool
errors; ordinary chat and file tools remain usable.

Unlike data-only preview compilation, the official MCP loader may execute
workspace LikeC4 configuration and read files it references. LikeC4 MCP calls are
automatically allowed without an approval prompt, independent of **Allow all in
workspace** for shell commands. Use it with trusted projects. Results are returned as JSON/text;
MCP Apps interactive widgets are not rendered in chat. Continue to use file tools
for model edits and the Architecture view for diagrams.

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
- **Allow all in workspace** stops prompting for `run_command` in that workspace
  for the rest of the app session. The sidebar shows an active indicator you can
  click to revoke; the grant is in memory only and never applies to file tools
  or another workspace. Commands can still touch files outside the workspace,
  so only grant this in a project you trust.
- Commands typed in the terminal view are not sent to the model and do not ask
  for approval: the shell runs immediately in the workspace, with no timeout.
- The agent can still run any command the user approves, including commands that
  touch files outside the workspace. Read the approval card before allowing it.
- Architecture preview sends only discovered model source text to the local,
  pinned helper. It never executes workspace configuration or JavaScript, and
  the compiler output is capped before it reaches the renderer.

## Status

Initial MVP. The conversation is in memory (restarting the app clears it), and
there is no syntax highlighting or context compaction yet.
