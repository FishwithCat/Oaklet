# Oaklet v1 Implementation Plan for Bug Fixes and Change Verification

Build bug reproduction, regression checks, and change verification into Oaklet. These workflows and a shared index of 24 engineering principles ship with the app. Users work in natural language from the existing chat composer without configuring Skills.

## User workflows

| User intent | Oaklet behavior |
| --- | --- |
| Fix a bug | Try to reproduce the issue before editing, then trace the failure to its root cause. Prefer existing tests or scripts. When a suitable regression check is available, confirm it fails for the expected reason, fix the bug, and rerun the same check. If reproduction is blocked, explain what's missing and run the relevant checks that are available. |
| Verify current changes | Inspect the current workspace changes and project conventions. Run the most relevant existing tests, builds, or scripts. Preserve the user's source files and Git staging state. Report any issues and describe what the checks covered. |
| Complete a feature change or refactor | Run checks relevant to the change before replying. Use the user's intent and project conventions to determine the scope. |

## Always-available principle index

Every main agent request includes a compact index of all 24 principles, including ordinary chat and `@arch` planning. Each entry gives the principle's name and one sentence describing when it applies. Based on the task and what it discovers, the agent calls `skill` for the matching principles before the relevant action. The detailed instructions enter the conversation only when requested. If those instructions are already in the active context, the agent reuses them.

The task determines which principles apply. A regular bug fix that uncovers duplicate writes on retry can load Make Operations Idempotent. A request to measure startup time can load Explain the Number before collecting a baseline. An `@arch` request can load Model the Domain when it involves scattered state. Loading a principle has read-only access, so each turn's existing permissions still govern any actions it suggests.

### Five principles with dedicated workflow rules in v1

| Principle | Oaklet behavior |
| --- | --- |
| Fix Root Causes | Try to reproduce the issue and trace the failure to its root cause before fixing it. |
| Laziness Protocol | Reuse existing code paths, tests, and scripts. Choose the smallest change that fully fixes the issue. |
| Test Behavior, Not Implementation | Check behavior that callers or users can observe. Capture the results before and after the fix. |
| Prove It Works | Use actual command results or observable runtime behavior as evidence. Explain what the evidence covers. |
| Sequence Work into Verifiable Units | Break multistep fixes into steps that can each be checked independently. |

The other 19 are also available on demand through the index. Dedicated workflows for architecture, performance, migrations, concurrency, or delegation can build on the same index later. [pstack Principles](https://github.com/cursor/plugins/blob/main/pstack/skills/poteto-mode/SKILL.md#principles)

## Implementation steps

### 1. Ship the principle index and load bodies through the existing Skill tool

- Package the 24 principle bodies as built-in Markdown in the core crate using `include_str!`. Maintain one catalog of names, short triggers, and body references as the source for both prompt guidance and exact-name lookup. Adapt the guidance to Oaklet's tools and permissions and retain pstack's MIT notice with adapted content. [pstack License](https://github.com/cursor/plugins/blob/main/pstack/LICENSE)
- Reuse the read-only `skill` tool. Register it even in workspaces with no installed skills. Give built-in principles distinct, reserved names such as `oaklet-principle-make-operations-idempotent` and keep existing discovery and precedence for other skills.
- Use the existing tool-guidance path to put the index into the main agent's system prompt. Return the full Markdown only for the exact principle requested through `skill`. Keep the index available when the system prompt is rebuilt, including after context compaction. Skill selection is driven by the model using the task, the trigger descriptions, and evidence found during the run.

Relevant code includes the existing [Skill loader](../packages/core/src/tools/skill.rs), [tool registration and guidance](../packages/core/src/tools/mod.rs), and [agent system prompt](../packages/core/src/agent/messages.rs).

### 2. Add built-in rules for bug fixes and verification

- Add concise workflow rules to `system_prompt` for turns with execution access. Use the user's intent to distinguish a fix request from a verification-only request. Run relevant checks after making code changes, including when implementing an approved plan.
- For verification-only requests, use the existing file-reading, Git query, and shell tools. Instruct the agent to preserve source files and Git staging state. This is a task constraint for the agent. Existing settings and approvals still govern command execution.
- Apply the existing permission boundaries to `@arch` planning, revisions to plans awaiting approval, and background notifications. Reuse existing command approval, cancellation, and workspace scoping.
- In the final reply, list the checks that actually ran, their results, and what they covered. Include before-and-after evidence for bug fixes when available. Explain any missing reproduction conditions, gaps in existing checks, or blockers to running them. A successful compile check establishes that the code compiles.

Relevant code includes the [agent system prompt](../packages/core/src/agent/messages.rs) and [chat run permissions](../packages/core/src/chat/run.rs).

### 3. Show command outcomes in existing chat tool rows

- Carry the process outcome from the shell execution layer into the existing tool result and chat history. Today, `run_command` includes a nonzero exit code in its output, but the tool layer can still show success because it captured the output. Use the actual process outcome to distinguish a successful exit, a nonzero exit, and termination by a signal. Preserve the exit code and stdout and stderr.
- For foreground commands, use the existing outcomes for timeouts, cancellations, and denied approvals. Starting a background command returns a job ID. Show the job as in progress until it finishes and report the final result using the existing job status, output query, or completion notification.
- Reuse the chat tool row and its expandable output. Save the process outcome as an optional field on the existing message. Use the existing fields when displaying older conversations. The agent should base its conclusions on the relevant tool results.

Relevant code includes [shell execution](../packages/core/src/tools/shell.rs), [background jobs](../packages/core/src/tools/jobs.rs), [tool results](../packages/core/src/tools/mod.rs), [chat events](../packages/core/src/chat/run/stream.rs), [history restoration](../packages/core/src/chat/transcript.rs), and the [desktop tool row](../packages/desktop/src/features/chat/Process.tsx).

## Acceptance criteria

| Scenario | Expected result |
| --- | --- |
| Start a chat with no workspace or user Skills | The main agent receives exactly one index entry per built-in principle. The `skill` tool is available and the initial prompt contains summaries only. |
| Fix a duplicate write on retry in ordinary chat | The agent can load the idempotence principle without an `@arch` mention. The tool returns the requested body. |
| Use `@arch` to plan a state model change | The same index is available. The agent can load Model the Domain while the planning turn retains read-only permissions. |
| Rebuild the system prompt or try to shadow a built-in principle with a local file | The index is still present and the reserved name resolves to Oaklet's bundled content. Existing local skills are still resolved according to their normal precedence. |
| Fix a consistently reproducible bug | The same check fails for the reported issue before the fix and passes afterward. Chat shows both commands and their results. |
| Verify existing changes only | Oaklet runs relevant project checks. Source files and Git staging state match their state before verification. |
| Finish a code change | Oaklet runs relevant checks before replying and explains what behavior they cover. |
| A command exits with a nonzero code or is terminated by a signal | The tool row shows failure or termination. Expanding it shows the exit code or termination details and any available output. Reopening the conversation shows the same status. |
| A command times out, is canceled, or is denied, or a background job is still running | Oaklet shows the corresponding status. The final reply describes verification progress using the results available so far. |
| The project lacks a suitable check or reproduction conditions are missing | Oaklet identifies the behavior it could not verify and explains why. |
| Run `@arch` planning or restore an older conversation | Planning permissions still apply, and older command records remain readable. |

Add Rust tests for catalog completeness and exact-name loading, prompt contents in ordinary chat and planning, real process exits, command result propagation, and history restoration. Add desktop tests for tool row states and the fallback for older records. In the macOS Tauri app, use a small project with a reproducible bug to walk through both chat workflows and inspect which skill requests appear in the tool record. For desktop UI acceptance, use the target project's existing test driver or manual steps with observable results.

```sh
pnpm --dir packages/architecture-helper build
(cd packages/core && cargo test && cargo clippy --all-targets)
pnpm --dir packages/desktop test
```
