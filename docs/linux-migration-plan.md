# Oaklet Linux 兼容方案

首版建议支持 **Ubuntu Desktop 24.04 LTS x64（`x86_64-unknown-linux-gnu`）、GNOME Wayland 和 Xorg、本地 ext4 工作区**，通过 GitHub Releases 提供 `.deb`。验收范围包括工作区、多窗口、聊天及历史、文件编辑、Git、`@worktree`、终端和 MCP。Architecture 预览与 `likec4_mcp` 继续要求 Node.js 22.23.3 或更高版本。其他发行版、ARM64、AppImage、RPM、Flatpak、Snap 和网络文件系统留待扩展目标时单独验收。 [Windows 迁移方案](windows-migration-plan.md) · [Tauri Debian 打包](https://v2.tauri.app/distribute/debian/)

当前前端和 Architecture helper 已在 Ubuntu CI 运行，Rust 原生检查和发布覆盖 macOS、Windows。Linux 可以复用现有 Unix 命令和进程组、PTY、SQLite 及非 macOS 的 `Ctrl` 快捷键。需要直接修正的是路径处理、第二次启动时相对路径的解析，以及 Linux 代理设置的说明。原生构建、WebKitGTK 桌面交互和安装包需要在目标系统验收。 [测试工作流](../.github/workflows/test.yml) · [进程组实现](../packages/core/src/process_tree.rs) · [终端实现](../packages/core/src/terminal.rs) · [快捷键](../packages/desktop/src/app/shortcuts.ts)

## 1. 建立 Linux 原生构建和测试

- 在 `test.yml` 增加固定 `ubuntu-24.04` 的 Rust 任务。安装编译器、`pkg-config`、WebKitGTK 4.1 和 OpenSSL 开发库。桌面前端和 helper 测试复用现有任务。pnpm 沿用根目录 `packageManager`，CI Node.js 沿用 `22.23.3`，Rust 使用 stable。先构建 helper，`tauri-build` 会在 `cargo test` 时复制它的产物。 [测试工作流](../.github/workflows/test.yml) · [Rust 依赖](../packages/core/Cargo.toml) · [Tauri Linux 构建依赖](https://v2.tauri.app/start/prerequisites/#linux) · [GitHub Actions runner](https://github.com/actions/runner-images)
- 在 Linux 原生任务跑现有 `cargo test` 和 `cargo clippy --all-targets`。检查 PTY 用例确实执行，`/dev/ptmx` 不可用而跳过用例时补跑。可以在 CI 加一次有 D-Bus 会话和虚拟显示的启动冒烟，验证应用窗口和基础 IPC。Wayland、Xorg、输入法及系统剪贴板由桌面验收覆盖。 [终端测试](../packages/core/src/terminal.rs) · [Tauri 单实例插件](https://v2.tauri.app/plugin/single-instance/)

在 Ubuntu 24.04 开发机装好 Rust、Node.js 和 pnpm 后，从仓库根目录执行：

```bash
sudo apt update
sudo apt install build-essential pkg-config git curl wget file \
  libwebkit2gtk-4.1-dev libssl-dev libxdo-dev \
  libayatana-appindicator3-dev librsvg2-dev

pnpm install --frozen-lockfile
pnpm --dir packages/desktop install --frozen-lockfile
pnpm --dir packages/architecture-helper install --frozen-lockfile
pnpm --dir packages/architecture-helper build
pnpm --dir packages/architecture-helper test
pnpm --dir packages/desktop test
pnpm --dir packages/desktop build
(cd packages/core && cargo test && cargo clippy --all-targets)
pnpm tauri dev
```

## 2. 修正文件路径和单实例启动

- **文件名与分隔符。** `file-model.ts` 和 `quick-open.ts` 同时按 `/` 和 `\` 拆分路径，Rust 搜索工具也会无条件把 `\` 换成 `/`。Linux 文件名可以包含反斜杠。把这种转换限制在实际接收 Windows 路径的入口，保留 Unix 文件名原值。添加包含 `a\b.txt` 和 `a/b.txt` 的回归夹具，确认文件树标记、快速打开、面包屑、复制路径、搜索结果和 Git diff 能区分并打开正确文件。Unix `OsStr` 到字符串 IPC 的转换也要检查。首版以 UTF-8 工作区和文件名为范围，非 UTF-8 条目应跳过或报错，避免用替代字符回传可操作路径。 [文件模型](../packages/desktop/src/features/files/file-model.ts) · [快速打开](../packages/desktop/src/features/files/quick-open.ts) · [Rust 搜索工具](../packages/core/src/tools/search.rs) · [文件 IPC](../packages/core/src/files.rs)
- **工作区与 Git。** 复用 Rust 路径规范化和 `.git` 写入保护。在 ext4 验证大小写不同的文件或目录保持独立，同一工作区的符号链接复用窗口，指向工作区外的链接无法通过文件工具写入。验证带中文和空格的路径、Git 重命名、未跟踪文件、历史 diff，以及 `@worktree` 创建、恢复和归档。编辑器和文件工具已有同目录临时文件发布逻辑，重点验收执行位、只读文件、外部编辑冲突、失败后的临时文件清理及 LF/CRLF 保留。 [工作区检查](../packages/core/src/tools/workspace.rs) · [Git 集成](../packages/core/src/git.rs) · [Worktree](../packages/core/src/worktrees.rs) · [编辑器保存](../packages/core/src/files.rs) · [文件工具](../packages/core/src/tools/fs.rs) · [补丁发布](../packages/core/src/tools/patch/commit.rs)
- **命令行与单实例。** Linux 插件通过会话 D-Bus 转发参数和第二次启动的工作目录，Oaklet 目前丢弃该目录。把回调的 `cwd` 传入现有入口，先用它解析 `--workspace`、`-w` 和位置参数中的相对路径，再规范化。测试从不同目录执行 `oaklet --workspace .`、绝对路径和无参数二次启动。Wayland 下同时测试工作区是否打开、已有窗口是否复用，以及桌面会话如何呈现窗口激活请求。 [单实例入口](../packages/core/src/lib.rs) · [参数解析与窗口处理](../packages/core/src/windows.rs) · [Tauri 单实例插件](https://v2.tauri.app/plugin/single-instance/)

## 3. 验收桌面、命令与可选依赖

- **WebKitGTK。** 在 GNOME Wayland 和 Xorg 从应用菜单启动安装版，验证工作区选择、文件拖放、截图粘贴、复制路径和提交哈希、外部链接、中文输入法候选确认、浅色和深色显示，以及终端缩放。检查 `Ctrl+N/R/F/W/S/P` 对应的新建、历史、查找、关闭、保存和快速打开，再验证 `Ctrl+C` 在复制、停止聊天和中断终端三个焦点下的行为。覆盖 `Alt+F4` 关闭确认和最后一窗退出。先复用现有 GTK 对话框及剪贴板入口，根据实际失败点修改。 [Tauri 能力配置](../packages/core/capabilities/default.json) · [快捷键与输入法](../packages/desktop/src/app/shortcuts.ts) · [聊天拖放](../packages/desktop/src/features/chat/Chat.tsx) · [退出流程](../packages/core/src/shutdown.rs) · [Tauri 对话框](https://v2.tauri.app/plugin/dialog/)
- **命令和终端。** 保持前后台工具使用 `sh -c`，在 Ubuntu 默认 `/bin/sh` 下验证命令夹具，显式需要 Bash 的命令使用 `bash`。交互终端沿用 `portable-pty` 的用户 shell。验证取消、超时、父 shell 先退出后仍有子进程、终端前后台作业和关闭多窗口中的单个窗口，检查各自拥有的进程是否回收。若 PTY 作业控制将子进程分到独立进程组并造成残留，从终端会话的关闭入口修复。也验收 Architecture 编译取消和 stdio MCP 在窗口或应用关闭后的清理。 [前台命令](../packages/core/src/tools/shell.rs) · [后台任务](../packages/core/src/tools/jobs.rs) · [终端](../packages/core/src/terminal.rs) · [MCP](../packages/core/src/tools/mcp.rs)
- **桌面启动环境。** 从应用菜单和终端分别启动，验证登录 shell 的 `PATH`、`OAKLET_NODE`、系统位置或用户的 Node 版本管理器，以及 `.deb` 内 helper 资源的定位。使用有效、缺失和版本过低的 Node，验证 Architecture 与 `likec4_mcp` 的提示。用最小运行环境检查缺少 Git 时的提示和基础聊天启动。配置 stdio 和 Streamable HTTP MCP 各执行一次。 [登录 shell 环境](../packages/core/src/shell_env.rs) · [Node 与 helper 发现](../packages/core/src/architecture/runtime.rs) · [Tauri 资源映射](../packages/core/tauri.conf.json)
- **网络与代理。** 现有 `reqwest` 的系统代理依赖在 Linux 从环境变量发现代理，原生系统设置分支覆盖 macOS 和 Windows。修正 Linux 的 `System` 提示，写明继承环境和登录 shell 的代理变量，测 `HTTP_PROXY`、`HTTPS_PROXY`、`ALL_PROXY`、`NO_PROXY` 和 `Direct`，覆盖设置中的连接测试与一次真实流式回复。GNOME 和 KDE 的代理设置及 PAC 留待有明确需求时实现。 [HTTP 客户端](../packages/core/src/http.rs) · [代理设置提示](../packages/desktop/src/features/settings/SettingsPanel.tsx) · [锁定的代理依赖实现](https://docs.rs/hyper-util/0.1.21/src/hyper_util/client/proxy/matcher.rs.html)

## 4. 验证 XDG 数据目录与升级

- 沿用 Tauri 目录解析和 `com.oaklet.desktop`。默认配置在 `~/.config/com.oaklet.desktop/config.json`，数据库和应用数据在 `~/.local/share/com.oaklet.desktop/`，日志在同一数据目录下的 `logs/oaklet.log`。分别以默认目录和自定义 `XDG_CONFIG_HOME`、`XDG_DATA_HOME` 验证首次启动、配置文件 `0600`、历史和附件恢复。 [应用启动](../packages/core/src/app.rs) · [配置写入](../packages/core/src/config.rs) · [锁定的 Tauri 目录实现](https://docs.rs/tauri/2.12.5/src/tauri/path/desktop.rs.html)
- 用前一内部 `.deb` 生成数据，再覆盖安装候选包，验证设置、会话、附件和 worktree 引用。验证卸载和重装保留用户目录。SQLite 沿用现有迁移器，平台兼容预计无需 schema 变更。如果实施中确需修改 schema，新增下一号 SQL 迁移并在 `packages/core` 运行 `cargo test` 和 `cargo clippy --all-targets`。 [数据库迁移规则](../AGENTS.md) · [迁移目录](../packages/core/migrations/)

## 5. 打包、验收和发布

新增独立 Linux 发布工作流，先由 `workflow_dispatch` 生成内部测试产物。复用 PNG 图标、Tauri 资源映射和现有构建钩子，明确选择 `.deb`。在 Ubuntu 24.04 x64 上从仓库根目录运行，保持构建基线与首版支持目标一致。若扩展到更早的发行版，从新的最老支持基线重新构建并验收 glibc 与 WebKitGTK 运行依赖。 [现有 Tauri 配置](../packages/core/tauri.conf.json) · [Tauri Debian 打包与系统兼容](https://v2.tauri.app/distribute/debian/)

```bash
pnpm tauri build --ci --target x86_64-unknown-linux-gnu --bundles deb
```

检查 `packages/core/target/x86_64-unknown-linux-gnu/release/bundle/deb/` 的产物。用 `dpkg-deb -f` 核对 `Architecture`、`Version` 和 `Depends`，用 `dpkg-deb -c` 核对 `.desktop` 文件、图标和 helper。验证包的依赖能解析到 GTK 3、WebKitGTK 4.1 及实际链接的 OpenSSL 运行库。若 OpenSSL 未被正确声明，使用 Tauri 的 `bundle.linux.deb.depends` 补齐 Ubuntu 24.04 的对应包。在干净虚拟机上用 `apt install` 安装下载的本地 `.deb`，由普通用户从应用菜单和命令行启动。 [Tauri Debian 依赖](https://v2.tauri.app/distribute/debian/) · [Ubuntu OpenSSL 运行库](https://launchpad.net/ubuntu/noble/+package/libssl3t64)

| 环境 | 放行条件 |
| --- | --- |
| Linux CI | Rust 测试和 Clippy、前端与 helper 测试和构建、`.deb` 生成及元数据和资源检查。 |
| 干净 Ubuntu 24.04 GNOME Wayland | 依赖解析、菜单启动、聊天和历史、工作区选择、单实例转发、输入法、剪贴板和退出。完成覆盖升级及卸载重装。 |
| Ubuntu 24.04 GNOME Xorg 和开发环境 | 复测窗口、输入法、拖放。覆盖 ext4 路径边界、Git 与 `@worktree`、PTY 及进程清理、代理、Architecture 和两种 MCP 传输。 |
| macOS 与 Windows 回归 | 现有 CI 通过。复测共享路径处理、快捷键和第二次启动的相对工作区，验证现有安装包构建。 |

验收通过后，让独立 Linux 工作流在 `v*` 标签对应的 GitHub Release 创建后追加 `.deb`。同步 README 的安装、快捷键和日志路径，以及 CONTRIBUTING 的 Linux 构建说明。遇到工作区边界错误、进程残留、安装依赖失配或升级丢数据时暂停 Linux 发布。已发布包出现阻断问题时撤下该 Linux 产物并保留用户目录，确认旧版能读取现有数据后再提供回退步骤。 [macOS 发布](../.github/workflows/macos-release.yml) · [Windows 发布](../.github/workflows/windows-release.yml) · [README](../README.md) · [CONTRIBUTING](../CONTRIBUTING.md)
