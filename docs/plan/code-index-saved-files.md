# 已保存文件同步与实时更新

日期：2026-11-18

[主计划](code-index-call-chain.md)第 4 步已实施。查询和文件监听均使用第 3 步的共享 Rust Service 与服务器池，不增加 MCP、进程池或前端写入钩子。本阶段没有数据库迁移、持久化索引、跨文件查询缓存或 Agent 工具注册。

## 实现

- `watch.rs`：每工作区一个 `notify::RecommendedWatcher`，先启用监听再发现项目，保留扫描期间的事件；256 条有界通道、200ms 固定合并窗口。忽略访问事件、既有排除目录及常见编辑器临时文件；原子保存、移动和删除按最终磁盘状态归一化。创建、删除或文件清单不一致时重新发现；普通保存只更新变更文件，不在查询前全仓扫描。
- 原生 watcher 的创建、销毁在 blocking 线程执行，打开窗口不等待扫描或 FSEvents 初始化。工作区关闭会取消后台任务，`drain` 等待任务及 watcher 回收；关闭重开后的旧任务不能覆盖新 generation。
- 复用 `globset` 处理 `client/registerCapability` 与 `client/unregisterCapability` 中的文件监听规则，支持字符串 glob、RelativePattern 的 URI / WorkspaceFolder base、创建／修改／删除掩码及注销。按当前工作区注册规则监听额外的非源码文件；同工作区共享目录不因服务器项目根不同被提前丢弃。扫描清单保留未忽略的普通文件，查询路由仍仅接受支持的源码语言。注册数、规则数和模式长度有上限，未知动态能力明确拒绝。
- `LanguageServerSession` 保留已同步文本及单调递增版本。查询前读取目标的已保存 UTF-8 文件，首次 `didOpen`、内容变化后 `didChange`；全文模式发送全文，增量模式发送替换旧文档完整范围的合法增量编辑。按声明处理 open/close、旧版数值同步能力和 didSave。删除或被忽略的文档发送 `didClose` 并移除会话文本；服务器重建后重新同步。每文件最多 8 MiB，每会话最多 64 份文档、合计 32 MiB，超限关闭并淘汰已有文档。
- 监听事件重新读取已经打开的文档，覆盖编辑器、Agent、终端、Git 和外部编辑器的保存；没有为每个写入入口增加专用通知。未查询的文件按服务器注册规则发送 `didChangeWatchedFiles`，服务器自身的文件监听继续有效。
- Cargo 清单／lock、package 清单／lock、tsconfig、jsconfig 和 ignore 变化触发重新发现，并重建相关语言和目录的实例；不重启没有受影响的独立 Rust 项目。普通源码保存、增加、移动和删除保留健康实例。文件属于哪个项目仍由发现结果确定，错误 Cargo 根继续作为路由屏障。
- watcher 错误、原生 rescan 信号或通道溢出会使工作区 `stale=true` 并记录原因。后台重新发现、重建会话并核对清单与已保存文本；初始化或同步失败后自动重新建立 watcher，再核对。恢复前查询返回 `NotReady`。`WorkspaceStatus` 增加 revision 和 stale，查询期间 revision 变化也返回 `NotReady`。
- `documents.rs` 有界读取、检查工作区 containment、拒绝目录／超大文件／无效 UTF-8。业务接口维持零基行、UTF-16 列，LSP 层支持协商 UTF-16／UTF-8／UTF-32，转换 Unicode 和 CRLF，拒绝拆分字符的位置、越界行列和倒序范围。返回位置必须是 file URI；外部依赖的 UTF-16 位置只作元数据，不读取工作区外源码。非 UTF-16 外部位置因无法安全转换而返回 `Unsupported`。

`Ready` 仍仅代表协议初始化。文件通知送达、progress 结束或单次查询成功都不证明语义索引完整。所有成功结果保留 `completeness_known=false`；当前 Service 对完整性未知的空符号／定义／引用／调用结果返回 `NotReady`，避免把“暂时没有结果”呈现为确定不存在。后续工具层需呈现不确定状态；缓存仍须经过步骤 6 的独立就绪判定。

## 自动验证

- core：`cargo test --offline --locked -q`，331 项通过，较步骤 3 增加 9 项回归。
- core：`cargo clippy --all-targets --offline --locked -- -D warnings`、`cargo fmt --all -- --check`。
- 穿刺协议：`cargo test --example lsp_spike --offline --locked -q`，3 项通过。

新增覆盖：真实原生 watcher 在没有后续查询时更新已打开文档；原子保存、移动、删除、新文件路由、ignore 变化；扫描期间创建文件；清单变化重建相关实例且保留独立实例；动态注册的非源码文件创建／修改／删除类型；全文同步及 UTF-8 返回位置归一化；UTF-32、emoji、CRLF、非法字符边界；越界 symlink、超大文件、无效 UTF-8、外部位置只读元数据；监听错误与队列溢出；核对前阻止查询、成功后清除 stale。既有取消、并发、关闭重开与进程回收回归继续通过。

macOS 原生 FSEvents 和已有本地回环网络测试需要脱离当前工具沙箱运行；沙箱内会报 `unable to start FSEvent stream`，该情形验证了 stale 状态，不能作为 watcher 成功验收。

## Acorn 真实服务器验收

沿用 `/private/tmp/oaklet-acorn-lsp-spike` 临时副本和此前安装的 rust-analyzer 1.99.1、typescript-language-server 6.1.0、TypeScript 5.9.3。原始 `/Users/linear/Workspace/Acorn` 未作为写入目标。

```sh
cd packages/core
cargo build --example code_index_saved_files --offline --locked
CARGO_HOME=/Users/linear/Workspace/Acorn/.cargo \
PATH="/private/tmp/oaklet-lsp-spike-tools/node_modules/.bin:$PATH" \
./target/debug/examples/code_index_saved_files /private/tmp/oaklet-acorn-lsp-spike
```

`examples/code_index_saved_files.rs` 只允许 OS 临时目录下的可丢弃 Acorn 副本，调用正式 Rust Service。先查询 `format_cny` 和 `formatFieldValue` 的跨文件引用并打开调用方文档；只在内存形成编辑时，磁盘结果不包含新调用；原子保存后出现新增调用；重命名后旧路径消失、新路径出现；删除后新路径消失。Rust 重命名同时调整模块声明以保留新文件的有效模块归属。每一项条件最多等待 120 秒，失败返回错误，不以空结果冒充通过。退出后恢复副本中原有文件并清理测试文件。

实际记录保存在 `/private/tmp/oaklet-acorn-saved-files.json`。验证脚本另核对修改前后的源文件 SHA-256，并比较进程列表，确认副本恢复且没有新增遗留的 rust-analyzer、typescript-language-server、tsserver 或 typingsInstaller。延迟是本机单次观测，包含合并窗口、服务器重新分析与轮询间隔，不作为 SLA；不代表完整调用图已经就绪。

最终一次记录的等待时间（ms）：

| 语言 | 保存后新增调用 | 重命名后旧路径消失 | 新路径出现 | 删除后新路径消失 |
| --- | ---: | ---: | ---: | ---: |
| Rust | 290 | 941 | 217 | 1192 |
| TypeScript | 1121 | 127 | 8 | 203 |

每列从对应断言开始计时；新路径断言在旧路径消失验证之后执行，Rust 还包含此前所述模块声明调整，因此这些数字不能直接比较为操作总耗时。

TypeScript 的首次非空引用集合仍可能只有 2 条，后续新增调用后可观察到 14 条；这个差异再次验证了“非空”不等于语义完整。保存更新验收检查的是已知新增／移动／删除调用位置，不能据此宣称所有引用均已找到。

## 边界

SQLite 索引、内容 hash、查询缓存、Agent 工具及状态界面按后续步骤实施；当前仅有随会话销毁的有界文档文本及工作区文件清单。未运行 Tauri GUI 手工保存流程，也未实测 Windows/Linux 原生监听；本轮不改前端，因此没有重复运行前端测试或构建。配置继承与服务器自身的异步分析仍可能延迟，查询必须保留完整性未知状态。
