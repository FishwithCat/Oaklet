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
drag Oaklet into Applications. If no build is available there, build from source
instead (see [CONTRIBUTING.md](CONTRIBUTING.md)). The build is universal
(Apple Silicon and Intel); without a Developer ID certificate it is ad-hoc
signed rather than notarized, so macOS warns on the first launch. Open it once,
then allow it:

- **macOS 15 or newer**: **System Settings → Privacy & Security → Open Anyway**.
- **macOS 14 or earlier**: right-click Oaklet and choose **Open**.

Or clear the quarantine flag from Terminal:

```sh
xattr -dr com.apple.quarantine /Applications/Oaklet.app
```

The Architecture view and the `likec4_mcp` tool need Node.js 22.22.3 or newer.
Oaklet finds it on `PATH`, in common install locations, or via `OAKLET_NODE`;
the rest of Oaklet works without it.

## Quick start

1. Click the folder icon in the sidebar and choose a workspace. File tools stay
   inside it.
2. Open **Settings**, use the **Model** tab, enter an API key, and choose a Base
   URL and model. DeepSeek is the default.
3. Describe a change and send it. The agent reads and edits files, runs commands
   (without an approval card by default), and streams the result into the chat.

To build Oaklet from source, see [CONTRIBUTING.md](CONTRIBUTING.md).

## Chat

- Multiple tabs, each titled by its first prompt and with its own conversation
  and draft. **⌘N** opens a tab and **⌘R** opens History while idle, where you
  can restore a saved session. Sessions live in the backend, so they survive a
  restart; a live run keeps streaming while you switch tabs, but quitting
  Oaklet stops it and the next start marks it interrupted.
- Paste screenshots or text files into the composer. Images are downscaled and
  sent as image parts, so the configured model must accept image input; text
  files up to 200 KB are accepted, and files above 16 KB are saved under
  `.oaklet/spill` with only a head/tail preview inlined.
- Replies stream as Markdown. Thinking and tool calls fold into compact,
  expandable process rows. `write_file`, `edit_file`, and `apply_patch` show an
  inline diff card with the applied changes and their added/removed line counts.
- **⌘F** finds text in the open conversation and highlights every match;
  **Enter** and **Shift+Enter** step through them.
- Select text in chat and click **Send to Terminal** to run it in the active
  terminal tab.
- **Stop** cancels the model request, a running command, or a pending approval.
- Long transcripts open on the latest page; scroll up and choose **Load earlier**
  to bring in older turns without reading the whole log.
- The plant in the composer's top-left corner estimates the next request against
  the context window and turns from green to yellow to red. Hover it for the
  percentage and token counts; click it while idle to fold older turns into a
  checkpoint immediately. Replaced messages stay in the transcript, so a
  compacted chat can page through the originals with `history_read` instead of
  losing them.

The agent has `read_file`, `read_image`, `write_file`, `edit_file`,
`apply_patch`, `list_dir`, `glob`, `grep`, `run_command`, `job_list`,
`job_output`, `job_kill`, `web_fetch`, and `likec4_mcp`; depending on your setup
it also has `skill`, MCP server tools, `web_search`, and, after compaction,
`history_read`.

## Workspace views

**Terminal** is backed by real PTYs: multiple tabs, interactive shells, colors,
job control (`Ctrl+C`), and 5,000 lines of scrollback, starting in the
workspace.

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
provider. The view re-reads when activated, on **Refresh**, and after an agent
run finishes or fails; it keeps the last valid diagram, marked stale, while new
sources are invalid. It does not watch the filesystem.

## Settings

Open **Settings** in the sidebar. Changes apply to the next run.

| Setting                       | Default                                    |
| ----------------------------- | ------------------------------------------ |
| API key                       | none                                       |
| Base URL                      | `https://api.deepseek.com`                 |
| Model                         | `deepseek-flash`                           |
| Summary model                 | same as Model                              |
| Context window                | `262144` tokens                            |
| Run commands without approval | on                                         |
| Search provider               | DeepSeek                                   |
| Search API key                | none (DeepSeek falls back to API key)      |
| Search URL                    | provider default                           |
| MCP servers                   | none                                       |
| Workspace                     | your home directory                        |

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

With a usable key — the search key, or the model API key when the search key is
empty — the agent also gets `web_search`. It sends one query to the configured
endpoint and requests up to 10 results with title, URL, excerpt, and date,
within 20 seconds. It asks for no approval: it sends only the query, never
workspace content.

**Settings → Web Search** sets the key and endpoint. The endpoint defaults to
the DeepSeek Search API and must speak that API. An empty search key sends the
model key to whatever host the endpoint names, so point it only at a host you
trust. Without a key the tool is not registered, so the model never sees it.

`web_fetch` is always available and does not need a search key. It returns up
to 200,000 characters and reduces HTML to readable text.

## Approvals and safety

- File tools resolve paths against the canonical workspace root and reject `..`
  escapes, symlinked escapes, and writes inside `.git`.
- `run_command` runs through your shell in the workspace and times out after
  120 seconds with capped output. Set `run_in_background` to start a long build
  or server; it returns a job id, keeps running after the chat turn, and is read
  and stopped with `job_output`/`job_kill` (or `job_list`). Background jobs have
  no timeout and are stopped when Oaklet exits. **Settings → Commands → Run
  commands without approval** is on by default, so commands run without an
  approval card; turn it off to see each command first.
- `web_fetch` reads any HTTP(S) URL and returns bounded text. It can reach
  localhost and private addresses, so treat fetched content as untrusted data.
- **Allow all in workspace** on an approval card stops prompting in that
  workspace for the rest of the app session. The sidebar leaf revokes that
  session grant. Grants never apply to file tools or another workspace, and
  commands can still touch files outside the workspace, so allow them only in a
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

## License

Apache License 2.0. See [LICENSE](LICENSE).

## Contributing

Development setup, tests, packaging, and architecture notes are in
[CONTRIBUTING.md](CONTRIBUTING.md).
