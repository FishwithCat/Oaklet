# Token 效率改造：从 DSH 借鉴上下文纪律

Status: plan · 2026-11-12 · scope: `packages/core`

本文是 Oaklet 从 [DeepSeek Harness](https://github.com/deepseek-ai/deepseek-harness) 借鉴
token 节约机制的落地清单。重点不是把 Oaklet 改造成 DSH，而是搬三条已经被验证的成本曲线：

1. **先 spill / 裁剪，再摘要**：零模型调用的手段优先。
2. **摘要请求复用前缀缓存**：压缩调用本身也要便宜。
3. **压缩后原文可召回**：敢删，是因为删掉的东西还能读回来。

> 基线说明：本文基于 2026-11-12 的 HEAD `73b55ae`。下表中的“已落地”项都已经在仓库里，
> 开始前先看 `git log` / `git status`，不要重复实现；本文本身尚未提交。

## 1. 已落地（不要重复做）

| 能力 | 位置 | 状态 |
| --- | --- | --- |
| 工具输出 spill | `packages/core/src/spill.rs`；`tools/mod.rs::prune_tool_output` | 已提交 |
| 大附件 spill | `chat.rs`（`ATTACHMENT_SPILL_BYTES = 16_000`，head/tail 预览） | 已提交 |
| 摘要复用前缀缓存 | `compaction.rs::summary_messages` / `summarize` | 已提交 |
| 摘要模型可配置 | `lib.rs::summary_model` | 已提交 |
| Provider 溢出恢复 | `agent.rs::RequestError` / `force` / 单次重试 | 已提交（`bf6604e`） |
| 压缩归档 + `history_read` | `store.rs::session_history`；`tools/history.rs` | 已提交（`73b55ae`） |
| 溢出时 L0 裁剪 | `compaction.rs::trim_tool_outputs` / `force` | 已提交（`73b55ae`） |

几个已经成立的设计约束，后续改动不要破坏：

- `history_read` 只在会话有归档后才注册，短会话不背这个 schema 成本。
- 摘要请求把 system prompt、tools、被压缩区间的消息原样重放，只在最后追加一条指令；
  不要改回“独立 summarizer system prompt + 扁平化 transcript”。
- 被替换的原文会和缩短后的 `messages` 在同一个事务里写进 `session_history`。
- 工具输出超过 12k 字符时先落盘，再裁剪；`read_file` 读 spill 文件不会再次 spill。

## 2. 剩余改造

| 优先级 | 改造 | 主要落点 | 效果 | 工作量 |
| --- | --- | --- | --- | --- |
| P0 | 正常压力下先做 L0 裁剪，再决定是否摘要 | `compaction.rs::compact` | 多数会话不再调用摘要模型 | 0.5–1 天 |
| P0 | 摘要失败时用机械摘要兜底 | `compaction.rs::compact_at` | 失败不再丢光历史 | 1 天 |
| P1 | completion / cache token 遥测 | `agent.rs`、`context.rs`、`store.rs`、UI | 能验证是否真的省了钱 | 0.5–1 天 |
| P2 | MCP 工具顺序稳定 / 按需加载 | `mcp.rs`、`tools/mod.rs` | MCP 多时省固定 schema 税 | 0.5–3 天 |
| P2 | `force` 裁剪保留 spill 指针 | `compaction.rs::trim_tool_outputs` | 不丢 spill 路径 | 0.5 天 |
| P3 | 并行只读工具 / 子代理 | `agent.rs` | 大仓库探索隔离 | 2–5 天 |

## 3. 详细方案

### 3.1 P0：正常压缩先做 L0

当前 `compact()` 只有一条路径：

```text
select_cut -> compact_at -> summarize
```

只要压力超过阈值，即使历史主要是旧工具输出，也会先付一次摘要调用。`force()` 已经在溢出路径上
先做 `trim_tool_outputs`，但正常压力路径没有。

建议改成“先试 L0，单独够用才提交”：

```rust
let trimmed = trim_tool_outputs_for_pressure(messages);
if estimate_tokens(&trimmed) <= trigger_at(window) {
    return Some(Compacted {
        messages: trimmed,
        replaced: original_tool_messages,
    });
}
// L0 单独不够：不要提交它，用原始 messages 走摘要，保住 prefix cache。
let compacted = compact_at(messages, ...)?;
// 摘要后可选：对保留的 tail 再做一次温和 L0，并把它替换的原文加入 replaced。
```

为什么是“试”而不是“先改再摘要”：如果 L0 已经改写了要被摘要的区间，摘要请求就不再是上一轮
请求的 prefix，cache 会从第一个被改写的 tool result 开始 miss。要么 L0 单独解决问题，要么
让摘要请求继续使用原始区间。

实现要点：

- 新增 `PRESSURE_TOOL_OUTPUT_CHARS`（建议 6k，head 4k + tail 1.5k），比 `force` 的 2k 温和。
- 只改 `role == "tool"` 的 `content`，保留 `tool_call_id`，不破坏 tool call/result 配对。
- `Compacted.replaced` 要带上被替换的原始 tool message，`history_read` 才能找回。
- 不要跳过 spill 文件本身；所有 tool 结果一视同仁。
- 不要把 `trim_tool_outputs` 直接复用到正常路径而不改预算；`FORCE_*` 是为溢出重试设计的。

验收：

- 在 32k 窗口下，20 条 8k 字符的 tool 结果 + 少量对话，L0 后低于 trigger，且没有发出摘要请求。
- L0 单独不够时，摘要请求的 body 与上一轮请求的前缀一致（mock server 可断言前缀）。
- 归档里有被 L0 替换的原文。

### 3.2 P0：机械摘要兜底

现在摘要失败或摘要不比原文短时，会退到常量 `DROPPED`，模型只看到一句“前面的对话被丢掉了”。
`history_read` 已经能找回原文，但模型不知道发生了什么，容易重复劳动。

借鉴 CliffCompaction 的规则，做一个确定性的 `mechanical_digest(messages[1..cut])`：

- 用户消息原文保留（每条设 sanity cap，比如 4k 字符）。
- assistant 文本保留（可 cap）。
- tool call 只留一行签名：`name(args 截断)`。
- tool 结果 ≤500 字符保留；超长换成 `[result omitted; use history_read for the original]`。
- 跳过上一版 `<compacted-summary>`（Cliff 的“绝不对 summary 再 summary”）。
- 如果 digest 不比被替换内容小，才退回 `DROPPED`。

使用位置：

- `compact_at` 的摘要错误 / 摘要不缩小分支（`drop_on_failure == true` 时）。
- `force` 里 prefix 放不下、当前直接 `assemble(..., DROPPED)` 的分支。
- 可选配置 `mechanical_fallback`，默认开启。

验收：

- 摘要端点返回空或超长时，checkpoint 含用户/assistant 文本和 tool 签名，不含大段 tool 正文。
- checkpoint 的估算 token 严格小于被替换区间。
- 原文仍可通过 `history_read` 取回。

### 3.3 P1：completion / cache token 遥测

`agent.rs` 现在只解析 `usage.prompt_tokens`，`Meter` 只拿它做锚点。结果是：无法判断 L0、
digest、spill 到底有没有减少计费 token。

建议把 `StreamDelta.usage` 从 `Option<usize>` 改成结构：

```rust
struct Usage {
    prompt: usize,
    completion: usize,
    cache_hit: usize,
    cache_miss: usize,
}
```

解析来源：

- 通用：`usage.prompt_tokens`、`usage.completion_tokens`。
- DeepSeek：`usage.prompt_cache_hit_tokens`、`usage.prompt_cache_miss_tokens`。
- OpenAI：`usage.prompt_tokens_details.cached_tokens`。

持久化与展示：

- 在 `runs` 表加四个 nullable 列（schema v3），或加一个 JSON `usage` 列；前者更好查询。
- `Meter` 继续只用 prompt 做锚点；usage 结构单独保存，供 UI 和后续统计使用。
- 植物 tooltip 显示 `input / cache hit% / output / 估算成本`；没有 usage 的 provider 显示
  “unavailable”，不要伪造 0。
- 缺失字段按 0 处理，不能让一个 provider 的省略字段打断请求。

验收：

- mock SSE 带 cache 字段时，四个值都能读出来并写进 run。
- 缺少 `completion_tokens` 时不影响主流程。
- 一个 turn 结束后，UI 能显示 cache hit 比例。

### 3.4 P2：MCP 工具顺序稳定 / 按需加载

**先做稳定性（低成本）**：`mcp::discover` 目前按 server 返回顺序 `push` 工具。不同进程/不同
server 版本的返回顺序变化，会让 tools 数组变化，从而打断 prefix cache。建议 discovery 结束后
按工具名排序（或至少在 `ToolRegistry::schemas()` 里排序），保证同一组工具每次请求的字节一致。

**按需加载（条件做）**：只有当遥测显示 tools 占 prompt 比例明显（比如 >20%）且 MCP 工具数量
很多时才值得。OpenAI-compatible chat 没有 Anthropic 的 `defer_loading`，不能照抄；客户端方案：

- 核心工具常驻：`read_file`、`write_file`、`edit_file`、`apply_patch`、`list_dir`、`run_command`、
  `skill`、`history_read`（有归档时）。
- 增加一个 `tool_search(query)`，返回匹配的 MCP 工具名 + 描述，并把它们加入当前 run 的
  “已激活”集合；后续请求才发送这些 schema。
- 搜索本身用 name + description 的简单匹配即可，不需要向量检索。
- 额外一轮往返是成本，所以只对“工具很多、但一次只用少数”的部署开启。

验收：

- 未搜索时，`schemas()` 只含核心工具 + `tool_search`。
- `tool_search` 命中后，下一次请求包含被激活工具的 schema。
- MCP server 返回顺序变化不改变 tools 数组顺序。

### 3.5 P2：`force` 裁剪保留 spill 指针

`trim_tool_outputs` 用 `FORCE_TOOL_OUTPUT_MARKER` 替换中段时，可能把原来 12k 裁剪时写入的
spill 路径一起切掉。虽然原文已归档、`history_read` 能找回，但模型看不到路径，会先尝试重跑命令。

建议：如果原文里已有 spill 标记（`.oaklet/spill/` 路径），把它提取出来，附在 force marker
后面；否则写一句 `[original available via history_read]`。这是一个小改动，但能减少一次无效重跑。

### 3.6 P3：并行只读工具 / 子代理

- **并行只读工具**：同一批 tool calls 里，`read_file` / `list_dir` / `grep` 可以并发执行；
  现在 `agent.rs` 是顺序 `for`。收益主要是延迟和减少往返轮次，token 收益间接。
- **子代理**：DSH 的 subagent 把探索放在独立上下文，父上下文只收摘要。Oaklet 可以做一个
  只读的 `explore` 子 run，但涉及嵌套取消、审批、UI，工作量大；只有大仓库用户多时再做。
- 不建议为了“像 DSH”而引入完整的 subagent/workflow/PTC 体系。

## 4. 不建议照搬

- **DSH 的 capability-seam / 插件体系**：Oaklet 是单体 Rust 应用，这层抽象不划算。
- **事件溯源 session log**：`session_history` 归档表已经够用；除非要做完整 replay/rewind。
- **Anthropic 专属的 `defer_loading` / `cache_edits`**：OpenAI-compatible 路由不通用。
- **`token-meter` 的 O(1) 投影复杂度**：Oaklet 只需要 usage 字段和简单 fold。
- **65KB `AGENTS.md` 预算**：桌面 agent 不需要这么长的常驻指令。
- **LLMLingua 式 prompt 压缩 / semantic cache**：有损、破坏精确标识符和缓存，不适合 coding agent。

## 5. 建议提交顺序（atomic，尚未执行）

按仓库的 Conventional Commits 约定，每项独立可回滚：

1. `feat(core): trim retained tool output before summarizing under pressure`
2. `feat(core): fall back to a mechanical digest when summarization fails`
3. `feat(core): record completion and cache token usage per run`
4. `perf(core): keep MCP tool schema order stable`
5. `fix(core): preserve spill pointers when trimming for overflow`
6. `feat(core): load MCP tools on demand`（仅在第 3 项数据支持时）
7. `feat(core): run independent read-only tools concurrently`（P3）

> `AGENTS.md` 要求提交前获得明确授权；以上只是提交信息建议，未执行 `git commit`。

## 6. 验证

```sh
cd packages/core
cargo test
cargo clippy --all-targets
```

新增测试建议：

- `compaction`：L0 单独够用时跳过摘要；digest 兜底；归档包含被替换原文。
- `agent`：usage 解析（prompt/completion/cache hit/cache miss）；溢出重试一次；`history_read`
  只在有归档时注册。
- `store`：`session_history` 与 `messages` 同事务写入；分页读取。
- `spill`：拒绝符号链接、重名后缀、完整内容 round-trip。

手工验证：

- 长会话里连续多次 `run_command`：植物下降后没有出现摘要调用，`history_read` 能翻到旧命令。
- 粘贴一个大日志：inline 只剩预览 + 路径，`read_file` 能分页读全文。
- 用小于配置窗口的模型发请求：触发一次 force，重试成功而不是直接报错。
- 改 MCP server 的返回顺序：tools 数组顺序不变（日志里对比两次请求体）。

## 7. 参考

- DSH `packages/compaction/compaction-tool-result-pruner/README.md` — 先裁剪再摘要。
- DSH `packages/compaction/compaction-basic/README.md` — 摘要复用 warm prefix。
- DSH `packages/spill/spill-policy/README.md` — 超限文本落盘 + 召回指针。
- DSH `packages/llm/token-meter/README.md` — cache read/write 与 usage 字段。
- DSH `.agents/notes/proposed/feature/2026-07-06-recallable-compaction.md` — 冻结 stub +
  state checkpoint + `history_read`/`history_search`。
- [CliffCompaction](https://arxiv.org/abs/2609.26779) — 机械截断、绝不 summary 套 summary。
