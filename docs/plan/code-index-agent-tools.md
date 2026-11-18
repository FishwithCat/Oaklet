# Agent 语义工具：实现与验证

日期：2026-11-18。对应主计划步骤 7；状态、设置和重建界面留在步骤 8。

## 实现

- `tools/code.rs` 注册 `code_symbols`、`code_definition`、`code_references`、`code_calls`。`with_host_tools` 克隆 `AppState.code_index` 的共享句柄；执行时只使用 `ToolContext.workspace` 与当前调用的 `Cancel`。沿用按名称分发、审批和输出管线，没有增加 MCP、HTTP、Tauri 查询命令或新的服务器池，也没有修改 `ToolContext`。
- 四个工具均为只读、允许并发，不要求审批。提供各自 schema、描述和摘要，并共享去重的 guidance；guidance 进入模型的 system prompt。索引错误通过工具结果返回，不进入标为 MCP warning 的注册警告列表。
- `code_symbols(path)` 查询文件符号，`code_symbols(path, query)` 经新增的类型化 `Query::WorkspaceSymbols` 调用标准 `workspace/symbol`。`path` 必须是已发现的源码文件，由 Service 选择所属项目；适配器按项目根过滤，Service 再排除未发现、忽略、其他语言或其他嵌套项目的目标。项目符号搜索始终实时查询，不作为经过完整性验证的缓存。
- 定义映射 `textDocument/definition`；引用映射 `textDocument/references`，发送 `includeDeclaration: false`；调用映射 prepare/incoming/outgoing。一层调用结果保留 prepare 候选的 `subject`、目标 `target` 和调用点 `sites`，不合并同名候选或递归边。prepare 仍最多处理 32 个候选，超过时明确截断。
- schema 与执行时共同验证参数：路径不能为空、行列为正整数、limit 为 1–200；query 只能用于符号工具，方向只能用于调用工具且必填。项目搜索 query 为 1–1024 个字符；拒绝未知字段及方向。文件和项目路由仍由既有 Workspace/Service 检查，工具不重新实现发现与索引更新。
- 模型输入行列从 1 开始，列按 Unicode 字符计数，工具转换为内部零起始 UTF-16，LSP 层继续转换服务器协商编码。位置查询同时核对转换时的输入文本、实际提交给服务器的文本 hash 和返回后的磁盘内容；遇到保存竞态返回未就绪。
- 内部结果返回工作区相对路径、1 起始 Unicode 范围；嵌套符号展开为带 `container` 的条目。外部目标只返回路径和范围，明确 `external: true` 与 `column_encoding: utf-16`，不为转换列号读取外部源码；工作区内指向外部的符号链接同样按外部处理。
- 结果包含 status、source、coverage、generation、completeness_known、truncated、limit 和 position_base。来源保留 live/memory/sqlite/file_index；不会把缓存命中或协议 Ready 报为完整索引。当前空语义结果因缺乏完整性依据继续返回 not_ready，而不是断言没有定义、引用或调用。
- 默认最多 50 项，最高 200 项；符号子项和调用点也计入预算，任何裁剪均置 truncated。调用元数据 subject/target 不另占条数。工具结果继续由现有管线保存完整的已返回结果并裁剪展示，模型可以通过 spill 指针使用 read_file 分页恢复；limit 已排除的后端条目不在 spill 中。
- 区分 unavailable、unsupported、not_ready、timeout 和 invalid_arguments；取消沿用工具 Cancelled。guidance 提示缺少后端或能力时继续 grep/read_file，并将文本匹配标为候选。单次工具还有既有管线的 120 秒总期限；LSP 单请求继续使用 30 秒期限。
- 没有新增依赖、数据库迁移或前端配置；文件符号继续复用步骤 6 的持久化和缓存，定义、引用、项目搜索及调用均实时查询。

## 自动验证

core 完整测试：356 项通过、2 项默认忽略的真实工程探针；严格 `cargo clippy --all-targets --offline --locked -- -D warnings`、`cargo fmt --all -- --check` 与 diff 空白检查通过。新增或扩展覆盖：

- schema/参数组合、默认及最大条数、错误状态映射；guidance 去重并进入 system prompt，warnings 无索引状态。
- Emoji、CRLF 和 UTF-16/Unicode 转换，嵌套同名符号、容器和截断；不存在的外部文件及外部符号链接不读取。
- 真实工具管线里的定义、引用、incoming/outgoing；同名 prepare 候选、递归边、32 个 prepare 上限以及调用点共享条数预算。
- 项目搜索排除未发现或非源码目标；50/200 项结果被 spill 后，可用 read_file 的 offset/limit 恢复。
- 取消一个会话的在途查询会发送取消请求，另一个会话仍可查询同一服务器；unsupported 不冒充空调用图，关闭工作区返回 unavailable，grep 继续可用。
- 沿用 LSP 请求超时发送取消且会话仍可使用的测试，以及 Service 多工作区隔离、文件同步和缓存回归。

未改前端，本步没有重复 desktop 测试/构建；没有运行 GUI、模型服务或 Windows/Linux 原生监听实测。真实模型是否更少使用文本搜索、整体调用链任务是否更准确，属于步骤 10 的效果验收。

## Acorn 真实后端验证

新增默认忽略的 `real_project_agent_tools_probe`，通过实际 ToolRegistry 执行工具。在 `/private/tmp/oaklet-acorn-lsp-spike` 既有临时副本上使用已有 Rust/TS 后端，不改 `/Users/linear/Workspace/Acorn` 原始工程，不要求 Tauri 界面或模型服务。

| 查询目标 | 文件符号 | 项目搜索候选 | 定义 | 引用（排除声明） | incoming 调用者 |
| --- | ---: | ---: | ---: | ---: | ---: |
| Rust `format_cny` | 16 | 2 | 1 | 11 | 5 |
| TS `formatFieldValue` | 17 | 1 | 1 | 1 | 3 |

另验 TS outgoing 返回 1 个被调用者，incoming/outgoing 均通过真实工具入口执行。探针耗时 7.57 秒，包含启动与关闭，不作为冷启动性能指标。所有返回均为实时来源且完整性未知。TS 引用仅返回 1 项，即使随后调用层次返回 3 个调用者，也不意味着引用结果完整；不使用固定等待、连续相同或非空结果推断语义就绪，也不缓存这些语义结果。项目搜索的 Rust 同名候选分别保留。

记录位于 `/private/tmp/oaklet-acorn-index-tools.txt`。探针核对临时副本源码 hash 未变、没有新增遗留语言服务器进程。此为工具到真实后端的集成验证，不代表模型已完成端到端调用链任务。
