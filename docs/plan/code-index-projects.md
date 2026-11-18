# 代码索引步骤 2：项目发现、配置与文件归属

日期：2026-11-18
状态：已实施并在 Acorn 验证。当前提供 Rust Service 的基础接口和检查示例，尚未在打开窗口时自动调用或启动长期 LSP 会话。

## 实现

- `code_index::discover(&Path)`：异步发现 Rust 与 TS/JS 项目；遍历在 spawn_blocking 中执行，遵守 ignore 规则，不跟随目录符号链接，排除 node_modules、target 等产物目录。上限 50,000 项、30 秒，扫描失败或截断明确返回 incomplete 和 warnings。
- Rust 使用 `cargo locate-project --workspace --message-format=json --manifest-path ...` 确认归属，单次最多 5 秒且计入发现总时限。成员去重至 workspace 根，独立嵌套 crate 使用自身根；解析失败保留错误边界，不回退到父工程。根超出所选工作区时拒绝。
- TS/JS 识别 package.json、tsconfig.json、jsconfig.json；同包多份 tsconfig 共用项目，嵌套包使用自己的根。目标文件按语言和最深所属根路由，不要求用户工程采用 Tauri。
- `Discovery::route(&str)`：接受工作区内相对或绝对路径；只路由已发现且仍存在的受支持文件。忽略文件、未发现文件、被删除文件、无清单的孤立文件及越界路径均返回错误。
- `detect_server(&Project, &CodeIndexConfig)`：显式 command 优先，否则查 PATH；区分 disabled、缺少可执行文件、缺少运行环境、缺少 TypeScript SDK 和项目根未确认。仅执行有超时和输出上限的版本探测，不初始化语言服务器。
- 版本探测在项目目录运行，禁用 rustup 自动安装和 Cargo 联网；保留 rustup 代理的原始程序名。SDK 检测优先项目依赖，再查服务器安装目录，可识别包管理器符号链接。
- 复用现有有界进程输出读取函数，并将其错误文字改为适用于各子进程的描述，未改变其大小限制或错误处理。

当前可查询扩展名为 rs、ts、tsx、mts、cts、js、jsx、mjs、cjs。Python 和 Go 留待步骤 11；混合语言目录可以同时存在 Rust 与 TS 项目。

## 配置

Rust AppConfig 与前端类型新增 codeIndex，旧配置缺省为 `{ "servers": {} }`。SettingsPanel 已通过展开原配置保留该字段，暂不新增设置页签。配置文件中的示例字段如下：

```json
{
  "codeIndex": {
    "servers": {
      "rust": {
        "enabled": true,
        "command": "/Users/linear/.cargo/bin/rust-analyzer",
        "args": []
      },
      "typescript": {
        "enabled": true,
        "command": "/private/tmp/oaklet-lsp-spike-tools/node_modules/.bin/typescript-language-server",
        "args": ["--stdio"]
      }
    }
  }
}
```

路径按本机调整；不要用这段局部示例覆盖完整 config.json。command 未设置时使用默认命令及默认参数，显式设置时 args 为原样参数数组；自定义 args 必须同时提供 command。保存时校验空命令和 NUL，参数中的空格及分号保持字面值，不经过 shell。

TypeScript 后端需要 Node 与可用 SDK；自定义 Node 启动方式可使用 command 指向 node、args 指定服务器脚本和 --stdio。检测结果 available 仅表示路径与版本探测成功，不等于 LSP 已初始化或索引已完成。

## Acorn 实测

直接对 `/Users/linear/Workspace/Acorn` 只读扫描与探测；不修改它的工程清单、锁文件或源码，不构建工程或安装依赖。

| 检查 | 结果 |
| --- | --- |
| 项目列表 | Rust 根为 Acorn，TS 根为 Acorn/frontend，共两个项目 |
| 完整性 | incomplete=false，warnings 为空 |
| core 的 cost.rs 与 tui 的 app.rs | 均路由至 Acorn Rust workspace |
| frontend/src/lib/format.ts | 路由至 Acorn/frontend TS 项目 |
| Rust 后端 | rust-analyzer 1.99.1；rustc/Cargo 1.99.1 |
| TS 后端 | typescript-language-server 6.1.0；Node 26.13.0；项目 TypeScript 5.9.3 |

Rust 探测保留 `/Users/linear/.cargo/bin/rust-analyzer`，未错误解析成 rustup；SDK 解析为 Acorn/frontend/node_modules/typescript/lib。

新增 `examples/code_projects.rs` 可输出项目、路由和探测结果。从 packages/core 执行：

```sh
PATH="/private/tmp/oaklet-lsp-spike-tools/node_modules/.bin:$PATH" \
  cargo run --example code_projects --offline --locked -- /Users/linear/Workspace/Acorn \
  crates/acorn-core/src/agent/cost.rs crates/acorn-tui/src/app.rs frontend/src/lib/format.ts \
  > /private/tmp/oaklet-acorn-projects.json
```

临时 TS LSP 来自步骤 1。无该 PATH 项时应报告 TS 后端缺失；仍应发现项目，不自动安装。示例使用默认服务器配置，显式覆盖、禁用和异常环境由单元测试覆盖。

## 验证与后续边界

- 新增 9 项测试：旧配置与往返、命令验证、Cargo workspace 与嵌套工程、TS/JS 项目、忽略与扫描限制、无主及删除文件、损坏清单、越界根与符号链接、缺少后端/Node/SDK、显式参数、防 shell 插值、代理名、超时与输出上限。
- core `cargo test --offline --locked -q`：311 项通过；现有 HTTP 测试在允许回环端口的环境运行。
- `cargo clippy --all-targets --offline --locked -- -D warnings` 与 `cargo fmt --all -- --check`：通过。
- desktop `pnpm test`：103 项通过；`pnpm build` 和最终 `tsc --noEmit` 通过，构建保留已有的大 chunk 提示。

未实现步骤 3 的 CodeIndexManager/LanguageServerManager、自动索引触发、文件监听、SQLite 迁移、设置界面或安装器。本轮只实测 macOS；Windows 的 npm 启动入口与其他平台兼容性仍需后续验证。步骤 2 代码已提交为 5fe1fef，文档另行提交。
