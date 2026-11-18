# Python、Go、Java 语言扩展

日期：2026-11-18

## 实现范围

复用现有 CodeIndexManager、LanguageServerManager、LSP 连接、文件同步、SQLite 符号索引及统一 Agent 工具。没有新增依赖、数据库迁移或安装入口。

| 语言 | 默认命令及参数 | 运行环境 | 项目归属 |
| --- | --- | --- | --- |
| Python | `pyright-langserver --stdio` | Node.js、Python | `pyrightconfig.json` / `pyproject.toml`；没有清单的源码归属所打开的工作区根 |
| Go | `gopls` | 本地 Go 工具链 | `go.work` 的显式成员共享实例，独立 `go.mod` 与未列入 workspace 的嵌套 module 使用自己的根 |
| Java | `jdtls` | JDK 21+；默认启动器还需 PATH 中的 Python 3 | `pom.xml`、Gradle 构建/设置文件；Gradle settings 下的子项目合并，普通 Java 源码回退工作区根 |

Python 同时识别 `.py` 和 `.pyi`。解释器优先项目 `.venv`、`venv`，再查 PATH 的 `python3` / `python`；将选定解释器通过 `python.pythonPath` 交给 Pyright。Pyright 自己解析项目配置；配置的虚拟环境、缺少依赖与动态导入可能限制结果，状态页明确提示这些缺口。

Go 使用 `go work edit -json` 解析已有清单，不自行复制 Go 语法；包括含空格的成员路径及所选工作区内部的兄弟目录。成员必须有已发现的 go.mod，越界、忽略或缺失成员导致明确错误。缺少 Go 或解析失败时保留错误边界，不静默启动一组猜测的成员实例。gopls 显式绑定本项目 go.work，独立 module 使用 `GOWORK=off`；探测和服务器都设置 `GOTOOLCHAIN=local`、`GOPROXY=off`，不自动下载工具链或依赖。

Java 优先检测 JAVA_HOME 的 JVM，再查 PATH。JDT LS 的 `-data` 由 Oaklet 为每个会话分配独立临时目录，正常关闭后删除，Unix 权限为 0700。默认关闭自动构建、构建配置更新及 Gradle 导入，Maven 使用离线模式；Gradle 依赖及生成源码的语义覆盖因此有限，页面显示该限制。异常终止可能留下系统临时目录，JDT LS 自有索引不承诺跨会话复用。

Pyright 与 JDT LS 不使用不兼容的通用 `--version`：分别读取安装包 package.json 和 JDT core 插件版本。自定义启动器无法定位元数据时显示版本未知，并将启动器大小/mtime 纳入环境指纹。gopls 与 Go 使用各自的 `version` 子命令；Java 的 `-version` 输出支持从 stderr 读取。

设置页已增加三种语言的启用、命令和参数字段。全局配置传播、配置文件变化重启、监听及缓存失效涵盖全部五种语言。符号、定义、引用、调用层次继续通过标准 LSP 能力协商；未支持调用层次返回 unsupported，不推断为空调用图。语义完整性保持未知，语义关系缓存仍关闭。

## 自动化验证

- core：`cargo test --offline --locked -q`，368 项通过；2 项既有真实后端探针及 3 项新增真实后端探针默认忽略。
- core：`cargo clippy --all-targets --offline --locked -- -D warnings`、`cargo fmt --all -- --check`。
- desktop：`pnpm test`，113 项通过；`pnpm build` 通过，保留既有大 bundle 提示。
- `git diff --check`。

新增检查覆盖：五语言混合项目、Python 嵌套配置和普通源码、Gradle 子项目归属、ignore、go.work 成员去重及兄弟目录、独立嵌套 module、Go 缺失和越界成员、配置往返、真实 CLI 参数形式、版本元数据、Python 虚拟环境、JDK 版本门槛、三语言的 languageId、四类查询、能力缺失、保存同步、JDT LS 数据目录清理、后端缺失隔离和仅修改 Python 配置时保留 Rust 会话。

完整测试需要 macOS FSEvent 和回环端口。首次沙箱运行受到权限限制，允许这些本地测试能力后通过；没有改写测试来规避文件监听检查。

## 真实后端验收待办

本机 PATH 中没有 Pyright、gopls、JDT LS 和 Go；未安装服务器，也未下载工具链。上面的协议测试使用假服务器，不证明真实后端的解析能力或延迟。尚未固定经验证的新增服务器版本，步骤 10 的 Agent 收益门槛亦未完成。

新增 `src/code_index/language_tests.rs` 提供三组隔离的双文件工程，断言跨文件定义、引用、保存新增调用后的引用增长，并按协商能力检查 incoming/outgoing calls。探针输出实际服务器版本、能力和查询/更新耗时，退出时关闭服务器。安装好对应后端后，在 `packages/core` 中分别运行：

```sh
cargo test --offline --locked code_index::language_tests::real_pyright -- --ignored --nocapture
cargo test --offline --locked code_index::language_tests::real_gopls -- --ignored --nocapture
cargo test --offline --locked code_index::language_tests::real_jdtls -- --ignored --nocapture
```

真实后端四类查询、保存更新延迟、Windows/Linux 启动器及桌面点击验收仍待执行；本节不将这些项目标记为通过。

建议提交消息：`feat(code-index): add Python, Go and Java backends`。遵循现有 Conventional Commits 形式，尚未执行 git commit。
