# Oaklet 签名与分发方案

把"签名"拆成互相独立的两层：升级包签名决定用户主动更新时的产物能否验证，代码签名决定用户首次安装时操作系统是否拦截。前者不需要任何证书，后者需要付费证书。更新只在设置面板手动发起，启动与后台都不检查。macOS 与 Windows 的门槛不同，Linux 不需要。价格与政策依据 2026 年 11 月的公开信息，实施前需重新核对。

## 1. 拆分两种签名

- 升级包签名使用 Tauri 的 minisign 密钥对，与操作系统信任链无关。公钥写入应用配置，私钥只进仓库 Secret，重新发布前用 `tauri signer generate` 生成并离线备份。
- 代码签名使用 Authenticode（Windows）或 Developer ID（macOS），由证书颁发机构验证发布者身份，决定 SmartScreen 与 Gatekeeper 是否拦截首次运行。
- 先做升级包签名，再做代码签名。前者零成本，且在没有证书的情况下同样生效。[Cargo.toml](../packages/core/Cargo.toml) · [Tauri Updater](https://v2.tauri.app/plugin/updater/)

## 2. 设置中的手动更新

- 现状是 `packages/core/Cargo.toml` 只注册了 dialog、opener 和 single-instance 三个插件，`packages/core/tauri.conf.json` 没有 updater 配置，也没有 `bundle.createUpdaterArtifacts`。用户每次升级都要回到 Releases 重新下载 dmg、exe 或 deb。
- 增加 `tauri-plugin-updater` 依赖并注册插件，配置 `bundle.createUpdaterArtifacts` 与 `plugins.updater` 的公钥和指向 GitHub Releases 的 `latest.json` 端点。
- 不做启动检查、不做定时轮询、不做后台下载。启动、窗口获得焦点、长时间空闲都不请求版本清单，唯一的触发点是设置面板里的检查操作。
- 检查结果只显示当前版本、可用版本和对应发布的链接，由用户决定是否下载。下载完成后由用户自己重启应用，不在会话进行中重启，也不静默替换正在运行的版本。
- 下载的升级包必须通过 minisign 校验，公钥编译进应用。校验失败时丢弃产物并显示原因，不提供忽略并继续的选项。该密钥对与操作系统证书无关，不需要付费。
- 三个发布工作流在发布资产里附上各平台的 `.sig` 与 `latest.json`。macOS 的 aarch64 与 x64、Windows 的 NSIS、Linux 的 deb 各自给出签名产物。[macOS 发布](../.github/workflows/macos-release.yml) · [Windows 发布](../.github/workflows/windows-release.yml) · [Linux 发布](../.github/workflows/linux-release.yml)
- 保留 `requireSignedVersion`，让更新响应里的版本号与实际签名的产物绑定，拒绝把版本号与另一次发布的产物配对的响应。
- 检查、下载或安装失败时在设置面板显示原因，应用继续以当前版本运行，日志记录一次失败，不打断进行中的会话。

## 3. 产物可验证性

- 三个发布工作流生成 `SHA256SUMS` 并随 Release 上传。
- 用 `actions/attest-build-provenance` 为 dmg、setup.exe 和 deb 生成构建来源证明，下载者能确认产物来自本仓库的 CI，而不是某台本地机器。
- README 增加"验证下载"一节，写清 `shasum -a 256 -c` 与 `gh attestation verify` 两条命令。
- 这一层不消除系统警告，但提供了独立于操作系统的证据链，与本地可审计的产品定位一致。[README](../README.md)

## 4. macOS 签名与公证

- 现状是 ad-hoc 签名，Gatekeeper 在首次启动时拦截。README 当前给出的处理方式是**系统设置 → 隐私与安全性 → 仍要打开**，或 `xattr -dr com.apple.quarantine`。
- Homebrew Cask 不再是绕过路径。Cask 默认给安装后的应用加 quarantine，`--no-quarantine` 与 `quarantine false` 已废弃并在移除中，Homebrew 自 2026-09-01 起不再支持无法通过 Gatekeeper 的 cask。GUI 应用通过 Cask 分发必须先有 Developer ID 签名与公证。
- 前置条件是 Apple Developer Program。做法是在 macOS 发布工作流里用 Developer ID Application 证书签名，开启 hardened runtime，用 `xcrun notarytool submit --wait` 提交公证，再用 `xcrun stapler staple` 把票据装订到 app 与 dmg。
- 证书、App Store Connect API key 与团队标识进仓库 Secrets，不进仓库。签名失败时工作流必须失败，不允许静默发布未签名产物。[macOS 发布](../.github/workflows/macos-release.yml)
- 在拿到证书之前，README 的安装章节必须直接写明"当前为未签名构建，首次打开需要一步终端命令"，不能让用户在装到一半时才发现。

## 5. Windows 签名

- 现状是 `*-setup.exe` 未签名，SmartScreen 显示"Windows 已保护你的电脑"，README 给出 `Unblock-File` 与"仍要运行"两条路径。
- 当前可用的免费通道已基本关闭：
  - SignPath Foundation 免费为开源项目签名，但按项目的公开信任信号评估，包括 star、fork、贡献者、外部文章与讨论、持续活动。新仓库直接申请会被拒绝。
  - Certum 的免费开源证书已于 2016 年停止，现有的开源证书为付费（约 69 欧元首年、29 欧元续费）。
  - OSSign 因积压暂停受理。
  - Sigstore 可以对产物做无密钥签名，但签名不属于 Authenticode，不消除 SmartScreen。
  - EV 证书自 2024 年起不再提供 SmartScreen 即时信誉，与 OV 没有差别。
  - 自签名证书不可用，证书链不受信任时警告与未签名相同。
- 顺序是先用"更多信息 → 仍要运行"完成 Windows 实机验收，积累公开讨论后再申请 SignPath。若届时仍被拒且 Windows 用户占比可观，再购买 Certum 开源证书。
- 签名覆盖 NSIS 安装包及其内嵌的可执行文件，使用时间戳服务，并在发布工作流末尾校验签名与证书指纹，未通过则失败。[Windows 发布](../.github/workflows/windows-release.yml) · [SignPath Foundation](https://signpath.org/)

## 6. Linux

- 本地安装的 `.deb` 不需要签名，只有通过 apt 仓库分发时才需要 GPG 签名。这是唯一零成本分发的平台，优先把安装与升级路径做完整。[Linux 发布](../.github/workflows/linux-release.yml)

## 7. 去掉 Node.js 运行时依赖

- 现状是 Architecture 视图与 `likec4_mcp` 需要宿主安装 Node.js 22.23.3 或更高。查找顺序是 `OAKLET_NODE`、`PATH`、常见安装位置，找不到时降级为文字提示。helper 的 JavaScript 产物已经通过 `bundle.resources` 打进安装包，缺的只是运行时。[runtime.rs](../packages/core/src/architecture/runtime.rs) · [tauri.conf.json](../packages/core/tauri.conf.json)
- 三个可选做法：
  1. 把 node 作为 sidecar：在 `bundle.externalBin` 声明，按目标三元组命名，发布工作流按平台下载对应版本。改动最小，包体约增加 60 MB。
  2. 把 helper 与运行时合并成单一可执行文件后同样以 sidecar 分发。包体更小，构建链更复杂。
  3. 把 LikeC4 编译迁到 Rust 侧。工作量最大，本阶段不做。
- `OAKLET_NODE` 与显式路径的优先级保持不变，让希望使用系统 Node 的用户仍可覆盖。

## 8. 分发渠道登记

- macOS：签名与公证通过后向 Homebrew 官方 cask 提交。在此之前只提供 dmg。
- Windows：提交 winget manifest。NSIS 已按当前用户安装，符合 winget 对安装范围的默认假设。
- Linux：现有 deb 直接发布。AUR、AppImage 与 RPM 属于后续范围，见 [Linux 兼容方案](linux-migration-plan.md) 的边界。

## 验收

| 验收场景 | 通过条件 |
| --- | --- |
| 手动检查更新 | 在设置面板检查并安装上一个版本的升级包，重启后版本号更新，会话、工作区与设置保持。 |
| 无后台检查 | 启动、切换窗口、长时间空闲期间不产生更新相关的网络请求；只有设置面板的操作会读取版本清单。 |
| 升级包签名 | 篡改下载的升级包后校验失败，安装被拒绝，设置面板显示原因且不提供忽略选项。 |
| 更新响应绑定 | 手工构造版本号与产物不匹配的 `latest.json`，客户端拒绝该响应且保留当前版本。 |
| 更新失败 | 断网、端点返回错误或写入失败时，设置面板显示原因，应用继续以当前版本运行，当前会话不受影响。 |
| 产物校验 | 三个平台的产物都能用 `SHA256SUMS` 校验通过，`gh attestation verify` 返回来自本仓库的构建。 |
| macOS 首次启动 | 干净系统下载 dmg、拖入 Applications、双击打开，全程无"仍要打开"提示、无 `xattr` 命令、无终端操作。 |
| macOS 公证 | `spctl --assess` 对 app 与 dmg 均通过，`stapler validate` 返回成功。 |
| Windows 首次启动 | 干净系统运行 setup.exe，无 SmartScreen 拦截；签名证书指纹与发布记录一致。 |
| Node 缺失 | 未安装 Node.js 的系统上，Architecture 视图与 `likec4_mcp` 均可用；设置 `OAKLET_NODE` 后仍优先使用指定的运行时。 |
| README 一致性 | 安装章节与当前签名状态一致，已签名的产物不再出现 `xattr` 或 `Unblock-File` 步骤。 |
