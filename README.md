# Oaklet

A local code agent with a Tauri 2 desktop shell: React/TypeScript UI, Rust agent
runtime. Inspired by the host-side loop, scoped tools, and explicit approvals in
[DeepSeek Harness](https://github.com/deepseek-ai/deepseek-harness), kept small
enough to read in one sitting.

## What it does

- Chats with any OpenAI-compatible `chat/completions` API (DeepSeek by default).
- Multiple chat tabs, each titled by its first prompt and with its own
  conversation and draft. Runs keep streaming while you switch tabs; Command+N
  opens a new chat tab, and Command+R restores a saved session into its existing
  tab or a new one.
- Streams assistant text and model reasoning, renders Markdown replies, and
  folds thinking and tool calls into compact, expandable process rows.
- Tools: `read_file`, `write_file`, `edit_file`, `list_dir`, `run_command`,
  `likec4_mcp`, `skill` when the workspace defines skills, tools from
  configured MCP servers, and `web_search` when an API key is set.
- A terminal view in the sidebar backed by real PTYs: multiple tabs, interactive
  shells, colors, job control (`Ctrl+C`), and full scrollback, starting in the
  workspace. Select text in chat and click **Send to Terminal** to run it in the
  active tab.
- A source control view split into **Changes** (uncommitted files with their
  diffs) and **Commits** (the current branch, the last 100 commits, and the diff
  of each commit with per-file stats and line numbers). The branch pill switches
  local branches. Its sidebar badge counts files with changes not staged for
  commit. Diff previews are limited to 1 MiB and display one file at a time,
  with 200 lines per page. Larger patches can be inspected in the terminal.
- An architecture view: discovers existing LikeC4 models in the workspace and
  renders their views. Generation and updates are ordinary, editable chat
  requests handled by the configured agent. When a workspace already contains
  LikeC4 sources, the agent is also asked to keep the affected model in sync
  after code changes.
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

When the workspace contains `*.c4` or `*.likec4` sources, the agent's system
prompt names a few of them and asks the agent to update the affected model after
finishing code changes, preserving existing IDs, views, and manual descriptions.
This applies to ordinary chat requests too, not only to **Update with agent**.

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

### MCP servers

**Settings → MCP** connects Oaklet to other MCP servers. Each configured server
can be enabled or disabled independently: a disabled server keeps its settings
but exposes no tools to the agent. Changes apply to the next agent run; a turn
that is already running keeps the tools it started with.

The tab edits the standard `mcpServers` JSON document:

```json
{
  "mcpServers": {
    "zhihu": {
      "type": "streamableHttp",
      "url": "https://developer.zhihu.com/api/mcp/v1",
      "headers": {
        "Authorization": "Bearer your_access_secret"
      }
    },
    "files": {
      "type": "stdio",
      "command": "npx",
      "args": ["-y", "@modelcontextprotocol/server-filesystem", "/workspace"],
      "env": {}
    }
  }
}
```

`type` is `stdio` or `streamableHttp` (the aliases `http` and `streamable_http`
are also accepted). When `type` is omitted, a command selects stdio and a URL
selects Streamable HTTP. Add `"enabled": false` to pause a server; the toggle
list under the editor does the same without deleting its configuration.

Each discovered tool is exposed to the model as
`mcp__<server>__<tool>`. Names are sanitized to the OpenAI function-name
character set and shortened to 64 characters. Servers start in parallel when a
run begins; a failed or slow server becomes a warning in the system prompt and
does not stop the run. Tool calls have the same 60-second deadline as LikeC4
MCP calls.

MCP tools run without an approval prompt. Only enable servers and credentials you
trust: a configured command runs with your user privileges, and an HTTP server
can receive the headers entered in Settings. Tool results are treated as
external data, never as instructions. Configuration, including environment
values and headers, is stored in the app config file (`0600` on Unix).

## Skills

Skills are reusable Markdown instructions the agent loads only when a task
matches. Oaklet scans these folders directly (no recursion) at the start of
every agent run; a higher row wins a duplicate name:

| Priority | Folder                       |
| -------- | ---------------------------- |
| 1        | `<workspace>/.oaklet/skills` |
| 2        | `<workspace>/.agents/skills` |
| 3        | `~/.oaklet/skills`           |
| 4        | `~/.agents/skills`           |

A skill is either a directory containing `SKILL.md` or a flat
`<name>.md` file:

```markdown
---
name: review
description: Review a pull request for correctness, lifecycle, and test gaps.
whenToUse: When the user asks for a code review.
disable-model-invocation: false
user-invocable: true
---

# Reviewing a change

Read the diff first...
```

`name` must be lower-case kebab-case and `description` is required. Set
`disable-model-invocation: true` to keep the model from loading a skill;
`user-invocable` is reserved for future `/name` invocation. The body is
returned inside `<skill_content>` with the skill's base directory, so relative
references resolve next to the skill.

The system prompt carries a summary of model-invocable skills. The agent calls
`skill({"name":"review"})` to load the full body. Files are re-read on each
load, so body edits apply to the next call; new or removed skills apply to the
next message. Skill files are limited to 256 KiB and bodies to 24,000
characters.

The **Skills** icon in the sidebar opens a popup listing discovered skills,
their source and invocation flags, and any files that were skipped. **Refresh**
rescans immediately. The popup is read-only; edits and new files belong in the
folders above.

## Web search

With an API key in **Settings**, the agent also gets `web_search`. It sends one
query to the DeepSeek Search API (`https://api.deepseek.com/v1/tools/search`)
with that key and asks for 10 results, then returns each source's title, URL,
and summary (or snippet) with its date. The tool has a 20-second deadline and
asks for no approval: it sends only the query, and to the provider the
conversation already uses.

**Search URL** sets the endpoint. It defaults to the DeepSeek Search API and
ignores the configured Base URL and Model, so search works with any model
provider as long as the key is a DeepSeek key. The result count stays fixed at
10, and an empty Search URL falls back to the default. Without a key the tool is
not registered and the model never sees it, so clearing the key also disables
web search.

## Configuration

Open **Settings** in the app:

| Setting    | Default                                    |
| ---------- | ------------------------------------------ |
| API key    | none                                       |
| Base URL   | `https://api.deepseek.com`                 |
| Model      | `deepseek-flash`                           |
| Context window | `262144` tokens                        |
| Search URL | `https://api.deepseek.com/v1/tools/search` |
| MCP servers | none                                      |
| Workspace  | your home directory                        |

Oaklet keeps long tool output to its head and tail, and when a run approaches
the context window it asks the model to replace older history with a short
checkpoint. The system prompt and the current turn are never summarized.
The composer shows the estimated size of the next request against the window
as a small plant in its top-left corner that turns from green to yellow to
red; hover it for the percentage and token counts.

A run has no fixed step limit. Three cheaper controls bound the cost instead:
every potentially slow tool call has its own timeout (`run_command` 120 seconds,
MCP 60, `web_search` 20), repeating one identical call earns an advisory
reminder at the third, fifth, and eighth try, and **Stop** cancels the model
request, a running command, or a pending approval immediately.

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
- `web_search` sends the model's query text to the DeepSeek Search API. It runs
  without an approval prompt, reads no workspace files, and is registered only
  while an API key is configured.
- MCP servers are user-configured programs and endpoints. Once enabled, their
  tools run without an approval prompt and may read or change anything those
  servers can reach. Their output is model input, not trusted instructions.
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

Initial MVP. Open chat tabs are in memory (restarting the app clears them;
completed sessions remain in history), and there is no syntax highlighting.
