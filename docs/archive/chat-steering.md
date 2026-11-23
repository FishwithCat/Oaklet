# Oaklet 架构规划：运行中用户消息（Chat Steering）

用户可以在 agent 执行过程中继续输入。本方案把这条消息送进正在运行的 turn，而不是拒绝、丢弃或等它结束。

## 摘要

- 运行中按 **Enter** 发送的消息默认作为 **steer**：在下一个 step 边界注入当前 turn，模型在下次请求时看到，不终止当前 turn。
- 不新增 `⌘/Ctrl+Enter` 快捷键。Enter 是唯一手势，`⌘/Ctrl+Enter` 保持现状，与 Enter 等价。
- **Stop** 取消当前 run，并清空尚未投递的排队消息。
- 不打断正在进行的模型请求或工具调用；需要“立即停并改方向”时先 Stop，再发送。
- 复用后台任务通知已有的“忙时排队、step 边界消费”机制，把它泛化为 `PendingInput`，不新造一套。
- 首版不新增数据库迁移；排队消息只存在于内存，随 run 一起结束。已确认接受“崩溃时排队消息丢失”这一取舍，持久化留到 P2。

## 现状

`packages/core/src/chat/commands.rs::session_send` 是唯一发送入口。`start_run` 在 live 锁内检查同一 session 是否已有 run，有则返回 `"This chat is already working."`。

桌面端 `ChatComposer` 在 `running` 时把 Send 换成 Stop；键盘 Enter 仍会走到 `useAgentSession.send()`，但被 `tab.running` 直接 `return false`，表现为静默无响应。

已有的可复用先例是后台任务通知：`chat/notices.rs::job_settled` 在 session 忙时把消息放进 `ChatLive.notices`，`agent.rs` 的循环顶部调用 `ctx.take_notices()`，在每个 step 请求之前注入。用户消息可以走同一条路。

持久化模型也支持这个设计：`begin_run` 立即写入 user turn，run 期间产生的 assistant/tool 消息暂存在 `RunContext.pending`，`finish_run` 时一次性落库。因此排队队列放内存与现有“run 结束才落库”的取舍一致。

## 目标与非目标

目标：

- 运行中发送的消息不丢、不静默失败，并在界面上明确显示“排队中/纠偏中”。
- 在工具批次之间安全注入，保证对话历史合法。
- 用户未察觉的边界（最后一步、Stop、错误）有确定行为。

非目标（首版）：

- 不打断在途请求或工具调用。
- 不做跨进程/崩溃恢复的持久化队列。
- 不做多窗口同一 session 的同步投递。
- 不改变模型、工具权限或 plan 状态机。

## 用户流程

| 场景 | 行为 |
| --- | --- |
| 空闲时按 Enter | 与现在完全一致：立即开新 run。 |
| 运行中按 Enter | 消息进入排队区，线程尾部显示为 `steering` 气泡；当前 step 结束后注入当前 turn。 |
| 运行中按 `⌘/Ctrl+Enter` | 与 Enter 相同（不新增语义）。 |
| 运行中点 Stop | 取消当前 run，清空未投递消息；当前 turn 标记为 `Stopped.`。 |
| 消息在最后一个 step 到达 | run 正常结束后自动开一条新 turn 投递该消息，`turns` 加一。 |
| 运行中发送附件 | 与普通发送相同：图片进入模型内容，文本文件按现有规则内联或 spill。 |
| 队列达到上限 | 拒绝新消息并提示，已有队列不受影响。 |

## 语义与交互

### steer

消息在下一个 step 边界加入当前 turn：

- 不产生新的 user turn，`sessions.turns` 不变。
- 不改变 run 的 access（普通执行、只读规划、已确认 plan 都不变）。
- 消息文本里的 `@arch` 不重新解析；steer 只延续当前 run 的模式。
- 多条 steer 按到达顺序整批注入，模型一次看到。

### queue（P1）

首版不做。P1 增加“当前 turn 结束后再发送”的语义：

- 仍然只用 Enter 一个手势，通过设置项选择运行中 Enter 的行为（`纠偏` / `排队`），或由发送按钮旁的模式控件决定。
- 不新增键盘 chord。
- `queue` 消息在当前 turn 结束后作为新 turn 投递，一条消息一个 turn。

### 为什么延续 Enter

`shortcuts.ts::isSendEnter` 只判断 `Enter`、`!shiftKey`、非输入法组合，没有排除 `metaKey/ctrlKey`；今天 `⌘/Ctrl+Enter` 与 Enter 完全等价，也没有文档把它定义为独立快捷键。给一个未定义的组合赋新语义是一次不必要的行为变化，用户没有理由知道这个约定。首版保持 Enter 单一语义即可。

## 端到端路径

```text
ChatComposer (Enter)
  → Chat.submit()
  → useAgentSession.send(text, attachments, mode)
  → invoke("session_send", { sessionId, text, attachments, mode })
  → session_send: 校验 workspace / session / 构建 user turn
  → ChatState::enqueue_input
       ├─ 有活跃 run（且属于本窗口）→ 入队 + 更新 view.pending + emit agent:pending
       └─ 无活跃 run               → 退回 start_run，开普通 turn
  → Agent loop 顶部：take_steer_inputs()
       → record_message（进入落库缓冲）
       → 追加到 live view.items
       → emit agent:steered
       → messages.push(...)
  → make_room → 下一次模型请求
  → finish：未消费的排队消息按终态处理
```

关键不变量：

> 只在“所有 tool_call 都已有 result”的 step 边界注入，即 `agent.rs` 循环顶部、`tool_calls::run` 返回之后。绝不把 user 消息插进一个工具批次的中间，否则历史会变成非法对话。

## 后端设计

### 排队结构

`packages/core/src/chat.rs`：

```rust
pub struct PendingInput {
    pub id: String,            // state.next_id()
    pub mode: InputMode,       // Steer | Queue
    pub message: Value,        // 模型可见内容，含 image parts
    pub display: Value,        // text + attachments，UI 用
    pub created_at: i64,
}

struct ChatLive {
    // ...
    pending: HashMap<String, Vec<PendingInput>>,  // 以 session_id 为键
}
```

`SessionView` 增加 `pending: Vec<PendingInputView>`，序列化后不带模型侧 `message`，只保留 `id`、`text`、`attachments`、`mode`。这样 `session_get` 从另一个客户端或窗口重新水合时，也能看到排队状态。从 DB 读取的空闲 session 默认为空数组。

队列上限建议 16 条，超过返回明确错误，避免刷屏。

### 命令入口

扩展现有 `session_send`，不新增命令（当前只有一个前端调用点）：

```rust
pub async fn session_send(
    ...,
    text: String,
    attachments: Vec<IncomingAttachment>,
    mode: Option<SendMode>,   // 缺省 Normal
)
```

`SendMode`：`normal | steer | queue`。

逻辑：

1. 校验 workspace、session 存在、workspace 目录存在（复用现有代码）。
2. `build_user_turn` 构建 `(message, display, names)`；标题只在 `meta.turns == 0` 时生成。
3. 锁 `ChatLive`：
   - 有该 session 的 run：
     - `closing` / `compacting` / `confirming` → 复用现有错误文案。
     - run 不属于当前 window → `"This chat is running in another window."`。
     - `mode == Normal` → 保持 `"This chat is already working."`。
     - `steer` / `queue` → push `PendingInput`，更新 `handle.view().pending`，克隆 view，解锁，emit `agent:pending`，返回 `SendResult { run_id: 现有 run_id, session }`。
   - 没有 run：`mode` 视为普通发送，走现有 `start_run`。这同时处理“前端以为忙、后端已空闲”的竞态，消息不会丢。
4. 入队成功后清空 `wakes`，与普通用户消息一致，终止 job notice 的唤醒链。

### step 边界消费

`RunContext` 增加：

```rust
pub fn take_steer_inputs(&self) -> Vec<PendingInput>;  // 只取 mode == Steer
pub fn has_pending_steer(&self) -> bool;
```

实现放在 `ChatState` 内，在 live 锁内同时改队列和 view，避免“入队”和“消费”两个竞态窗口：

```text
live 锁
  → 找到 run
  → 从 pending 中拆出 Steer 项
  → view.pending = 剩余项
  → view.items 追加 user item（text + attachments）
  → 返回被消费项
解锁后：emit agent:steered
```

`agent.rs` 循环顶部，在 `take_notices()` 之后：

```rust
for notice in ctx.take_notices() { ... }        // 现有，hidden

for input in ctx.take_steer_inputs() {          // 新增
    let message = ctx.record_message(
        input.message,
        Some(json!({ ...input.display, "steer": true, "runId": run_id })),
    );
    messages.push(message);
}
```

顺序：notice 在前、user steer 在后。模型最后看到的是用户最新意图，且 hidden notice 不会把两条 user 消息隔开。

`make_room` 在注入之后执行，所以 steer 增加 token 时会立刻走正常压缩路径；压缩保留最近的尾部消息，刚注入的 steer 在尾部，不会被吞掉（补测试确认）。

### turn 结束时不丢消息

只靠循环顶部还不够：如果 steer 在最后一次请求过程中到达，而这次回复没有 tool_call，循环会直接 `finish_done`。

两处兜底：

1. `agent.rs` 的 `reply.calls.is_empty()` 分支：

   ```rust
   if reply.calls.is_empty() {
       if ctx.has_pending_steer() { continue; }  // 带着 steer 再走一步
       ctx.finish_done(...).await;
       return;
   }
   ```

   助手这条最终回复已经 `record_message` 过，`continue` 后下一步会把它连同 steer 一起发给模型。

2. `chat/run/finish.rs::finish` 在 `live.runs.remove` 之后，检查该 session 是否还有排队消息：
   - `done` / `error`：取第一条开新 run（`RunTrigger::User`，不是 `Notice`，否则会被降成只读），其余留在队列里，由新 run 的 step 1 继续消费。`Queue` 项不在 step 1 消费，等新 turn 结束再开下一条。
   - `stopped`（用户点 Stop）：清空队列。用户明确停止时，不再自动开跑。
   - `stopped` 且是 P2 的 `interrupt & send`：保留队列并开新 run。

`done` 情况的新 run 会 `begin_run`，`turns` 正常加一；被消费的 steer 只是当前 turn 内的一条消息，不增加 turn 数。

### 终态处理

| 终态 | 未消费队列 | 说明 |
| --- | --- | --- |
| done | 第一条开新 run，其余保留 | 不丢用户输入 |
| error | 同上，只自动尝试一次 | 用户已明确要发；失败也不循环 |
| stopped（Stop） | 丢弃 | 停止语义优先 |
| stopped（interrupt&send） | 保留并开新 run | P2 |
| 窗口关闭 / 退出 | 丢弃 | 已有 `interrupted` notice |

### 边界与不变量

- **plan pending**：steer 只延续当前 run 的 access；run 结束后若 plan 待审，新 run 仍走 `RunTrigger::User` + `pending_plan`，保持只读规划。
- **审批阻塞（blocked）**：允许入队；消息在当前工具结算后的 step 才送达。审批卡仍需要用户处理。不做“steer 自动拒绝审批”。
- **附件**：入队时就执行 `build_user_turn`（spill 文件此时写入）；取消后 spill 文件成为孤儿，由现有 30 天清理兜底。
- **多窗口**：workspace 与窗口一一对应，正常不会出现；仍加 window 校验作为防御。
- **崩溃**：首版队列在内存，崩溃会随 run 一起丢；但 run 本身也是 finish 才落库，属于同一取舍。已确认接受，持久化留到 P2。
- **分页对齐**：`store/events.rs::is_visible_user_event` 会把中途的 user 事件当成页边界。首版可以不改；如果页首出现“从一条 steer 开始”的观感问题，再加 `display.steer != true` 排除。
- **重复气泡**：不做乐观 user item，`pending` 是唯一权威状态；claim 时后端同时移出 `pending` 并追加 `items`，完成后由持久化投影替换，不会重复。

## 前端设计

### 事件

新增两个 window-scoped 事件，注册在 `agent-events.ts`：

```ts
type PendingEvent = {
  sessionId: string;
  pending: PendingInput[];
};

type SteeredEvent = {
  sessionId: string;
  runId: string;
  items: Item[];        // 本次被消费的 user item
  pending: PendingInput[];
};
```

- `agent:pending`：入队 / 取消时更新排队区。
- `agent:steered`：step 边界消费时，把 pending 移入线程，同时更新剩余队列。

最终 `agent:done` 的 `SessionView` 已经包含落库后的 user item（带 `seq`），`mergeWindow` 会用持久化投影替换 live 行，不会重复。

### 会话模型

`session-model.ts`：

- `SessionView` 增加 `pending: PendingInput[]`。
- 增加 `applyPending`、`applySteered`。
- 确认 `mergeWindow` 的 `withItems(incoming, items)` 会带过来 `pending`。

### 发送路径

`useAgentSession.send` 签名改为：

```ts
send(text, attachments, mode: "normal" | "steer" | "queue")
```

- `tab.running` 时不再 `return false`；把 `mode` 传给后端。
- 不为 steer 做乐观 user item。后端返回的 `session.pending` 就是权威状态，省掉“乐观气泡 + 去重”的复杂度。
- 对 steer 的返回值只合并 `pending`、`title`、`turns`、`updatedAt`、`running`、`activeRunId`，不要整体覆盖 `items`，否则会冲掉正在流式输出的内容。
- `mode === "normal"` 且后端返回 `"already working"` 时，可重试一次 `steer`，覆盖 job notice 唤醒与前端 running 标志之间的短暂竞态。

### 输入框

`ChatComposer.tsx`：

- 运行中不再只显示 Stop：改为 `[Stop] [Send]` 两个按钮（或 Stop 小方块加 Send 主按钮）。Send 的 tooltip 显示当前模式。
- 运行中按 Enter 走 steer；`isSendEnter` 保持现状，不读 `metaKey/ctrlKey`。
- 输入法组合期间不触发，沿用现有 `composing` 判断。
- 在 composer 上方或帮助里提示“运行中 Enter 会纠偏当前回合”。

### 排队消息渲染

首版放在 `ChatThread` 的线程尾部（`items` 之后），不做独立 dock：

- 一个淡化样式的 user 气泡，右侧小标签 `steering` / `queued`。
- P1 再加 `×` 取消；首版可以先只展示。
- `ChatThread` 增加 `pending` prop，`Chat.tsx` 透传。
- 滚动依赖里加入 `pending.length`，否则新排队消息不会触发贴底。

样式加在 `styles/chat.css`：`.msg.user.pending`、`.pending-tag`、双按钮 footer。

## 与 deepseek-harness 的对照

参考实现位于 `/Users/linear/Workspace/deepseek-harness`。

| Harness 概念 | 位置 | Oaklet 对应 |
| --- | --- | --- |
| `Inbox` 两条队列 `next-step` / `next-turn` | `packages/core/agent-loop/src/inbox.ts` | `ChatLive.pending` + `InputMode::Steer/Queue` |
| `agent.steer()` / `followup()` | `packages/core/agent-loop/src/agent.ts` | `enqueue_input(Steer/Queue)` |
| step 前 `claim` 整批 | `agent.ts::preStep` | `agent.rs` 循环顶部 `take_steer_inputs` |
| 持久化 `agent/inbox/spliced` | `inbox.ts` + session projection | P2：`pending_inputs` 表或新事件类型 |
| `prompt(mode: 'queue' \| 'steer')` | `packages/api/session-controller/src/commands.ts` | `session_send` 的 `mode` |
| busy Enter 偏好 + 加速键翻转 | `ui-conversation/submission-settings.ts`、`submission-policy.ts` | P1 的设置项；首版不引入 chord |
| 排队项 dock、可单条 steer/删除 | `ui-conversation` 队列 dock | P1 的 pending 行 + 取消/转 steer |
| `cancel({ keepInbox })` | `agent.ts` | P2 的 `interrupt & send` |

不照搬：projections、durable splice 事件、ACP / 多 agent inbox。Oaklet 是一 session 一 run、事件按 window 分发、turn 在 finish 时整批落库，直接复用 `notices` 的机制成本最低。

## 实现步骤

### 1. 后端排队结构与视图

- 在 `chat.rs` 增加 `PendingInput`、`ChatLive.pending`、`SessionView.pending`。
- `view_from_session` / `view_from_window` 初始化为空。
- `transcript.rs` 增加从 `display` 构建 user item 的 helper，供 live view 与投影共用。
- 涉及 [会话状态](../packages/core/src/chat.rs)、[历史投影](../packages/core/src/chat/transcript.rs)。

### 2. 发送入口分流

- `session_send` 增加 `mode` 参数；抽取“构建 turn + 校验”的公共路径。
- `ChatState::enqueue_input`：live 锁内完成“有 run 则入队、无 run 则返回”，调用方再走 `start_run`。
- 上限、window、closing/compacting/confirming 校验。
- 涉及 [会话命令](../packages/core/src/chat/commands.rs)、[运行入口](../packages/core/src/chat/run.rs)。

### 3. step 边界注入

- `RunContext::take_steer_inputs` / `has_pending_steer`。
- `agent.rs` 循环顶部消费；回复没有 tool_call 且有待投递 steer 时 `continue`。
- 顺序保持在 `take_notices` 之后、`make_room` 之前。
- 涉及 [Agent 循环](../packages/core/src/agent.rs)、[任务通知](../packages/core/src/chat/notices.rs)。

### 4. 结束与停止语义

- `finish.rs` 在移除 run 后检查队列：`done` / `error` 用第一条开新 run，`stopped` 清空。
- 复用 `notices.rs` 的唤醒路径，但新 run 使用 `RunTrigger::User`，保留执行权限。
- 入队与普通发送都清空 `wakes`。
- 涉及 [运行收尾](../packages/core/src/chat/run/finish.rs)、[运行入口](../packages/core/src/chat/run.rs)。

### 5. 前端事件与发送路径

- `agent-events.ts` 增加 `agent:pending`、`agent:steered`。
- `session-model.ts` 增加 `pending` 类型与 apply 函数。
- `useAgentSession.send` 增加 `mode`，运行中不再拒绝；只合并 pending 等字段。
- `Chat.tsx` / `ChatComposer.tsx`：提交时传 `mode`，运行中保留 Send 按钮。
- 涉及 [桌面会话入口](../packages/desktop/src/features/chat/useAgentSession.ts)、[事件订阅](../packages/desktop/src/features/chat/agent-events.ts)、[会话模型](../packages/desktop/src/features/chat/session-model.ts)、[聊天页](../packages/desktop/src/features/chat/Chat.tsx)、[输入框](../packages/desktop/src/features/chat/ChatComposer.tsx)。

### 6. 排队消息 UI

- `ChatThread` 渲染 `pending` 行与 `steering` 标签。
- `Chat.tsx` 透传；滚动依赖加入 `pending.length`。
- `styles/chat.css` 增加样式。
- 涉及 [聊天渲染](../packages/desktop/src/features/chat/ChatThread.tsx)、[聊天样式](../packages/desktop/src/styles/chat.css)。

### P1

- `queue` 模式与设置项（运行中 Enter：纠偏 / 排队）。
- pending 行支持取消、单条转为立即纠偏。
- pending dock 提供“全部立即纠偏”按钮，不额外占用键盘手势。

### P2

- **interrupt & send**：入队 + 标记 redirect + cancel；`finish` 的 stopped 分支保留队列并开新 run。
- **持久化队列**：新增 `V1__pending_inputs.sql`（`id, session_id, mode, message, display, created_at`），入队插入、claim/finish 删除；启动时把遗留项标成“未发送”给 UI 重试。
- 可选 `agent:run_started` 事件，消除 finish 后自动续跑与前端 `running` 标志之间的短暂空窗。

## 首版验收

| 场景 | 通过条件 |
| --- | --- |
| 空闲时发送 | 与现在一致：立即开 run，无排队区。 |
| 运行中按 Enter | 线程尾部出现 `steering` 气泡；下一个 step 的请求体包含该消息；`turns` 不变。 |
| 多条 steer | 按顺序整批注入，模型一次看到；顺序与发送顺序一致。 |
| 最后一步到达 | run 正常结束后自动开新 turn，消息落库，`turns` 加一。 |
| 点 Stop | run 标记 stopped，未投递队列清空；已消费的消息落库并显示 `Stopped.`。 |
| 附件 | 图片/文本进入模型内容；重载后 bubble 仍带附件缩略图。 |
| 队列上限 | 超过 16 条被拒绝，已有队列不受影响，界面显示错误。 |
| 重载 / 另一客户端 | `session_get` 返回 pending；空闲 session 的 pending 为空。 |
| plan 待审 | steer 不改变只读 access；run 结束后仍停在待审状态。 |
| 崩溃 | run 标记 interrupted；首版排队消息丢失，已确认接受；持久化留到 P2。 |

## 验证命令

```sh
(cd packages/core && cargo test && cargo clippy --all-targets)
pnpm --dir packages/desktop test
pnpm --dir packages/desktop build
```

## 已确认的决策

1. **`done` / `error` 后自动续跑**：默认自动投递一次，不无限重试。`finish` 在 run 结束后取第一条排队消息开新 turn；如果这次也失败，队列已清空，不再自动重试。
2. **pending 展示位置**：首版放线程尾部，不引入独立 dock。P1 增加取消 / 转为立即纠偏时再评估是否需要 dock。
3. **崩溃丢失排队消息**：首版接受该取舍，不新增迁移；持久化队列（`V1__pending_inputs.sql`）留到 P2。
