# Oaklet Windows 兼容方案

Oaklet 继续支持 **macOS Apple Silicon 和 Intel**，并新增 **Windows 11 x64、本地 NTFS 工作区和 NSIS 用户级安装包**。三个目标共用同一套 Tauri 2、React、Rust 和 SQLite 代码。两端均支持打开工作区、发送消息、恢复会话、编辑文件、查看 Git 差异、使用 `@worktree` 和终端。LikeC4 预览及 `likec4_mcp` 沿用现有的 Node.js 运行时要求。

Windows 首版使用 GitHub Releases 分发。Windows 10、ARM64、网络共享工作区、MSI 和离线安装属于后续支持选项，各自需要独立验收。

## 1. 建立 Windows 原生构建和测试

- 在现有测试工作流增加 `windows-2025` Rust 任务，使用 `x86_64-pc-windows-msvc`、Microsoft C++ Build Tools 的桌面 C++ 工作负载、Windows SDK、Edge WebView2 和 Git for Windows。保留现有 macOS 检查。CI 使用满足 Architecture helper 要求的 Node.js 22 版本（至少 `22.23.3`）和仓库指定的 pnpm 版本。 [测试工作流](../.github/workflows/test.yml) · [GitHub Windows 镜像](https://github.com/actions/runner-images/blob/main/images/windows/Windows2025-Readme.md) · [Tauri Windows 构建要求](https://v2.tauri.app/start/prerequisites/#windows)
- 在 Windows 同时跑桌面前端和 Architecture helper 的测试、构建。检查 Windows 默认执行环境下的 pnpm 脚本、Tauri 构建钩子和测试夹具。把通用测试中的 Unix 路径及命令替换为可移植等价项，为系统行为补充 Windows 用例。 [桌面脚本](../packages/desktop/package.json) · [Helper 脚本和 Node.js 要求](../packages/architecture-helper/package.json) · [Tauri 构建配置](../packages/core/tauri.conf.json)

在 Windows 开发机的 PowerShell 中从仓库根目录运行以下命令。先构建 helper，Rust 构建会读取它的产物。

```powershell
pnpm install --frozen-lockfile
pnpm --dir packages/desktop install --frozen-lockfile
pnpm --dir packages/architecture-helper install --frozen-lockfile
pnpm --dir packages/architecture-helper build
pnpm --dir packages/architecture-helper test
pnpm --dir packages/desktop test
pnpm --dir packages/desktop build

Push-Location packages/core
cargo test
cargo clippy --all-targets
Pop-Location

pnpm tauri dev
```

阶段完成条件是 Windows CI 通过，原生应用能从资源管理器选择工作区，完成一次对话，并在重启后从历史恢复。

## 2. 补齐命令和终端的进程管理

- 前台 `run_command` 和后台任务已经共用 `shell_command`，Windows 分支使用 `cmd`。继续复用这个入口，在工具描述中告知模型实际 shell。验证工作目录、引号、中文输出、退出码和输出截断，同时保持现有审批、取消和超时语义。MCP 的结构化 `command` 和 `args` 继续作为结构化参数处理，按实际需要验证 `.exe` 和常见 `.cmd` 启动器的行为。 [命令工具](../packages/core/src/tools/shell.rs) · [模型环境说明](../packages/core/src/agent/messages.rs) · [MCP 启动](../packages/core/src/tools/mcp.rs)
- macOS 沿用现有 Unix 进程组清理。Windows 前台命令和后台任务各自使用一个 Job Object，创建进程时就完成关联，避免父进程在关联前启动子进程。停止、超时或所有者退出时终止整个任务并等待回收。后台任务在 shell 已退出但子进程仍运行时保持运行状态。Git、Architecture 和 stdio MCP 的启动点也要检查关闭行为，对会产生后代进程的入口复用相同的清理机制。 [后台任务与收尾](../packages/core/src/tools/jobs.rs) · [应用退出](../packages/core/src/shutdown.rs) · [Windows Job Objects](https://learn.microsoft.com/en-us/windows/win32/procthread/job-objects) · [进程创建时关联 Job Object](https://learn.microsoft.com/en-us/windows/win32/api/processthreadsapi/nf-processthreadsapi-updateprocthreadattribute)
- 终端沿用 `portable-pty` 的 Windows ConPTY 后端。通过窗口关闭和终端标签关闭验证交互 shell 以及后代进程的生命周期。若现有 PTY 清理无法收回后代，在 Windows PTY 启动路径补齐。 [终端会话](../packages/core/src/terminal.rs)

自动化回归测试使用可观察退出状态的子进程夹具，覆盖父 shell 先退出、后代持有输出管道、后代重定向输出三种情况。手工验收再覆盖终端 `Ctrl+C`、缩放、粘贴、中文输出，以及两个窗口中关闭一个窗口只停止其拥有的任务。

## 3. 打通工作区路径、保存和 Git

- Rust 内部使用规范化后的 `PathBuf` 做工作区身份和边界检查。对 UI、IPC 和 Git 分别按现有接口提供路径表示，重点检查 Windows `\\?\` 前缀、盘符、分隔符和常见大小写别名。文件树、快速打开、会话文件链接和 Git diff 使用同一个工作区相对路径约定。Worktree 归属校验通过规范化路径检查。用目录联接、符号链接及文件名别名验证出界和 `.git` 写入保护。 [工作区路径校验](../packages/core/src/tools/workspace.rs) · [窗口与工作区](../packages/core/src/windows.rs) · [文件模型](../packages/desktop/src/features/files/file-model.ts) · [Rust `canonicalize`](https://doc.rust-lang.org/std/fs/fn.canonicalize.html)
- 使用临时文件发布保存结果，保留编辑器的外部改动检查和工具的排他创建语义。先在 NTFS 验证现有 `std::fs::rename` 的覆盖行为，以及只读、文件被占用、当前用户权限和失败后的临时文件清理。根据测试结果决定需要专门处理的 Windows 分支。 [编辑器保存](../packages/core/src/files.rs) · [文件工具写入](../packages/core/src/tools/fs.rs) · [Rust `rename`](https://doc.rust-lang.org/std/fs/fn.rename.html)
- 编辑器已有纯 CRLF 文件的换行保留逻辑。`apply_patch` 仍有把 CRLF 文件写成 LF 的测试。补丁匹配可按 LF 处理，发布内容时保留原来纯 CRLF 文件的换行风格，并测试末尾有无换行的情况。用 `core.autocrlf` 开关两种配置比较 Git diff，确认单行修改只显示预期的内容差异。 [文件模型](../packages/desktop/src/features/files/file-model.ts) · [补丁 CRLF 测试](../packages/core/src/tools/patch/tests.rs)
- 用 Git for Windows 验证文件变更、未跟踪文件、重命名、分支切换和历史 diff。`@worktree` 另外覆盖创建、打开、恢复及归档清理。现有 worktree 命令使用 `core.hooksPath=/dev/null`，用带标记副作用的本地测试 hook 确认 Windows 上也会跳过 hook，再按验证结果决定是否调整。 [Git 集成](../packages/core/src/git.rs) · [Worktree 命令](../packages/core/src/worktrees.rs) · [Git `core.hooksPath`](https://git-scm.com/docs/git-config#Documentation/git-config.txt-corehooksPath)

阶段完成条件是在含中文、空格和多级子目录的本地仓库里，用户能在文件树、编辑器、聊天和 Git diff 之间定位同一文件。同一目录经不同路径打开时复用同一窗口。工作区边界和 `.git` 写入保护在 Windows 上通过回归测试。

## 4. 验收桌面交互、可选依赖和本地数据

- 为 Windows 接入 `Ctrl+N` 新建会话或终端、`Ctrl+R` 打开历史、`Ctrl+F` 聊天搜索和 `Ctrl+W` 关闭当前会话，并更新快捷键提示。检查 `Ctrl+X` 剪切、`Ctrl+C` 复制、聊天停止和终端中断在各自焦点下的行为。macOS 继续使用现有 `⌘` 快捷键和 `⌘Q` 原生退出确认。WebView2 手工验收包括中文输入法确认候选词、粘贴附件、文件拖放、`--workspace`、单实例窗口聚焦和标题栏关闭确认。 [快捷键](../packages/desktop/src/app/shortcuts.ts) · [聊天搜索](../packages/desktop/src/features/chat/ChatFind.tsx) · [单实例入口](../packages/core/src/lib.rs)
- 从安装版检查 Architecture helper 能从 Tauri 的资源目录加载，验证普通 Node.js 安装、`PATH`、`OAKLET_NODE` 和不满足版本要求时的提示。使用配置好的 stdio 和 Streamable HTTP MCP 各执行一次调用，验证错误提示和窗口关闭后的清理。用无 Node.js 或 Git 的普通用户环境确认基础应用能启动，对需要这些依赖的功能显示可操作的提示。 [Architecture 运行时](../packages/core/src/architecture/runtime.rs) · [LikeC4 工具](../packages/core/src/tools/likec4.rs) · [MCP 实现](../packages/core/src/tools/mcp.rs)
- 配置、日志、`oaklet.db` 和附件继续使用 Tauri 的每用户目录。验证普通用户首次启动、保存设置、恢复历史及覆盖升级，并检查含 API key 的配置文件获得预期访问权限。SQLite 初始化和升级走现有迁移器。若实施中确需 schema 变更，按仓库规则新增下一号 SQL 迁移，并在 `packages/core` 运行 `cargo test` 和 `cargo clippy --all-targets`。升级验收使用前一测试安装包生成的真实数据，覆盖会话、附件和 worktree 引用。 [应用启动与目录](../packages/core/src/app.rs) · [配置写入](../packages/core/src/config.rs) · [数据库迁移规则](../AGENTS.md)

## 5. 打包、分发和回退

先让 Windows 构建任务在手动触发时上传内部测试包。复用现有 `.ico` 和 helper 资源映射，首版选择 NSIS 用户级安装和 WebView2 默认下载模式。构建命令在完成依赖安装和 helper 构建后，从仓库根目录执行。 [现有打包配置](../packages/core/tauri.conf.json) · [Tauri Windows 安装包](https://v2.tauri.app/distribute/windows-installer/)

```powershell
pnpm tauri build --ci --target x86_64-pc-windows-msvc --bundles nsis
```

安装包位于 `packages/core/target/x86_64-pc-windows-msvc/release/bundle/nsis/`。公开发布前在 CI 验证 Windows 应用和安装包的 Authenticode 签名，并在干净的 Windows 11 虚拟机上以普通用户完成安装、启动、升级和卸载。保留一次无开发工具的测试，覆盖 WebView2 和应用资源的加载。签名后的实际下载体验也要在目标机器上验证。 [Tauri Windows 签名](https://v2.tauri.app/distribute/sign/windows/) · [PowerShell 签名验证](https://learn.microsoft.com/en-us/powershell/module/microsoft.powershell.security/get-authenticodesignature)

macOS 沿用现有 `v*` 标签发布流程，继续提供 Apple Silicon 和 Intel 的 `.dmg` 和 `.app.zip`。Windows 构建和签名由独立任务执行，首版在 Windows 验收通过后，把签名的 NSIS 安装包追加到同一标签对应的 GitHub Release。Windows 构建或验收失败时，macOS 仍按原流程发布。README 在现有 macOS 安装说明旁增加 Windows 入口，CONTRIBUTING 补充 Windows 构建与测试命令。 [现有发布工作流](../.github/workflows/macos-release.yml) · [README](../README.md) · [CONTRIBUTING](../CONTRIBUTING.md)

如果测试包出现进程残留、工作区越界、覆盖升级丢数据或签名验证失败，暂停 Windows 安装包发布。已发布版本出现阻断问题时，先撤下受影响的 Windows 下载并保留用户数据，修复后用新版本发布。只有经过已有数据兼容验证时，才提供回退到前一安装包的步骤。

## 发布验收表

| 环境 | 必须通过的检查 |
| --- | --- |
| Windows CI | Rust 测试和 Clippy，前端及 helper 测试和构建，NSIS 产物生成，签名验证。 |
| Windows 11 干净虚拟机 | 普通用户安装、启动、升级、卸载。检查 WebView2、会话恢复、Windows 快捷键、中文输入法和窗口关闭。 |
| Windows 11 实际开发环境 | Git for Windows、中文和空格路径、CRLF 补丁、`@worktree`、终端以及前后台进程清理。启用 Node.js 后检查 Architecture 和 MCP。 |
| macOS CI 和回归测试 | Apple Silicon 和 Intel 的安装包均能构建并启动。覆盖原有 `⌘` 快捷键、终端与进程清理、LF 和 CRLF 文件编辑、Git 与 worktree，以及从既有版本升级后恢复设置和会话。 |
