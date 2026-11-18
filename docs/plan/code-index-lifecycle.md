# 工作区代码索引服务与服务器生命周期

日期：2026-11-18

[主计划](code-index-call-chain.md)第 3 步已实施。打开用户工程会后台发现项目并启动已安装的语言服务器；首次主窗口和后续工作区窗口均已接入。该阶段交付生命周期与 Rust 查询基础接口，已保存文件同步、持久化、Agent 工具和状态界面分别留在后续步骤。

## 实现范围

- `code_index/manager.rs`：共享 `CodeIndexManager`，拥有工作区发现结果、配置与 generation；提供 `open_workspace`、`status`、`query`、`retry`、`close_workspace`、`drain`、`shutdown`。重复打开幂等；发现可取消，关闭后迟到的发现或检测结果不能注册服务器；全局退出后拒绝重新打开。并发 drain 串行等待清理。
- `code_index/types.rs`：业务查询、符号、位置、调用、结果、错误、能力和状态类型。参数与结果不携带 ToolContext、MCP 消息或任意 LSP JSON；位置采用零基行与 UTF-16 列，结果标记语义完整性未知。
- `code_index/language_servers.rs`：唯一服务器实例表，键为规范工作区、项目根、语言和完整后端命令/版本指纹；并发首次启动合并；全局最多 8 个启动中或已启动实例。资源满时交互查询优先于后台启动，先回收没有查询的实例，保留正在执行的请求；同实例最多容纳 32 个查询，超出返回 Busy。
- 同一实例的启动和查询由异步锁顺序提交；协议读写、反向请求和 stderr 排空由独立任务运行，不受查询等待的锁阻塞。空闲 120 秒可回收，每 15 秒巡检；空闲回收后的项目保留 Idle 状态，下一次查询重新启动。崩溃不循环重启，由下一次查询或手动 retry 触发；retry 会重新检测此前缺少的可执行文件。
- `code_index/lsp.rs`：stdio 会话，初始化最长 60 秒，单次普通请求最长 30 秒；初始化能力、只读配置、进度与 stderr 有界处理。Ready 仅表示完成协议初始化，progress 不作为语义完整性证明。Rust 默认关闭 build scripts 和 proc-macro；设置离线 Cargo 和禁用 rustup 自动安装，不执行安装，也拒绝 `workspace/applyEdit`。
- 单个入站帧最多 8 MiB，头部最多 8 KiB；stderr 保留末尾 8 KiB，进度最多 64 个 token，每条消息最多 1024 个字符。分帧保护从穿刺示例提取为共享 `framing.rs`。
- 工作区取消信号与单次查询取消信号独立。`cleanup_window`、显式关闭工作区和真正退出均接入清理；聊天取消使用的 `tools::shutdown_workspace` 不负责索引。关闭依次尝试 shutdown/exit，各等待最多 2 秒后强制终止并 wait；Unix 复用现有 `ProcessGroupGuard`，清理语言服务器派生的进程组。

`query` 当前提供文档符号、定义、引用及一层上下游调用的类型化适配，协商 UTF-16；未声明能力或实际返回 MethodNotFound 均返回 Unsupported。文档同步尚未实施，所以该 API 只作 Service/协议验证基础，不能据此声称真实工程的保存更新闭环已经完成，也未向 Agent 注册工具。

## async-lsp 取消修补

步骤 1 已证实上游 0.2.4 的公开出站请求 API 不暴露请求 ID，丢弃 future 也不会发送取消或清理未响应记录。本阶段保留选定框架，在 `packages/core/vendor/async-lsp` 保存原始源码与许可证，并用 Cargo `[patch.crates-io]` 指向它，没有自行实现 JSON-RPC。

修补仅改变出站请求生命周期：共享 socket 分配 ID；未完成的请求 future 在 Drop 时按原消息队列提交取消；主循环先删除对应 pending sender，再发送该 ID 的 `$/cancelRequest`。已完成请求不发取消，迟到响应因找不到 pending ID 被忽略，不关闭其他请求的连接。修补说明见该目录的 `OAKLET-PATCH.md`，上游提供等价能力后可移除本地 patch。

框架内回归直接断言：连续 1000 次永不响应请求在取消后 pending 表均恢复为空；完成一个请求不取消另一个；迟到响应不恢复旧记录。生产请求超时和显式取消测试另验证线上通知和会话复用。取消通知不保证服务器立即停止计算。

## 验证

- core：`cargo test --offline --locked -q`，322 项通过，其中新增 11 项生命周期与请求超时回归。
- 穿刺协议：`cargo test --example lsp_spike --offline --locked -q`，3 项通过；原“未发送取消”的观测已更新为验证修补后的取消行为。
- 框架修补：`cargo test --manifest-path vendor/async-lsp/Cargo.toml --no-default-features --features tokio --lib`，4 项通过。独立清单首次执行需要解析依赖元数据；其本地 lock 与构建产物忽略，生产依赖仍由 core/Cargo.lock 固定。
- core：`cargo clippy --all-targets --offline --locked -- -D warnings`、`cargo fmt --all -- --check`，均通过。
- desktop：`pnpm test` 103 项通过，`pnpm build` 通过，保留既有大 chunk 提示。

假服务器覆盖：并发首次查询只启动一个进程、重复打开、多工作区与越界隔离、等待查询期间响应配置/进度/编辑请求、超时和取消通知、取消聊天工具资源后索引仍存活、迟到响应、能力缺失与实际方法未实现、崩溃后按需重启、缺少服务器后 retry、禁用后端、初始化期间关闭与关闭重开、单槽队列和交互优先、启动失败释放容量、空闲回收、强制退出及 Unix 派生进程回收。测试可直接调用 Rust Service，不运行界面、模型或 MCP，也不下载真实服务器。

## Acorn 实际启动验证

使用步骤 1 保留的 `/private/tmp/oaklet-acorn-lsp-spike` 副本，来源为 `/Users/linear/Workspace/Acorn` 当时的工作树；原始 Acorn 不作为写入目标。沿用已安装的 rust-analyzer 1.99.1、typescript-language-server 6.1.0、TypeScript 5.9.3 和 Acorn 的离线 Cargo 缓存。

```sh
cd packages/core
cargo build --example code_index_lifecycle --offline --locked
CARGO_HOME=/Users/linear/Workspace/Acorn/.cargo \
PATH="/private/tmp/oaklet-lsp-spike-tools/node_modules/.bin:$PATH" \
./target/debug/examples/code_index_lifecycle /private/tmp/oaklet-acorn-lsp-spike
```

`examples/code_index_lifecycle.rs` 使用正式 `CodeIndexManager`，依次打开、重复打开、关闭、重开及退出，输出三份状态。实际记录位于 `/private/tmp/oaklet-acorn-lifecycle.json`：两个项目分别对应 Rust workspace 根与 frontend；扫描无缺口或 warning，两种后端均报告符号、定义、引用和调用能力。重复打开的 generation 保持不变，重开后均增加。验证脚本比较执行前后的进程列表，确认退出后没有新增遗留的 rust-analyzer、typescript-language-server、tsserver 或 typingsInstaller 进程。该结果只证明初始化、能力声明和生命周期，不能替代语义查询或完整索引验收。

## 后续边界

文件监听、文档版本与 didOpen/didChange、非 UTF-16 编码转换、返回 URI 进一步校验在步骤 4；SQLite、缓存与 Agent 接入尚未实施。未运行 Tauri 图形界面的真实开关窗口操作；已核对接入位置并通过编译、调用测试和前端回归。Windows/Linux 的进程回收与运行环境兼容性还需目标平台实测，Unix 进程组回归当前在 macOS 通过。
