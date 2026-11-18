# 语言服务器安装

用户在 Settings → Code Index 的失败项目卡片发起安装。界面通过原来的状态事件同步各语言的安装阶段与错误，一种语言同时只有一个安装任务。发布后为打开的工作区重新检测后端，并由现有进程池启动。各卡片保留独立的项目启动结果，安装验证成功不代表语义索引完成。

| 后端 | 固定版本或来源 | 安装位置 |
| --- | --- | --- |
| rust-analyzer | 项目选定的已安装 Rust 工具链 | rustup 工具链组件 |
| TypeScript / JavaScript | typescript-language-server 6.1.0 和 TypeScript 5.9.3 | Oaklet 数据目录 |
| Pyright | 1.1.407 | Oaklet 数据目录 |
| gopls | v0.17.1 | Oaklet 数据目录 |
| Eclipse JDT LS | [1.63.0 构建 202610191322](https://download.eclipse.org/justj/?file=jdtls%2Fmilestones%2F1.63.0) | Oaklet 数据目录 |

npm 使用已有的 Node.js 和 npm。Windows 使用标准 Node.js 发行目录中的 `node_modules/npm/bin/npm-cli.js`，通过 Node 直接执行。Go 使用已有的 Go 工具链和专用 GOBIN，项目和工具链自动切换关闭。JDT LS 使用官方 HTTPS 归档和仓内固定的 SHA-256，在解包前校验内容。Java 需 JDK 21+ 和 Python 3，启动时使用安装包自带的 Python 入口及会话独立的配置和数据目录。

四种独立后端在同一个应用数据文件系统暂存并验证。初始化成功后把目录重命名到版本目录，下一次安装回收中断留下的暂存目录。Rust 组件的发布和回收由 rustup 管理。工作区关闭后共享安装可以继续，应用退出会取消安装。安装被取消前已由 rustup 发布的组件仍可使用。

普通回归不联网。真实安装验收使用临时应用数据及两个独立工作区，每次指定一种语言：

```sh
cd packages/core
OAKLET_INSTALL_TEST=typescript cargo test --locked real_install_retries_both_open_workspaces -- --ignored --nocapture
```

可依次替换为 `rust`、`python`、`go`、`java`。程序通过该测试所处环境的 PATH 查找依赖，包管理器的系统代理与 registry 配置仍适用。Java 必须能访问 Eclipse 官方下载地址。

macOS arm64 上使用 Node.js 26.13.0 验证了 TypeScript 6.1.0 和 Pyright 1.1.407 的真实 npm 安装。每轮两个工作区都进入可查询状态，重复发起安装共用一个任务。Go 1.24.2 上的 gopls v0.17.1 也通过安装和双工作区初始化。Rust 1.99.1 的 rust-analyzer 已在本机工具链中，验证了 rustup 安装入口的幂等行为及两个工作区的初始化。

Eclipse 官方校验文件及归档中的启动入口已核对。本机下载完整 JDT LS 归档时超时，真实安装和初始化仍待验收。未在 Windows 或 Linux 上运行真实安装，也未完成桌面 GUI 人工验收。

自动化覆盖缺少安装工具、子进程失败和取消、暂存清理及保留旧版本。Core 测试 372 通过、6 跳过，Clippy 和格式检查通过。桌面测试 114 通过，构建通过。

建议提交边界：安装服务、桌面入口、用户说明与验收记录属于同一项可独立工作的功能，建议提交消息为 `feat(code-index): add language server installation`。
