# Oaklet 架构规划：@arch 显式触发与 LikeC4 Model Diffs

用户沿用同一个聊天输入框。架构规划是显式能力：只有用户主动使用 `@arch` 时才进入只读的架构规划与 Model Diff 比对流程。普通消息继续走现有执行流程，即使改动触及架构，也不会自动生成方案卡片；完成后可以在回复中提示用户用 `@arch` 重新发起，以获得带校验的模型 Diff。`@` 命令表按可扩展的方式设计，输入框提供智能补全。

规划阶段只读，草案随聊天历史保存。第 4 步交付基础卡片和实施确认，第 5 步补齐可视化审阅。保存 `.c4` 草案、在 PR 中评审和将草案合并进架构源文件可以作为独立功能交付。

## 用户流程

| 用户请求 | Oaklet 的行为 |
| --- | --- |
| 「修复发送按钮的样式」 | 走现有执行流程，不查询模型，不生成方案。 |
| 「@arch 新增一个后台 Worker」 | 进入只读规划：查询现有 LikeC4 模型，生成实施步骤和 Model Diff，展示方案卡片并等待审阅。 |
| 「新增一个后台 Worker」（没有 `@arch`） | 按普通执行流程改代码。改动触及架构时，在完成后的回复里提示可以用 `@arch` 重新发起以获得校验过的模型 Diff，但不自动转入规划。 |
| 「@arch 评估一下把队列拆出去」 | 只读评估，给出可验证的草案，停在审阅阶段。 |
| 方案待审阅时继续追问或修订 | 不需要重新输入 `@arch`，同一会话沿用该方案继续保持只读。点击 **按方案实施** 后才解锁写入。 |

`@arch` 可以出现在消息开头或任意空白之后，大小写不敏感；邮箱地址里的 `@` 和未注册的 `@命令` 都按普通文本处理。存在多个模型时，优先沿用 Architecture 面板中有效的选择，否则在聊天中询问用户。模型缺失、编译错误或 helper 不可用时，`@arch` 仍会给出文字方案，并在卡片上说明无法校验的原因。

## @ 命令与智能补全

- 命令表由后端裁决：Rust 侧的 `mentions::COMMANDS` 解析并决定运行模式，桌面端的命令表只负责菜单名称和说明。两处名称由测试比对，新增命令时同步修改；后端注册表是唯一的行为来源。
- 输入框在消息开头或空白之后输入 `@` 时打开补全菜单，按已输入前缀过滤命令。`↑` / `↓` 切换选中项，`Enter` 或 `Tab` 插入 `@命令 `，`Esc` 关闭菜单。菜单只替换当前命令令牌，并保留其后已有的空格。
- 中文输入法组合期间不打开菜单；邮箱地址不触发补全。未知的 `@` 文本照常发送，不会改变运行模式。
- 历史消息保留原文，会话标题去掉命令令牌。后续增加命令时，只需在两张命令表加名称、在 `session_send` 的分流里加对应分支，补全菜单自动生效。

## 实现步骤

### 1. 验证 LikeC4 原生 Diff 能力

- 当前工作区的 helper 和桌面依赖已更新到 `1.61.0`，这是上游引入原生 Model Diffs 的版本。保持两处依赖版本同步。 [上游发布说明](https://github.com/likec4/likec4/releases/tag/v1.61.0)
- 现有 Diff 测试覆盖多文件模型与内存草案、无效引用、重叠告警和对比视图布局。集成时继续验证 Architecture 普通视图、MCP 查询和打包应用里的 helper 加载。 [Diff 测试](../packages/architecture-helper/test/diff.test.mjs)
- 确认桌面嵌入组件如何消费上游的对比视图数据。首版交付依赖 DSL 校验及视图摘要，图形对比单独验收。 [LikeC4 Model Diffs](https://likec4.dev/dsl/diff/)

涉及 [helper 依赖](../packages/architecture-helper/package.json)、[桌面依赖](../packages/desktop/package.json)、[编译 helper](../packages/architecture-helper/src/compile.mjs) 和 [Tauri 打包资源](../packages/core/tauri.conf.json)。

### 2. 接通 @arch 显式触发、只读边界与命令补全

- 在 `session_send` 入口解析 mention：出现 `@arch` 的回合使用规划触发，其余回合使用普通执行触发。后端不再根据自然语言推断“是否涉及架构”，也不在普通回合自动进入只读规划。待审阅方案的后续修订回合视为规划回合，避免用户为了改一句话重复输入 `@arch`。
- 只有 `@arch`、方案修订和方案实施确认回合注入架构规划提示词和规划工具。普通回合沿用现有工具和授权；即使模型看到 LikeC4 源文件，也不展示 `present_plan` 或 `validate_architecture_diff`，后端拒绝这些调用。普通回合完成后可提示 `@arch`，但不能自行切换运行模式。
- 规划回合在工具执行入口保持只读：允许读取文件、列目录、搜索和受限的 LikeC4 查询，拒绝写文件、补丁、Shell、后台任务操作和外部 MCP。`assess_change` 只记录意图与架构影响用于生成卡片，不能打开写入。LikeC4 bridge 同时限制可发现与可调用的子工具，模型加载只读取受支持的源文件和配置。
- 命令解析放在 [mentions.rs](../packages/core/src/chat/mentions.rs)，桌面补全表放在 [mentions.ts](../packages/desktop/src/features/chat/mentions.ts)，菜单在 [ChatComposer](../packages/desktop/src/features/chat/ChatComposer.tsx) 中渲染。补全测试同时读取 Rust 命令表，防止两端名称漂移。
- 用现有 JSON 会话事件记录待审阅方案。只读运行期间以及方案待审阅时，后台任务通知入队。启动运行时在同一保护范围内检查待审阅状态，避免自动唤醒与用户发送交错并绕过确认。

涉及 [会话命令](../packages/core/src/chat/commands.rs)、[运行入口](../packages/core/src/chat/run.rs)、[权限门](../packages/core/src/tools/mod.rs)、[Agent](../packages/core/src/agent.rs)、[系统提示词](../packages/core/src/agent/messages.rs)、[命令解析](../packages/core/src/chat/mentions.rs)、[桌面补全](../packages/desktop/src/features/chat/mentions.ts) 和 [任务通知](../packages/core/src/chat/notices.rs)。

### 3. 查询现状并校验草案

- 复用 Architecture 的模型发现、`read_sources` 和 `sourceDigest`。只把目标模型根目录和已选范围传给规划回合，后端重新校验它属于当前工作区。Agent 先查询项目概要、相关元素和关系，再根据需要读取源文件，引用实际元素 ID 和源码路径。
- 加一个规划阶段使用的草案校验工具。输入为完整的原生 `diff` DSL，校验范围固定为已选模型。由后端生成虚拟文件名，把草案追加到内存源文件集合中。用上游解析结果约束草案只包含一个顶层 `diff`，检查 ID 与现有 Diff 的冲突，并沿用源文件大小限制、超时和取消处理。
- 工具返回模型根目录、基线 `sourceDigest`、Diff ID、原始 DSL、诊断、变更摘要、受影响视图和重叠告警。摘要使用 LikeC4 的结果。提交前核对当前源文件与本轮查询时的基线，若文件已变更，要求重新查询并校验。语法或语义错误返回给 Agent 修订，重叠继续显示为告警。 [验证和重叠规则](https://likec4.dev/dsl/diff/#validation)

涉及 [模型发现及摘要](../packages/core/src/architecture/discovery.rs)、[LikeC4 bridge](../packages/core/src/tools/likec4.rs)、[Rust 编译入口](../packages/core/src/architecture/compiler.rs) 和 [编译 helper](../packages/architecture-helper/src/compile.mjs)。

### 4. 展示方案卡片并接通实施确认

- 随现有会话事件保存方案版本、实施步骤和后端生成的校验结果。方案卡片展示步骤、可复制的 DSL、受影响视图和告警，历史会话通过现有 transcript 恢复。只有后端成功校验的 Diff 才显示为「已验证」。草案存在校验错误时，可以继续修改。模型或工具不可用而只能给出文字方案时，显示「按文字方案实施」，仍需用户主动确认。
- 增加方案确认命令。桌面端传会话 ID 和方案版本，后端从事件中读取该版本的计划和 Diff，校验工作区归属、版本是否为最新待审阅版本、当前基线以及是否已有运行。检查通过后，在现有启动保护范围内消费确认并启动执行，以免重复点击启动两次。新一版草案使旧卡片失效，基线变化则提示更新方案并重新确认。
- 待审阅期间，该方案的追问和修订继续走只读规划，由当前输入框完成；实施只能从卡片确认启动。方案实施回合不再要求 `@arch`，它绑定的是已确认的版本。

涉及 [聊天渲染](../packages/desktop/src/features/chat/ChatThread.tsx)、[桌面会话入口](../packages/desktop/src/features/chat/useAgentSession.ts)、[历史投影](../packages/core/src/chat/transcript.rs) 和 [消息事件](../packages/core/src/store/types.rs)。

### 5. 增加可视化方案审阅视图

- **卡片默认展示方案概览。** 用自然语言概括目标和改动，按执行顺序列出步骤。每步可展开查看预期结果、相关文件或组件、前置依赖及验证方式。将影响实施决策的风险和待确认事项放在确认按钮上方。就地提供 **查看方案详情**，展开后沿用同一份计划记录。
- **用可读清单呈现架构影响。** 详情按「新增」「修改」「移除」列出元素和关系，以模型中的名称和关系描述为主，元素 ID 和原始 DSL 放在可展开的详情中。若某项改动已关联到实施步骤，点击该项可以定位到对应步骤。架构清单来自后端校验结果，方案说明和实施步骤保留 Agent 给出的依据。
- **按需打开架构对比。** 从受影响视图列表选取一项后，复用 Architecture 面板的图组件和 LikeC4 的对比布局，默认打开「变更」，可切换到「现状」和「变更后」。三种状态沿用同一布局。图中的增删改同时用颜色和文字或图标标识，影响清单也能独立用于审阅。 [Architecture 面板](../packages/desktop/src/features/architecture/Architecture.tsx) · [LikeC4 对比视图](https://likec4.dev/dsl/diff/#compare-the-model-before-and-after-the-diff)
- **让详情、图和确认绑定到同一方案版本。** 按需用模型根目录、`sourceDigest` 和 Diff ID 加载对比数据，并核对返回结果的方案版本，避免旧请求覆盖新方案。没有架构变更、模型不可用或图加载失败时，仍展示步骤和验证方式，并说明架构校验或预览的状态。基线过期时，保留原方案供阅读，待新版本通过校验后再开放实施确认。

## 首版验收

| 场景 | 通过条件 |
| --- | --- |
| `@arch` 给已有服务新增后台 Worker | 从普通聊天输入 `@arch` 发起。Agent 先查询目标模型，再展示实施顺序、有效 `diff`、变更摘要和受影响视图。确认前工作区源文件一致。点击「按方案实施」后启动该版本的实施。 |
| 不带 `@arch` 的同类请求 | 走现有执行流程，不出现方案卡片，也不因架构影响自动转只读。回复提示可用 `@arch` 获取带校验的模型 Diff。 |
| 输入 `@` 或 `@ar` | 补全菜单按前缀列出 `@arch`，`↑` / `↓` 可切换，`Enter` 或 `Tab` 插入并保留空格，`Esc` 关闭。邮箱地址和未知命令不触发。 |
| 修复现有组件样式，或只评估方案 | 不带 `@arch` 的样式修复沿用现有执行路径；带 `@arch` 的评估停在只读方案。 |
| 草案引用不存在的元素 | 返回 LikeC4 诊断，聊天不显示已验证。修正后可以重新校验并复制 DSL。 |
| 已有草案与新草案重叠 | 聊天显示 LikeC4 的重叠告警，现有草案和架构文件保持原样。 |
| 多模型、无模型或模型损坏 | 多模型的校验仅作用于选中的模型。无可用模型时，显示原因并保留文字方案供用户确认。 |
| 只读权限与后台任务 | 规划期间直接请求写文件、Shell 或未开放的 MCP 工具会被后端拒绝；普通回合仍可正常执行。待审阅时的普通追问和后台通知都无法启动已审方案的执行。 |
| 历史恢复、修订与重复确认 | 重开会话能查看并复制原草案。旧方案版本、过期基线和重复确认无法启动实施。修订后的最新版本通过重新校验后可以确认。 |

自动化验证覆盖 helper 中的真实 DSL、Rust 的流程分流、只读权限、mention 解析和确认校验，以及桌面端的补全、方案卡片和历史恢复。运行以下现有检查，并在 macOS 打包版本手动走一遍 Worker 场景。

```sh
pnpm --dir packages/architecture-helper test
(cd packages/core && cargo test && cargo clippy --all-targets)
pnpm --dir packages/desktop test
pnpm --dir packages/desktop build
```

## 第 5 步验收

- **读懂方案。** 用 `@arch` 新增后台 Worker 的请求验证，只看卡片就能知道为什么新增 Worker、它如何接收任务、实施先后和验证方式。查看详情可定位受影响的关系和对应步骤，原始 DSL 可按需展开并复制。
- **核对架构影响。** 在同一个视图切换「现状」「变更」「变更后」，确认节点位置一致，新增、修改和移除的元素或关系与清单一致。仅靠文字和图标也能区分变更状态。
- **验证无图与过期场景。** 文字方案和预览失败的方案仍可查看实施步骤。恢复历史会话或快速切换方案时，概览、清单、图与确认操作都对应当前版本。基线过期时提示重新校验，确认操作保持禁用。
