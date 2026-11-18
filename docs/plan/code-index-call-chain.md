# Oaklet 代码索引与调用链感知计划

日期：2026-11-18  
状态：步骤 1 的 [LSP 穿刺](code-index-spike.md)、步骤 2 的 [项目发现及配置](code-index-projects.md) 与步骤 3 的 [工作区服务及服务器生命周期](code-index-lifecycle.md) 已完成，步骤 4–12 待实施。先交付 Rust/TS 用户工作区闭环并验证收益，再扩展语言和安装入口。

## 目标与成功标准

为 Agent 提供代码级语义查询和跨边界证据，减少修改任务中的漏查与漏改。最终覆盖 Rust、TypeScript/JavaScript、Python、Go；首批验证 Rust、TS 和 Tauri IPC。

产品对象是用户用 Oaklet 打开的工程。用户打开工作区后，Oaklet 自动识别其中的语言和项目，为具备可用后端的项目启动后台索引；Agent 查询该工作区的索引。Oaklet 自身仅可作为测试样本，不能成为唯一支持的工程。

语言服务器负责语言内的定义、引用与可用的调用关系；Oaklet 补充 Tauri IPC 的调用点、命令注册和 Rust 实现证据。结果允许不完整，但必须显式表达缺口。

主要验收标准是同一模型、同一任务下，Agent 是否更可靠地找到并处理预先标注的受影响位置。耗时、查询次数和 token 消耗作为辅助指标。

## 已有基础与接入方式

- `packages/core/src/tools/mod.rs` 已统一工具 schema、guidance、执行、超时、取消和输出裁剪，新工具沿用该接口。
- 现有 glob、grep、read_file 作为候选搜索、源码核对与降级路径。LikeC4 保留系统架构查询职责。
- Rust core 基于 async-lsp 增加独立 LSP 客户端，通过 stdio 管理语言服务器。协议分帧与请求关联交给框架；MCP 与 LSP 的消息封装不同，不直接复用 MCP 传输实现。
- rust-analyzer 是用户 Rust 工程的候选索引后端，由 Oaklet 在运行时检测和启动，不要求 Oaklet 仓库已有 rust-analyzer 集成，也不把它加入 Oaklet 的 Cargo 依赖。其他语言采用同样的工作区后端机制。
- 语言服务器管理自身语义索引；Oaklet 在现有 SQLite 中持久化文件、符号、候选证据和按需查询缓存，新增编号迁移，不引入独立图数据库。聊天界面复用通用工具状态展示。

### 调用边界：内置 Agent 直接调用 Rust Service

当前消费者仅为 Tauri 内置 Agent。CodeIndexManager 本身就是进程内 Rust Service，不再叠加一层同职责的 Service wrapper，也不启动索引 MCP 服务。

- Agent 路径：现有 Tool 执行管线 → tools/code.rs 参数适配 → CodeIndexManager 的 Rust 方法 → 语言后端。
- 界面路径：Tauri command → 按窗口校验工作区 → 同一个 CodeIndexManager；Tauri event 只负责通知界面状态变化。
- 后端路径：CodeIndexManager → LanguageServerManager → async-lsp 客户端 → 语言服务器 stdio。两个 manager 均为进程内 Rust 对象；LSP 是后端适配协议，语言服务器依然是独立进程。

项目发现、查询路由、文件同步、候选证据、状态和错误语义都属于 Service。Tool 层只负责 schema、模型参数校验、guidance 和结果呈现；Tauri command 只负责窗口上下文校验及调用，不复制业务逻辑。

Service 使用明确的 Rust 查询参数、结果和错误类型，不接收 ToolContext、MCP 请求或任意 LSP 方法名，不返回模型专用 ToolOutput。lsp-types 和协议 JSON 限定在 LSP 适配层；Service 结果中的位置、能力缺失、截断与候选证据不依赖某种通信格式。调用方将已有取消信号传给 Service，无需为此额外引入通用传输抽象。

未来明确需要外部 Agent 时，在相同 Service 外增加 MCP 适配层，映射工具参数、工作区授权与结果即可；不迁移或复制索引逻辑。外部访问的生命周期与授权到时按实际需求设计，当前不预建 MCP 进程、端口或可插拔通信框架。

该结构省去 Agent 到 Service 的额外进程及协议往返；不会消除语言服务器的 LSP 通信，也不会改变用户工程本身可能存在的 Tauri IPC 调用。

## 技术选型（本方案采用）

### 框架与依赖

| 层次 | 采用技术 | Oaklet 负责的部分 |
| --- | --- | --- |
| 桌面宿主与状态接口 | 现有 Tauri 2、Rust、React 19、TypeScript | 工作区生命周期、状态展示、设置与安装入口 |
| LSP 客户端框架 | async-lsp 0.2.4，关闭默认 features，启用 tokio | 基于 MainLoop::new_client 和 Router 注册客户端处理器；通过 ServerSocket 发查询 |
| 协议类型 | lsp-types 0.95 系列 | 与 async-lsp 的类型依赖保持一致，不单独升级到另一不兼容版本 |
| 异步运行与子进程 | 现有 Tokio 1；新增 tokio-util 0.7 的 compat feature | 启停进程、超时、任务取消；转换 Tokio 子进程管道供框架读写 |
| 文件监听 | notify 8.2 的 RecommendedWatcher | 工作区级监听、事件合并、ignore 过滤和错误恢复 |
| 项目发现与候选搜索 | 现有 ignore 0.4、globset 0.4、regex 1、serde_json 1 | 遍历项目清单、候选证据搜索及配置解析 |
| 语义索引引擎 | rust-analyzer、typescript-language-server + TypeScript；随后 Pyright、gopls | 按语言配置启动和能力映射，不自行实现类型解析与符号绑定 |
| 索引 Service | AppState 中的 Arc<CodeIndexManager> | 工作区、项目、监听、查询路由与候选证据，协调持久化与缓存 |
| 语言服务器管理 | CodeIndexManager 内持有的 LanguageServerManager | 服务器实例池、启动去重、并发与资源上限、重试及关闭；不新增进程 |
| 持久化索引与缓存 | 现有 rusqlite 0.42 + bundled SQLite，复用 oaklet.db 的 WAL、迁移和 Store | 文件清单、符号、IPC 候选、查询结果及覆盖率；普通 B-tree 索引，不新增存储框架 |
| 内容与缓存指纹 | 新增 blake3 1.x；内存复用标准 HashMap | 内容寻址、项目快照与缓存键；内存缓存有界，不引入 Redis 或缓存服务 |

上述依赖已在步骤 1 加入，并通过 Cargo.lock 锁定实际版本。步骤 3 为 async-lsp 0.2.4 增加随仓维护的取消修补，见 [实现与验证记录](code-index-lifecycle.md)。语言服务器的发行版本单独通过集成用例验证后固定，不使用运行时 latest。

选择 async-lsp 是因为它支持客户端角色，能复用双向请求、响应与通知基础设施。Oaklet 仍需实现项目配置、初始化顺序、能力协商和文档同步。tower-lsp 面向语言服务器开发，不用于本方案的客户端；不自行重写通用 JSON-RPC 框架。

不引入 Tree-sitter、向量数据库、图数据库、SCIP 或独立 MCP 索引服务。首批需求是实时语言语义查询和少量框架证据；这些技术留到出现明确能力缺口再评估。Tauri IPC 首批采用有边界的候选搜索，不把正则匹配升级为已确认语义关系。

### 模块与内部接口

新增 packages/core/src/code_index/ 模块，保持以下直接分工：

| 模块 | 职责 |
| --- | --- |
| mod.rs / manager.rs / types.rs | 公共导出；CodeIndexManager、工作区打开关闭与查询路由；业务查询、状态和错误类型 |
| discovery.rs | 项目清单发现、项目与语言归属、排除规则及扫描上限 |
| language_servers.rs | LanguageServerManager、服务器实例注册、启动去重、资源调度、重试与回收 |
| lsp.rs | 单实例 LanguageServerSession：async-lsp 客户端、子进程、初始化、能力、文档与位置编码转换 |
| watch.rs | notify 监听、事件合并、文件与项目配置更新 |
| ipc.rs | Tauri invoke、generate_handler! 与 command 实现的候选证据 |
| cache.rs | 内存查询缓存、指纹与失效判定、读写持久缓存的编排；不再包装一个通用 CacheManager |

SQL 访问集中在现有 store 的 code_index.rs 子模块，通过 Store 的类型化方法提供；不在 LSP 客户端或 Tool 实现中拼接数据库访问。code_index 模块持有 Store 克隆，沿用现有共享连接及短事务方式。

新增 tools/code.rs 作为现有 Tool trait 的进程内适配层，不在 agent 循环按工具名称分发。CodeIndexManager 提供 open_workspace、close_workspace、shutdown、status、retry 和 query；query 的内部操作枚举对应符号、定义、引用、上下游调用及 IPC 查询，返回 Rust 结果类型，不提供任意 LSP 方法执行入口。

LanguageServerManager 必须实现，不能把 async-lsp 视为服务器管理器。async-lsp 提供协议通信基础设施，用户工作区的进程池、启动去重和生命周期由 Oaklet 管理：

- CodeIndexManager 拥有发现结果、文件监听和项目路由，决定某次查询属于哪个项目；它不再另存一份进程池或文档版本。
- LanguageServerManager 是 CodeIndexManager 持有的具体内部组件，不建立第二个全局 Service 或通用管理器 trait。其内部通过共享句柄供后台任务使用，实例表按“工作区 + 项目根 + 后端配置”唯一索引。
- LanguageServerManager 提供 ensure_started、query、sync_files、status、restart、close_workspace 和 shutdown；参数采用项目键、已解析的服务器配置及 Rust 查询类型，不暴露原始 LSP JSON。
- ensure_started 必须合并并发启动请求，不能重复 spawn；同一实例的同步与查询按序提交，stdout 读取、服务器请求响应及进程退出监听独立运行，不被等待查询的锁阻塞。
- LanguageServerSession 是实例表中一个服务器连接的具体状态，持有 Child、协议循环任务、能力、文档版本与实例 generation；进程退出和重启后旧句柄不能继续路由查询。
- 资源上限、等待队列、空闲回收、崩溃与重试由 LanguageServerManager 唯一管理；工作区层仅聚合这些状态，不再实现第二套重启逻辑。
- notify 事件先由 CodeIndexManager 过滤并定位受影响项目，再交 LanguageServerManager 做文档同步；Tauri IPC 候选查询可直接使用搜索，不要求语言服务器先启动。

- open_workspace 幂等地创建工作区任务并立即返回，扫描在 spawn_blocking 中执行；close_workspace 先标记关闭并取消任务，再停止相关进程。
- query 接收调用方从 ToolContext 或窗口绑定取得的 Workspace、Rust 查询参数和本次调用的 Cancel，不依赖 ToolContext 本身。取消单次查询不关闭工作区服务器。
- 每次工作区打开或重建携带 generation，迟到的发现结果、服务器通知和重试结果不得重新注册到已关闭或已替换的工作区。
- with_host_tools 从 AppState 取得共享 manager，克隆句柄注入 code 工具；保留现有 ToolContext 结构，避免把索引句柄传播到所有文件工具及其测试。
- 不将索引清理挂到 tools::shutdown_workspace：当前 agent 初始化取消也会调用它。仅在真正的工作区清理与应用退出路径释放索引。

### 持久化索引与缓存方案

#### 存储范围与职责

采用“SQLite 持久层 + 有界内存查询缓存 + 语言服务器自身索引”三层结构。持久层属于 CodeIndexManager；LanguageServerManager 只持有活动会话和文档状态，不负责 SQLite，也不被反序列化恢复为已初始化状态。

| 数据 | 生成方式 | 重启后的用途 |
| --- | --- | --- |
| 项目与文件清单、内容 hash | 初次扫描及 notify 增量更新 | 核对新增、修改、删除及项目归属，调度增量重建 |
| 符号名称、种类、容器与范围 | 后台逐文件 documentSymbol；查询时优先处理目标文件 | 无需重新询问服务器即可搜索已验证文件的符号 |
| Tauri IPC 候选证据 | 对变化文件重新提取已有候选规则 | 恢复有来源位置、内容 hash 与候选身份的跨边界证据 |
| 定义、引用与上下游调用查询结果 | 用户或 Agent 实际查询成功后记录 | 在有效性检查通过时复用已查询过的关系 |
| 覆盖率与版本 | 每文件提交及项目快照记录 | 区分未索引、部分覆盖、待核对和已验证；不将局部结果说成全仓完整 |

后台符号提取优先于预计算调用图：按项目依次处理发现的受支持文件，使用文档符号能力并在无需保留时关闭临时文档，避免把全仓文件永久 didOpen。能力缺失、单文件超限或资源不足时记录跳过原因及覆盖率，不无限重试。索引任务让位给交互查询。

不预计算全部函数间的引用与调用关系。引用和调用的持久化粒度为一次有来源和快照信息的查询结果，保留结构化节点、调用点和截断标记；它们不构成完整全仓图。LSP 服务自有数据库由各服务器管理，Oaklet 不复制其内部格式，也不假设四种服务器均能跨重启恢复语义索引。

#### SQLite 数据模型与迁移

在 oaklet.db 新增以下表，全部使用 code_index_ 前缀，按规范工作区路径与项目键隔离：

| 表 | 最小职责与关键索引 |
| --- | --- |
| code_index_projects | 工作区、项目根、语言、后端/工具链指纹、格式版本、项目 revision、快照 hash 和覆盖状态；工作区与项目键唯一 |
| code_index_files | 项目键、相对路径、大小、mtime、BLAKE3 内容 hash、已提取 hash 与状态；项目与路径唯一 |
| code_index_symbols | 文件键、名称、可搜索名称、种类、容器、范围；文件键索引及项目/名称前缀 B-tree 索引 |
| code_index_evidence | 文件键、命令名、证据类别、范围、候选信息与提取版本；按项目/命令名查询 |
| code_index_queries | 规范查询键、项目快照与环境指纹、结果 JSON、生成时间及最近访问时间；按查询键查找、按访问时间回收 |

只存索引元数据、结构化关系与必要证据，不保存整份源码；证据片段在返回时从当前文件读取并核对 hash。JSON 是 Service 结果的存储格式，不存 MCP/ToolOutput 或原始 LSP 消息。初版使用前缀搜索和现有 LSP workspaceSymbol；不引入 FTS5、向量检索或独立图查询引擎。

当前只有 V0__baseline.sql，实施时新增 V1__code_index.sql；若实施前已有其他迁移，取下一个编号。所有建表与索引 SQL 放在 packages/core/migrations/，由既有 build.rs 嵌入和迁移 runner 执行；不改 V0，不在 Rust 字符串中建表，不替换 runner。

setup 中统一 Store::open 一次，向 ChatState 和 CodeIndexManager 注入 Store 克隆；为 ChatState 增加从已有 Store 初始化的入口，保留原有 stale run 恢复行为并更新调用和测试。不要为索引再创建独立迁移机制或运行时绕过 Store 打开第二套数据库。

数据库操作在 spawn_blocking 中执行；源码读取、hash 与 LSP 请求均在数据库锁外完成。每个文件的 hash、符号和证据在一个短事务内替换，query 结果写入前再次核对 generation/revision。写盘失败不能撤销已经成功的实时查询；报告缓存不可用并退回实时路径。

#### 启动恢复与增量更新

1. 工作区打开时立即读入上次项目摘要，状态为待核对，不能直接将旧结果标记为最新。启动 watcher 后进行文件清单与内容 hash 核对，期间变更纳入 revision。
2. mtime/size 仅用于调度优化，不作为跨重启复用语义结果的充分依据。一次启动核对受支持项目的文件内容，结合相对路径、配置清单和锁文件生成稳定项目快照；大仓库后台分批进行，不阻塞窗口。
3. 已验证且内容未变的文件复用符号和候选；新增或变化文件安排提取，删除文件清除相关索引行。后端版本、工具链或提取规则变化时重新调度受影响数据。
4. 项目快照核对完成前不复用跨文件语义查询缓存；扫描不完整、监听故障或环境指纹不可确认时继续走实时查询，并展示缓存验证不完整。
5. notify 源码变化使项目 revision 立即增加，并在返回结果前核对；删除、重命名、ignore、项目配置、锁文件与分支切换同样参与失效。关闭工作区保留磁盘索引，丢弃活动会话与内存缓存。

项目 revision 是运行中并发失效标记，项目快照 hash 用于跨重启核对。二者不得替代服务器内部索引就绪状态。扫描核对、后台符号提取和 LSP 分析进度分别记录，不能统一展示为一个虚构的“完整索引完成”。

#### 缓存键、失效和查询顺序

规范缓存键包含工作区、项目、语言、查询种类、目标路径与位置或查询文本、方向、limit 等有效选项，以及项目快照、后端版本/配置/工具链指纹和结果格式版本；用 BLAKE3 生成键。绝对路径与工作区内等价相对路径归一化。

- CodeIndexManager 按“验证后的内存命中 → SQLite 命中 → LanguageServerManager 实时查询”的顺序读取；符号和 IPC 候选可直接查询持久索引。调用方仍能看到 coverage 与 source=live/index/cache。
- 项目内任一有效源码或配置变化，首版保守失效该项目全部跨文件查询缓存；只增量重建变更文件的符号和证据，不尝试猜测完整依赖图。
- 引用、调用和定义查询只缓存成功的非空结果，最多复用 10 分钟；失败、未就绪、取消、能力缺失与空结果不落盘。Acorn 穿刺已发现 TS 启动初期可先返回成功非空的部分引用，因此还必须确认后端就绪及项目快照。没有经验证的就绪依据时不跨查询持久缓存语义结果，只返回带完整性未知标记的实时结果；固定等待或连续相同结果不能代替就绪依据。TTL 不能替代上述条件。
- 同一次查询开始和结束时核对 revision/generation；期间变化的结果不缓存，并标记可能过期。同一实例内序列化目标文档同步与查询，避免读取旧版本缓存后再同步文件。
- 不将服务器重新初始化后的结果默认视为完整；文档版本和能力就绪之外的语义完整性仍由服务器决定。无法确认环境指纹、外部依赖状态或缓存版本时走实时查询，不假装命中。
- 过期数据不作为默认成功结果返回。缺少服务器时，仅提供已核对文件的符号和候选证据；跨文件引用/调用缓存无法证明有效时返回 unavailable 或 not_ready，而非返回无标记旧结果。

内存缓存先使用 HashMap 与访问序号实现简单有界淘汰，全局最多 256 条且结果负载合计不超过 16 MiB，不再加缓存依赖。SQLite 派生数据以逻辑负载 256 MiB 为软上限，优先删除过期查询，再淘汰最近未使用的关闭工作区索引；覆盖不足处标记为待重建。最多 30 天未访问的关闭工作区数据可回收。只清理 code_index_ 表，不操作聊天或迁移记录；SQLite 文件大小不保证随删行立即缩小，后台不执行整库 VACUUM。

在 Code Index 页签提供“清除当前工程索引并重建”：先使 generation/revision 失效，清理当前工程的派生行和内存缓存，再重建；不删除 oaklet.db 或用户工程文件。记录解码失败只丢弃对应派生记录；整库损坏或迁移错误继续遵循原有错误处理，不自动重置包含聊天历史的数据库。

### LSP 初始化与查询流程

1. 用 tokio::process::Command 启动服务器，cwd 与 initialize 的 rootUri/workspaceFolders 指向识别出的用户项目。以参数数组启动，不经过 shell。
2. 将 stdout/stdin 通过 tokio-util compat 接入 async-lsp MainLoop；持续有界读取 stderr。进程句柄与主循环任务由项目实例持有。
3. 完成 initialize → initialized，记录 ServerCapabilities 和协商的位置编码；处理 workspace/configuration、能力注册、进度与日志。服务器请求应用编辑时明确拒绝，不让查询工具修改工程。
4. 查询前为目标文件发送 didOpen 或 didChange，同步版本与文本；尊重服务器声明的全文或增量同步方式。位置转换和返回结果归一化在 lsp.rs 完成。
5. code_symbols 映射 documentSymbol/workspaceSymbol，code_definition 映射 definition，code_references 映射 references；code_calls 先 prepareCallHierarchy，再查 incomingCalls/outgoingCalls。未声明或实际不支持的方法返回 unsupported。
6. 关闭时发送 shutdown/exit 并等待短暂退出；超时终止并回收进程。请求超时后停止等待、丢弃迟到结果，并验证框架的请求取消行为，不宣称服务器已经停止计算。

### 文件更新与前端接口

- 每工作区持有一个 notify watcher，事件通过有界通道交给后台任务，按 200ms 窗口合并；过滤忽略目录、临时写入文件和工作区外目标。
- 已打开文档变化后重新读取已保存内容，更新文档版本；向注册了对应文件监听能力的服务器发送 didChangeWatchedFiles。项目清单变化触发重新发现与受影响实例重建。
- 原子保存的 rename/create/delete 统一按最终文件存在状态处理。监听报错或通道溢出时标记状态可能过期并触发后台重新核对；不在每次查询前扫描全仓。
- 新增 code_index_status 和 code_index_retry Tauri commands，始终通过 workspace_for_window 取工作区；状态通过 emit_to 向所属窗口发送 code-index://changed。前端先订阅再取快照，快照和事件携带 revision，忽略旧版本。
- Rust AppConfig 与前端 AppConfig 同步新增 codeIndex.servers；按语言保存 enabled、可选 command 和 args，缺省使用内置检测。配置改变只重建受影响语言实例，旧配置文件通过默认值兼容。
- 设置页新增 Code Index 页签与项目状态列表，不新增侧栏页面或调用图。后续安装、安装进度和安装后重试沿用同一页签。

### 技术依据

- [async-lsp 0.2.4：客户端与服务器支持、依赖及职责](https://docs.rs/crate/async-lsp/0.2.4)
- [MainLoop::new_client](https://docs.rs/async-lsp/latest/async_lsp/struct.MainLoop.html)
- [ServerSocket：客户端查询与通知](https://docs.rs/async-lsp/latest/async_lsp/struct.ServerSocket.html)
- [tokio-util compat：Tokio 与 futures I/O 适配](https://docs.rs/tokio-util/latest/tokio_util/compat/)
- [notify RecommendedWatcher](https://docs.rs/notify/latest/notify/type.RecommendedWatcher.html)

## 用户工程的索引生命周期

1. 用户打开或恢复工作区时，通过现有工作区生命周期入口异步发现项目清单与语言，遵守 ignore 规则；不阻塞窗口打开，不等到 Agent 第一次调用工具才开始发现。
2. 为已识别项目检测相应服务器和运行环境。可用时排队启动并初始化，以用户项目根和配置加载项目；不以 Oaklet 安装目录或源码目录作为索引根。
3. 语言服务器管理自身语义索引，Oaklet 提供统一查询入口。不同服务器可能采用后台或按需分析，不把初始化成功等同于全仓索引完成，也不强制逐个打开全部源码文件。
4. 缺少后端时显示待安装或环境缺失，提供安装入口，仍允许普通文件搜索和聊天。安装成功后自动重新启动该工作区对应的索引任务，无需重开工程。
5. 已保存文件、项目清单和配置改变时增量更新对应项目及持久层，并失效相关查询缓存。关闭工作区时取消排队任务并释放实例但保留磁盘索引；重新打开后核对恢复，语言服务器另行初始化，不承诺服务器秒开或完整调用图恢复。

工作区提供索引状态入口，逐项目显示发现中、等待资源、启动中、可查询、缺少环境或失败。服务器提供索引进度时显示进度，否则不编造百分比或“完整索引完成”。可查询只代表服务器能接受查询，不代表所有调用关系均已收录。

多工作区状态、进程与结果必须隔离；当前会话只能查询其所属工作区。资源达到上限时后台任务等待，查询明确提示未就绪，不影响用户继续编辑。

## 模型工具接口

### 语言内语义查询

| 工具 | 输入 | 输出 |
| --- | --- | --- |
| `code_symbols` | path，可选 query | 文件符号；提供 query 时搜索 path 所属项目的符号 |
| `code_definition` | 文件、行、列 | 定义位置，允许多个候选 |
| `code_references` | 文件、行、列 | 引用位置，默认排除定义自身 |
| `code_calls` | 文件、行、列、incoming/outgoing | 一层调用者或被调用者，附调用点 |

- 行列从 1 开始，列按 Unicode 字符计数；内部转换为服务器协商的位置编码。
- 返回工作区相对路径、范围、符号名称与证据来源。工作区外的位置仅报告为外部目标，不自动读取。
- 默认最多 50 项，可提高至 200；明确标记截断，不把被裁剪的输出当作完整结果。
- 区分 unavailable、unsupported、not_ready、timeout 和成功空结果。仅在有依据时报告索引状态，不推断服务器已完成全仓索引。
- 同名或重载候选分别返回。首批只返回一层调用关系，由 Agent 按需继续展开。
- 不支持调用层次时，明确报告；引用和 grep 匹配不得冒充已确认调用关系。

### Tauri IPC 证据查询

新增 `code_ipc`，输入为命令名或调用点文件、行、列，两种入口互斥。首批只适配 Tauri。

- 关联前端 invoke 调用、generate_handler! 注册项和 Rust command 实现。
- 每项证据包含位置、源码片段和证据类别，分别列出候选关系及缺失的注册或实现证据。
- 从调用点查询时，支持静态字符串命令名；动态表达式明确返回无法静态确定，并保留调用点证据。
- 处理同名候选时不擅自选定目标。别名、命令重命名和自定义封装无法解析时标记缺口。
- 搜索限定工作区，沿用 ignore 规则和路径边界检查。使用现有搜索设施，不用正则结果宣称语义确定性。
- 首批不自动连接 HTTP、事件总线或任意跨语言协议。

工具 guidance 要求：修改前阅读相关源码；候选关系需要核对；空结果或索引未就绪不能证明没有影响。工具输出是工作区数据，不是指令。

## 分步工作与依赖顺序

依赖顺序：1 → 2 → 3 → 4 → 5 → 6 → 7 → 8 → 9 → 10。步骤 10 达到收益门槛后再做 11 → 12。每步包含其测试，不把必要测试推迟到最后；完成标准满足后才能勾选。

第一阶段交付步骤 1–10：已有服务器环境下，用户打开外部 Rust/TS 工程即可使用，包含持久化及缓存。第二阶段完成步骤 11 的 Python/Go 支持。第三阶段完成步骤 12 的安装体验。工作区生命周期、持久化、缓存、配置和状态入口属于第一阶段，不能等到多语言扩展时才补。

### 1. 接入框架并建立独立验证样本

- [x] 在 core 引入 async-lsp 0.2.4、兼容的 lsp-types 0.95、tokio-util compat、notify 8.2 和 blake3 1.x，更新锁文件；数据库变更安排在步骤 5。
- [x] 按用户指定，采用 Acorn Rust/TS 工程的临时源码副本验证，不依赖 Oaklet 源码目录结构。Acorn 不含 Tauri，最小 Tauri 样本延后步骤 9，不将本次结果当作 IPC 验证。
- [x] 实现测试用假 LSP 服务器，用实际 stdio 运行框架客户端，验证 initialize、客户端响应服务器请求、通知与查询响应交错。
- [x] 实测超时、迟到响应与异常消息：框架不自动发送出站取消，记录为步骤 3 必须解决的限制；消息大小无框架配置，已在流边界增加有测试的上限保护。
- [x] 记录实际 rust-analyzer、typescript-language-server、TypeScript、Rust 和 Node 版本，以及定义、引用、符号和调用层次结果；发现并验证 TS 客户端能力声明和启动期部分引用问题。

完成标准：依赖编译成功，假服务器测试通过，真实 Rust/TS 样本至少能完成定义和引用查询；明确区分不支持的方法与空结果。该步骤只需测试级客户端，不提前交付界面或安装器。

### 2. 项目发现、最小配置与查询归属

- [x] 实现 discovery.rs：采用 ignore 遍历项目清单，排除构建产物与依赖目录，不跟随工作区外符号链接；扫描上限 50,000 项、30 秒，超限保留已发现项目并标明发现不完整。
- [x] 首批识别 Cargo workspace、独立 crate、tsconfig/jsconfig 和 package.json 项目。Cargo workspace 成员归属同一项目；TS 首版同一包内多份 tsconfig 共用项目，长期服务器实例留到步骤 3。
- [x] Rust workspace 根通过已有 Cargo 的 locate-project --workspace --message-format=json 确认；失败时保留带错误的路由边界，不静默重复创建成员项目。
- [x] 实现目标文件到项目的路由；嵌套独立项目使用自身根，多语言项目按文件语言选择后端。忽略、缺失、删除、越界或无主文件返回明确错误。
- [x] Rust/前端 AppConfig 同步新增 codeIndex.servers 的 enabled、command、args 及默认值，接入保存校验；旧配置默认值与往返测试通过，现有 SettingsPanel 保留新字段。
- [x] 实现显式配置与 PATH 检测，在用户项目目录探测版本。缺少可执行文件、Node、TypeScript SDK 或 Rust 工具链时返回原因；禁用安装与联网，第三阶段再加入管理安装目录。

完成标准：独立 Rust/TS、Cargo workspace、混合语言、嵌套项目与 ignore 用例路由正确；旧配置无需迁移可加载。整个发现与检测过程不构建工程或安装依赖。

### 3. 工作区索引管理与服务器生命周期

- [x] 新增 CodeIndexManager 并由 AppState 持有 Arc，内部持有 LanguageServerManager；更新生产初始化及 architecture、agent 等测试中手工构造 AppState 的位置。
- [x] 将 Service 查询参数、结果与错误定义为 Rust 类型；LSP 结果在 lsp.rs 转换；tools/code.rs 的模型输出转换留在步骤 7。直接测试 Service，不要求运行 Tauri 界面、模型服务或 MCP。
- [x] 在 setup 的主窗口注册之后，以及 open_workspace_window 成功创建新窗口后调用幂等的 open_workspace；扫描放入 spawn_blocking，服务器初始化放入异步任务。
- [x] 实现 language_servers.rs 中 LanguageServerManager 的实例表、ensure_started 启动去重与调度；按工作区、项目根和后端配置复用实例，generation 防止旧实例和已关闭任务的结果重新进入状态。
- [x] LanguageServerManager 实现全局最多 8 个实例与空闲回收；等待项目排队，查询到等待实例时优先安排它，不终止正在查询的实例，不在 CodeIndexManager 重复持有进程池。
- [x] 在 lsp.rs 实现单实例 LanguageServerSession，用 async-lsp 接入子进程，完成 initialize/initialized、能力记录、配置与进度处理；初始化超时 60 秒，普通请求超时 30 秒，持续有界读取 stderr。
- [x] 解决步骤 1 暴露的出站请求 ID、$/cancelRequest 和未响应记录回收问题，验证取消单个请求不关闭服务器、不影响其他调用。不能直接把示例的 tokio timeout 当作生产取消实现；这一项未通过不得进入 Agent 工具集成。
- [x] rust-analyzer 默认禁用 build scripts 与 proc-macro 执行；不自动修改工程配置、安装项目依赖或接受 workspace/applyEdit。
- [x] cleanup_window 和真正的应用退出调用 CodeIndexManager 清理工作区任务，再委托 LanguageServerManager 完成 shutdown/exit、超时终止与进程回收；不要接到聊天取消也会使用的 tools::shutdown_workspace。
- [x] LanguageServerManager 实现崩溃、重试与状态来源，CodeIndexManager 聚合为项目状态；崩溃使当前查询失败，下一次查询或手动重试可重新启动，禁止无限自动重启循环。
- [x] 用假服务器验证同项目并发启动只产生一个进程、查询等待时仍能响应服务器请求、不同工作区隔离、重启丢弃旧响应、资源耗尽排队与退出释放。

实现与验证：[工作区服务及服务器生命周期](code-index-lifecycle.md)。此阶段尚不进行 didOpen/didChange 或 Agent 工具接入，文件同步在步骤 4 实施。

完成标准：打开外部工程自动启动后台任务；重复打开不重复创建实例；关闭期间迟到任务无法复活；停止聊天后服务器仍存活；关闭工作区或退出后无遗留服务器进程。

### 4. 已保存文件同步与实时更新

- [ ] 实现 watch.rs，每工作区一个 RecommendedWatcher，使用有界事件通道与 200ms 合并窗口；将项目发现和监听启用期间发生的变更纳入核对，避免启动时丢更新。
- [ ] 查询前读取目标文件的磁盘内容，通过 LanguageServerManager 定位会话，由 LanguageServerSession 维护文档版本并发送 didOpen/didChange；兼容协商的全文或增量同步方式，删除时发送 didClose 并清理缓存。
- [ ] 监听编辑器保存、Agent write/edit/patch、终端、Git 和外部编辑器的文件变化。原子保存、移动与删除按最终磁盘状态归一化，不要求逐个改写文件写入函数。
- [ ] 处理服务器注册和注销的文件监听规则，发送匹配的 didChangeWatchedFiles；清单、tsconfig 和 ignore 变化触发相关项目重新发现或重新初始化。
- [ ] 监听错误或队列溢出时标记可能过期，安排后台核对并支持重试；不在每次查询前全仓扫描，也不把文件通知送达视为服务器完成重新索引。
- [ ] 校验输入路径和返回 URI，拒绝越界读取；支持 Unicode、CRLF 与服务器协商的位置编码转换。

完成标准：预先查询过的符号及其跨文件引用在保存、删除和重命名后更新；未保存编辑不进入结果；外部变更无需重开工程即可反映。真实服务器记录更新延迟，超时或不确定状态不返回为成功空集。

### 5. SQLite 持久化与重启恢复

- [ ] 按上述五类表新增下一编号 SQL 迁移，复用现有 runner；更新空库初始化、升级与 checksum 测试，不修改历史迁移。
- [ ] setup 统一打开 Store，向 ChatState 与 CodeIndexManager 注入克隆；保持聊天中断恢复逻辑，更新构造调用与测试。
- [ ] 实现 store/code_index.rs 的类型化读写、按文件短事务替换、按工程清理与覆盖率记录；所有文件读取、hash 与 LSP 等待在数据库锁外。
- [ ] 实现文件内容和稳定项目快照 BLAKE3，打开工程时恢复摘要并后台核对，监听变更纳入 revision；未完成核对的数据不冒充最新结果。
- [ ] 调度后台逐文件符号及 IPC 候选提取，交互查询优先，未支持或跳过文件记录原因；能力尚未实现的证据类别保持未覆盖，步骤 9 接入后自动调度。
- [ ] 验证退出重开复用未变文件、仅重建变化文件、删除清理、事务中断、派生行解码失败及多工作区隔离；同时验证聊天表数据不变。

完成标准：索引确实写入 SQLite，重启后能恢复并核对；未变文件不重复提取符号，变化和删除不留下被误用的旧位置。迁移升级与 checksum 校验通过，数据库操作不持锁等待 LSP。

### 6. 查询缓存、失效与重建

- [ ] 实现 cache.rs、规范查询键、结果格式版本、项目 revision/快照和后端环境指纹；使用有界内存缓存与 SQLite 持久缓存。
- [ ] 按验证后的内存 → SQLite → 实时查询顺序读取，结果带 source/coverage；仅允许具备经验证后端就绪依据的非空成功语义结果缓存最多 10 分钟，否则绕过语义缓存，保留完整性与截断标记。
- [ ] 变更、重命名、删除、锁文件、配置和分支切换保守失效项目跨文件查询缓存；请求结束再核对 revision/generation，迟到结果不得回填旧缓存。
- [ ] 实现过期清理、负载上限、关闭工程数据回收和仅重建当前工程的 Service 方法；不清理聊天或整个数据库，缓存写失败不阻断实时结果。
- [ ] 覆盖冷热命中、跨重启、同 mtime/size 但内容变化、服务器/工具链升级、查询期间变更、监听溢出、负缓存禁止及部分覆盖。

完成标准：同一有效查询可以跨会话和重启复用，文件或环境变化后不再命中旧结果；缓存不可用时实时查询仍可执行，清除重建不影响用户文件和聊天历史。

### 7. 向 Agent 暴露四个语义工具

- [ ] 实现 tools/code.rs，在 with_host_tools 中注入共享 manager，使用 ToolContext.workspace 和本次调用 Cancel；不改 agent 的按名称分发或全部 ToolContext 构造。
- [ ] Tool execute 直接调用 manager 的 Rust 方法，不经 Tauri invoke、MCP 或本地 HTTP；Tool 层不重新实现项目路由、索引更新及证据关联。
- [ ] 实现 code_symbols、code_definition、code_references、code_calls 的 schema、参数验证、摘要和 guidance，按前文映射到标准 LSP 方法。
- [ ] 统一结果位置、来源、能力缺失与索引状态；默认 50 项、最高 200 项，显式标记截断。外部定义只报告目标，不自动读取源码。
- [ ] 测试同名候选、递归、多个 prepareCallHierarchy 结果、无支持能力、超时和取消。取消一个查询不得取消其他会话的查询或整个工作区索引。
- [ ] 验证现有工具输出裁剪与 spill 仍可分页恢复，guidance 正确进入模型请求；语义工具的状态不通过当前统一标为 MCP warning 的 warnings 列表传递。

完成标准：Agent 在独立工程中能定位定义、引用和服务器支持的一层调用关系；缺少后端时能继续使用 grep/read_file，且不会把候选或能力缺失当作无调用关系。

### 8. 工作区状态、设置和手动重试

- [ ] 新增 code_index_status、code_index_retry commands，并注册到 Tauri handler；通过 workspace_for_window 取根，不接受前端指定任意工程。
- [ ] 新增 code_index_rebuild command 调用步骤 6 的当前工程重建方法；展示磁盘恢复、核对、符号覆盖率、缓存状态和清除重建入口，不将缓存命中误报为实时分析。
- [ ] 实现带 revision 的状态快照和 code-index://changed 窗口事件；前端先订阅再取快照，忽略旧 revision，卸载时释放订阅。
- [ ] 设置页新增 Code Index 页签，显示项目根、语言、服务器版本、可用能力、等待或错误原因；支持步骤 2 的命令配置、禁用与重试。
- [ ] 配置保存后仅重建受影响的语言实例；一个窗口更新全局配置时，所有打开工作区按各自项目重建并保持状态隔离。
- [ ] 测试缺少后端、等待资源、启动失败、配置错误、重试成功和窗口销毁；没有服务器进度时不展示虚构百分比。

完成标准：用户无需发送聊天即可看到打开工程后的索引状态；配置服务器或重试后状态自动更新，其他工程不会收到本工程的索引事件。此时尚无安装按钮，明确显示服务器需自行安装。

### 9. Tauri IPC 候选证据工具

- [ ] 实现 ipc.rs 和 code_ipc，接受命令名或调用点，使用现有遍历与候选搜索能力关联静态 invoke、generate_handler! 注册项和 command 实现。
- [ ] 仅在用户工程中发现 Tauri 相关证据时启用专门关联；其他工程返回不适用，不要求用户项目采用 Tauri。
- [ ] 返回各端源码位置、片段与证据缺口；同名实现保留多个候选，注释、普通同名字符串和无法确认的宏上下文不得标成确定注册关系。
- [ ] 无法解析动态命令、别名、重命名或自定义封装时明确标记，允许 Agent 回到通用搜索核对。候选结果始终保留候选身份，即使三种证据都已找到。
- [ ] 用独立 Tauri 样本覆盖完整证据、注册缺失、同名、多行代码与动态命令；Oaklet files_save 只作为额外回归。
- [ ] 接入按文件持久提取及查询恢复，变更时清理旧证据；读取证据时核对内容 hash 并从当前源码生成片段。

完成标准：Agent 能取得跨 TS/Rust 边界的可核对证据，且不会将正则匹配表述为已解析的完整调用链。先完成该能力再进行收益对照，不先扩展更多框架。

### 10. 第一阶段集成验收与收益门槛

- [ ] 选定独立外部 Tauri 工程中的真实修改任务，人工提前列出必须发现和修改的调用点、注册处、实现及关联测试，固定完成条件。
- [ ] 在隔离副本中分别使用现有工具与新增工具，保持模型、任务、初始代码、上下文和运行预算一致；两组各运行 3 次，不把一组输出放入另一组上下文。
- [ ] 每次记录必需位置的发现率、漏改数、错误改动、测试结果、耗时、查询次数和可获取的 token 用量；无法获取的指标写明缺失。
- [ ] 验收打开工程到查询、文件更新、混合项目、多窗口隔离、关闭重开与关闭聊天不关闭索引的完整流程。
- [ ] 分别记录冷启动、恢复核对、符号复用及查询缓存命中表现；Agent 对照实验两组使用相同冷热条件，另测修改后失效，不用缓存优势掩盖错误旧结果。
- [ ] 运行 core 的 cargo test、cargo clippy --all-targets，以及 desktop 的 pnpm test、pnpm build，修复此次变更引入的失败。
- [ ] 将服务器能力、版本、实际索引更新延迟和对照结果写入本计划同目录的验证记录，不将待验证项标记为已通过。

完成标准：技术用例全部通过；新增工具组的总漏改数低于基线，且没有由错误链路关联造成的新错误改动。基线本来零漏改或结果无改善时，不宣称门槛通过，换有代表性的任务或调整接口/guidance 后复测。通过后才进入步骤 11、12。

### 11. 扩展 Python 与 Go

- [ ] 按同一项目实例机制接入 Pyright、gopls，新增相应运行环境检测和独立工程夹具，不复制 manager 或传输层。
- [ ] Python 识别 pyrightconfig、pyproject，随后工作区根；返回配置和解释器环境导致的解析限制，不代替用户安装 Python 依赖。
- [ ] Go 识别 go.work、go.mod，处理 workspace 和嵌套 module 的归属；不为同一 Go workspace 重复创建实例。
- [ ] 分别记录四种查询的实际能力、版本与更新行为，按支持范围验收；不强求不同服务器都实现调用层次。
- [ ] 验证四语言混合仓库、服务器缺失和失败隔离，执行受影响测试与集成检查。

完成标准：Python/Go 项目在打开时自动识别，定义与引用用例通过；调用层次支持情况可见；一个后端故障不阻止其他语言查询。

### 12. 用户触发的安装与最终交付

- [ ] 在现有 Code Index 页签增加安装、进度和失败重试；新增 code_index_install command，由后端维护允许的服务器及固定版本清单，不接受模型发起安装或任意下载地址。
- [ ] TS/JS 与 Pyright 使用已有 npm 安装固定包版本到 Oaklet 数据目录；gopls 使用已有 Go 和专用 GOBIN；不修改用户工程依赖或全局 npm 包。
- [ ] rust-analyzer 使用已有 rustup 为用户项目选定的工具链安装组件，操作前在安装入口说明它会更改该工具链的组件，不声称该组件位于 Oaklet 私有目录。不安装或切换 Rust 工具链本身。
- [ ] 缺少 Node/npm、Go 或 rustup 时展示明确说明；根据实际目标平台验证 npm 可执行入口，命令使用参数数组，不构造 shell 安装字符串。
- [ ] 按服务器和版本串行安装，失败不发布半成品，保留原可用版本；安装后用版本检测和初始化探测确认可用，再重试受影响工作区。
- [ ] 补齐显式配置 → Oaklet 管理版本 → PATH 的选择顺序；rust-analyzer 按项目工具链检测。退出时停止安装任务，并确保下次安装可恢复或清理中间状态。
- [ ] 验证缺少运行环境、断网、安装中取消、重复点击、安装失败、安装成功后自动启动及多窗口共享安装结果。完成四语言回归与全量检查。
- [ ] 更新用户使用说明、各平台运行环境要求和已验证的服务器版本；生成按逻辑独立变更拆分的提交说明，未经用户明确授权不执行 git commit。

完成标准：用户打开自己的工程后，能看到缺少的后端，点击安装并在成功后直接查询，无需重开工程或手动运行索引命令；失败时现有聊天、搜索和其他后端继续可用。

## 工作包与提交边界

步骤编号用于实施和验收，不要求一项对应一个巨大提交。按“依赖与框架测试、项目配置与发现、生命周期、文件同步、持久化与迁移、缓存失效、Agent 工具、状态界面、IPC、验证记录、语言适配、安装”保持逻辑独立；必要的调用方、类型和测试随同一变化提交。所有实际提交都须另有用户明确授权。

## 测试与交付检查

- 协议测试：使用假服务器覆盖消息分帧、通知交错、编码转换、能力缺失、超时、取消和进程崩溃。
- 语义测试：固定语言项目覆盖定义、跨文件引用、同名符号、递归及服务器实际支持的上下游调用。
- IPC 测试：静态命令、注册缺失、多候选和动态命令，确保候选不被标记为已确认关系。
- 工作区体验：打开外部工程后自动发现并启动索引，缺少服务器时提示安装，安装后自动重试，关闭并重新打开后恢复初始化；整个过程不要求用户手动运行索引命令。
- 同步与隔离：文件修改、删除、重命名、混合语言及嵌套项目、多工作区隔离、工作区关闭和配置变化；确认不会查询到 Oaklet 源码或另一用户工程。
- 边界与安装：路径越界、符号链接、缺少运行环境、网络失败、重复安装及配置覆盖；普通单元测试不下载真实服务器。
- 持久化与缓存：迁移、重启恢复、内容 hash、跨文件失效、TTL、版本变化、事务回滚、覆盖率、资源回收、清除重建及聊天数据隔离。
- 实际变更完成后运行 core 的 cargo test、cargo clippy --all-targets，以及 desktop 的测试和构建。

## 限制与待验证项

- 首批不包含调用图界面、独立全仓图数据库、未保存缓冲区、运行时追踪或其他框架的专门适配。
- 当前不支持外部 Agent 接入，不建设索引 MCP 服务；后续通过同一 Rust Service 的薄适配层提供，避免协议进入业务逻辑。
- 客户端框架、文件监听与模块接口按本文选型实施；语言服务器能力、发行版本、安装平台兼容性、同步兼容行为与真实修改任务，仍需阶段一验证，不能视为已经完成的保证。
- 动态分派、反射、宏生成代码及框架封装可能漏检；候选关系和空结果必须保持可核对、可解释。
- 步骤 1 代码与文档已分别提交为 7f4a5d6、2eafa99；步骤 2 的项目发现、配置与运行环境检测代码已提交为 5fe1fef，文档另行提交。步骤 3 的工作区服务、LanguageServerManager 与请求取消修补已提交为 dd3de0d；步骤 4–12 待实施。
