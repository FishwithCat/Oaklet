<p align="center">
  <img src="desktop/src/assets/oaklet-icon.png" width="170" alt="Oaklet logo">
</p>

# Oaklet

A local code agent with a Tauri 2 desktop shell: React/TypeScript UI, Rust agent
runtime. Inspired by the host-side loop, scoped tools, and explicit approvals in
[DeepSeek Harness](https://github.com/deepseek-ai/deepseek-harness).

## Install

Download the latest `.dmg` from
[Releases](https://github.com/FishwithCat/Oaklet/releases/latest), open it, and
drag Oaklet into Applications. The build is universal (Apple Silicon and Intel)
and ad-hoc signed rather than notarized, so macOS warns on the first launch.
Open it once, then allow it:

- **macOS 15 or newer**: **System Settings → Privacy & Security → Open Anyway**.
- **macOS 14 or earlier**: right-click Oaklet and choose **Open**.

Or clear the quarantine flag from Terminal:

```sh
xattr -dr com.apple.quarantine /Applications/Oaklet.app
```

The Architecture view needs Node.js 22.22.3 or newer on `PATH`; the rest of
Oaklet does not.

## Quick start

1. Click the folder icon in the sidebar and choose a workspace. File tools stay
   inside it.
2. Open **Settings**, use the **Model** tab, enter an API key, and choose a Base
   URL and model. DeepSeek is the default.
3. Describe a change and send it. The agent reads and edits files, runs commands
   after your approval, and streams the result into the chat.

To build Oaklet from source, see [CONTRIBUTING.md](CONTRIBUTING.md).

## Chat

- Multiple tabs, each titled by its first prompt and with its own conversation
  and draft. **⌘N** opens a tab and **⌘R** restores a saved session. Sessions
  and runs live in the backend, so they survive a restart and keep streaming
  while you switch tabs.
- Paste screenshots or text files into the composer. Images are downscaled and
  sent as image parts, so the configured model must accept image input; text
  files are inlined up to 200 KB.
- Replies stream as Markdown. Thinking and tool calls fold into compact,
  expandable process rows.
- Select text in chat and click **Send to Terminal** to run it in the active
  terminal tab.
- **Stop** cancels the model request, a running command, or a pending approval.
- The plant in the composer's top-left corner estimates the next request against
  the context window and turns from green to yellow to red. Hover it for the
  percentage and token counts; click it while idle to fold older turns into a
  checkpoint immediately.

The agent has `read_file`, `write_file`, `edit_file`, `list_dir`, `run_command`,
and, depending on your setup, `likec4_mcp`, `skill`, MCP server tools, and
`web_search`.

## Workspace views

**Terminal** is backed by real PTYs: multiple tabs, interactive shells, colors,
job control (`Ctrl+C`), and full scrollback, starting in the workspace.

**Files** browses the workspace as a tree and opens files in an editor. Markdown
can be previewed, and **⌘S** saves.

**Source control** has **Changes** (uncommitted files with diffs) and
**Commits** (current branch, last 100 commits, and the diff of each with
per-file stats). The branch pill switches branches, and the sidebar badge counts
files with unstaged changes. Diff previews are limited to 1 MiB, show one file
at a time, and paginate at 200 lines.

**Architecture** discovers LikeC4 models in the workspace and renders their
views. **Generate architecture** and **Update with agent** put an editable prompt
in the composer without discarding your draft; the normal chat flow still sends,
approves, and can cancel it. Viewing an existing model never calls the model
provider. The view re-reads when activated or refreshed and keeps the last valid
diagram, marked stale, while new sources are invalid. It does not watch the
filesystem.

## Settings

Open **Settings** in the sidebar. Changes apply to the next run.

| Setting        | Default                                    |
| -------------- | ------------------------------------------ |
| API key        | none                                       |
| Base URL       | `https://api.deepseek.com`                 |
| Model          | `deepseek-flash`                           |
| Context window | `262144` tokens                            |
| Search URL     | `https://api.deepseek.com/v1/tools/search` |
| MCP servers    | none                                       |
| Workspace      | your home directory                        |

For another OpenAI-compatible provider, set its base URL (for example
`https://api.openai.com/v1`) and model. For a local setup with
[Ollama](https://ollama.com), use `http://localhost:11434/v1` and a model such
as `qwen2.5-coder`.

## Skills

Skills are reusable Markdown instructions the agent loads only when a task
matches. Oaklet scans these folders (no recursion) at the start of every run; a
higher row wins a duplicate name:

| Priority | Folder                       |
| -------- | ---------------------------- |
| 1        | `<workspace>/.oaklet/skills` |
| 2        | `<workspace>/.agents/skills` |
| 3        | `~/.oaklet/skills`           |
| 4        | `~/.agents/skills`           |

A skill is either a directory containing `SKILL.md` or a flat `<name>.md`:

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
`user-invocable` is reserved for future `/name` invocation. The model sees a
summary and calls `skill({"name":"review"})` to load the full body. Bodies are
re-read on each load, so edits apply to the next call, and relative references
resolve from the skill's own directory. Skill files are limited to 256 KiB and
bodies to 24,000 characters.

The **Skills** icon in the sidebar lists discovered skills, their source and
invocation flags, and any skipped files. **Refresh** rescans immediately; the
popup is read-only.

## MCP servers

**Settings → MCP** connects Oaklet to other MCP servers. The tab edits the
standard `mcpServers` document:

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

`type` is `stdio` or `streamableHttp` (`http` and `streamable_http` are also
accepted). When omitted, a command selects stdio and a URL selects Streamable
HTTP. Add `"enabled": false` to pause a server, or use the toggle list below the
editor. Changes apply to the next run.

MCP tools run without an approval prompt. Only enable servers and credentials
you trust: a configured command runs with your user privileges, and an HTTP
server can receive the headers entered here. Tool output is treated as external
data, never as instructions.

## Web search

With an API key in Settings, the agent also gets `web_search`. It sends one
query to the DeepSeek Search API and returns up to 10 results with title, URL,
summary or snippet, and date, within 20 seconds. It asks for no approval: it
sends only the query, to the provider the conversation already uses.

**Search URL** sets the endpoint and ignores the configured Base URL and Model,
so search works with any model provider as long as the key is a DeepSeek key. An
empty Search URL falls back to the default. Without a key the tool is not
registered, so the model never sees it.

## Approvals and safety

- File tools resolve paths against the canonical workspace root and reject `..`
  escapes, symlinked escapes, and writes inside `.git`.
- `run_command` runs through your shell in the workspace and waits for an
  approval card showing the exact command. It times out after 120 seconds and
  caps captured output.
- **Allow all in workspace** stops prompting for `run_command` in that workspace
  for the rest of the app session. The sidebar indicator revokes it; the grant
  is in memory only and never applies to file tools or another workspace.
  Commands can still touch files outside the workspace, so grant it only in a
  project you trust.
- Commands typed in the terminal run immediately in the workspace, without
  model involvement, approval, or a timeout.
- MCP tools run without approval and can reach anything their server can. Their
  output is model input, not trusted instructions.
- Architecture preview parses model sources in the local, pinned helper. It does
  not execute workspace configuration or JavaScript.
- The agent stores the API key in the app config directory (`0600` on Unix),
  never in the repository.

## Status

Initial MVP. Open chat tabs and drafts are in memory (a restart clears them;
every session remains in History), there is no multi-device server, and chat
code blocks have no syntax highlighting.

## Contributing

Development setup, tests, packaging, and architecture notes are in
[CONTRIBUTING.md](CONTRIBUTING.md).
