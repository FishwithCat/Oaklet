# 代码索引步骤 1：Acorn LSP 穿刺

日期：2026-11-18  
结论：Rust/TS 的 stdio 客户端、定义、引用、文件符号和双向调用层次均已实测通过。可继续项目发现与工作区服务实现；请求取消和就绪后的缓存准入仍需按下述发现补齐，当前示例不是生产 Service。

## 范围与环境

- 测试源：用户指定的 `/Users/linear/Workspace/Acorn` 当前工作树，HEAD 为 `1036b373a8cd50a74b83082c3567df73e414ccda`，包含已有未提交修改，并非该提交的干净快照。
- 将根 Cargo 清单、锁文件、crates、frontend 的源码、测试工具和配置复制到 `/private/tmp/oaklet-acorn-lsp-spike`；frontend/node_modules 只建立到原有依赖的符号链接。未在 Acorn 中编辑源码、安装依赖或启动数据库、后端及 LLM。
- Node `26.13.0`；Rust/Cargo `1.99.1`；rust-analyzer `1.99.1 (58cf8f9c 2026-10-13)`；typescript-language-server `6.1.0`；TypeScript `5.9.3`。TS LSP 及其 TypeScript 安装在临时专用目录，不改 Acorn package.json 或锁文件。
- 新增依赖实际锁定为 async-lsp `0.2.4`、lsp-types `0.95.1`、tokio-util `0.7.20`、notify `8.2.2`、blake3 `1.8.7`。notify 与 blake3 当前仅完成编译接入，行为测试仍属于后续监听与持久化步骤。
- Rust 禁用 build scripts、proc-macro 和 checkOnSave，采用现有本地 Cargo 依赖缓存并禁止联网自动安装；索引产物位于临时副本。未宣称执行了 Acorn 自身的编译或业务测试。

## 实测结果

| 查询 | Rust：format_cny | TS：formatFieldValue |
| --- | --- | --- |
| 定义 | cost.rs 第 52 行 | format.ts 第 33 行 |
| 文件符号 | 6 个顶层符号 | 7 个顶层符号 |
| 引用，排除定义 | 11 处，覆盖 5 个文件与两个 crate | 最终 12 处，覆盖 3 个文件 |
| incomingCalls | 5 个节点 | 3 个节点 |
| outgoingCalls | 2 个节点，目标在 Rust 标准库 | 1 个节点：formatDate |
| shutdown/exit | 正常，退出码 0 | 正常，退出码 0 |

Rust 引用覆盖 cost.rs、core/lib.rs、eval/report.rs、tui/app.rs 和 tui/app/render.rs。上游包括 command、render_markdown、render_case、draw_status 和 zero_usage_is_free；实际断言检查了 render_markdown 跨文件调用。

TS 引用覆盖 format.ts、EntityTable.tsx 与 format.test.ts。上游为 formatFunctionValue、EntityTable 和测试文件的模块级节点；断言检查了 EntityTable 的跨文件调用，以及下游 formatDate。调用层次节点不一定都是函数，生产归一化必须保留 kind。

所查源码 SHA-256（用于识别这次样本，不是正式 BLAKE3 索引格式）：

- `crates/acorn-core/src/agent/cost.rs`：`e6ed17ef71f81b9bc059904ea5e65f69f715d75f11e055e58613d5fa37ae18c4`
- `frontend/src/lib/format.ts`：`0cb22a31857f6cf669f0c7700ed6b08040c9193fbc6756d46b303b1962677090`

## 需要带入正式实现的发现

1. **必须主动声明客户端能力。** TS LSP 只有看到 `textDocument.callHierarchy` 才返回 callHierarchyProvider。最初未声明时它并非真正不支持；现已补充能力声明，并由假服务器断言防回退。声明 linkSupport 后定义结果为 LocationLink，不能只处理 Location。
2. **初始化和成功非空结果均不代表项目加载完毕。** TS 的四次引用样本为 `1 → 12 → 12 → 12`，首次约 543ms，后续约 2.6s 起达到 12；Rust 为 `11 → 11 → 11 → 11`。示例在查询调用层次后每隔 1 秒再取三次引用，只用于观察漂移，重复结果不证明完整性，也不是生产就绪算法。不得将首次成功的非空结果直接持久缓存十分钟。
3. **框架不自动取消出站请求。** tokio timeout 丢弃 future 后，假服务器没有收到 `$/cancelRequest`，迟到响应不会串到下一查询。但未响应请求的内部记录不会因丢弃 future 自动清理，长期会话需要显式解决。async-lsp 的内部出站 ID 不由当前高层 request API 暴露；步骤 3 必须先验证请求 ID、取消与回收适配，不能机械搬用示例的 timeout 包装。
4. **框架没有消息长度上限。** 源码按 Content-Length 分配缓冲；示例增加仅负责检查帧边界的转发保护，头上限 8 KiB、消息体上限 8 MiB，并以有界 duplex 接入框架，没有替换 JSON-RPC 处理。超大头、超大声明体、重复 Content-Length、截断体和非法 JSON 均有测试。
5. **外部目标与进度需要单独处理。** Rust 下游指向工作区外标准库，应只返回外部位置，不自动读文件；进度通知数量可很高，示例只聚合有界的通知计数。缺少可选 rust-analyzer 配置文件产生警告，但此次查询和退出正常。

这次协议与能力穿刺通过，不等于跨进程取消、缓存一致性、监听更新或 Agent 漏改收益已通过。保持已选 async-lsp 路线，步骤 3 将取消适配列为进入生产工具前的必要门槛；若公开 API 无法满足，应先报告并评估最小依赖 API 修正，不偷偷换框架或保留无界请求泄漏。

## 代码与自动检查

- `packages/core/examples/lsp_spike.rs`：通用命令行探针，输出结构化 JSON，查询或正常退出失败时返回非零退出码。SYMBOL 定位文件中第一次出现的位置，因此复现需选择首次出现即目标声明的符号。
- `packages/core/examples/lsp_spike/client.rs`：Tokio/async-lsp stdio 连接、必要客户端回调、有界帧输入和 stderr 收集、进程退出。
- `packages/core/examples/lsp_spike/tests.rs` 与 fake_server.py：独立 Python 标准库 stdio peer，覆盖服务器反向请求、交错通知、超时迟到响应、不支持方法、非法 JSON 及帧限制；不需要真实服务器或 Acorn。
- `cargo test --example lsp_spike --offline --locked`：3 项通过。普通 cargo test 不执行 example 内测试，需明确运行此命令。
- `cargo test --offline --locked`：302 项通过。首次受沙箱本地端口限制失败，放宽沙箱后重跑通过。
- `cargo clippy --all-targets --offline --locked -- -D warnings`：通过。

## 复现

以下命令从 Oaklet 的 packages/core 目录执行。真实服务器和 Acorn 依赖需已安装；临时目录名与路径按本机调整。

```sh
cargo test --example lsp_spike --offline --locked
cargo build --example lsp_spike --offline --locked
npm install --prefix /private/tmp/oaklet-lsp-spike-tools --ignore-scripts --no-audit --no-fund typescript-language-server@6.1.0 typescript@5.9.3
```

建立源码副本（不复制环境变量文件或 Acorn 的本地数据库）：

```python
from pathlib import Path
import shutil

src = Path('/Users/linear/Workspace/Acorn')
dst = Path('/private/tmp/oaklet-acorn-lsp-spike')
dst.mkdir(exist_ok=True)
for name in ('Cargo.toml', 'Cargo.lock'):
    shutil.copy2(src / name, dst / name)
shutil.copytree(src / 'crates', dst / 'crates', dirs_exist_ok=True,
                ignore=shutil.ignore_patterns('target', 'node_modules'))
for name in ('src', 'test-utils'):
    shutil.copytree(src / 'frontend' / name, dst / 'frontend' / name, dirs_exist_ok=True)
for name in ('package.json', 'tsconfig.json'):
    shutil.copy2(src / 'frontend' / name, dst / 'frontend' / name)
modules = dst / 'frontend/node_modules'
if not modules.exists():
    modules.symlink_to(src / 'frontend/node_modules', target_is_directory=True)
```

运行真实查询；CARGO_HOME 指向本机 Acorn 已有的依赖缓存，其他机器按实际缓存位置设置：

```sh
RUSTUP_AUTO_INSTALL=0 CARGO_NET_OFFLINE=true CARGO_HOME=/Users/linear/Workspace/Acorn/.cargo \
  target/debug/examples/lsp_spike /private/tmp/oaklet-acorn-lsp-spike rust \
  crates/acorn-core/src/agent/cost.rs format_cny /Users/linear/.cargo/bin/rust-analyzer \
  > /private/tmp/oaklet-acorn-rust-spike.json

target/debug/examples/lsp_spike /private/tmp/oaklet-acorn-lsp-spike/frontend typescript \
  src/lib/format.ts formatFieldValue \
  /private/tmp/oaklet-lsp-spike-tools/node_modules/.bin/typescript-language-server --stdio \
  > /private/tmp/oaklet-acorn-ts-spike.json
```

原始 JSON 保留在上述临时路径；计数包括 import/re-export 等引用，不能把引用数直接当调用点数。本轮时间仅为本机已有依赖和缓存下的观测，不作为冷启动性能承诺。

未实现工作区服务、监听、SQLite 索引、MCP、安装界面或 Tauri IPC 证据工具；Acorn 是 HTTP 工程，最小 Tauri 样本留到 IPC 步骤补齐。没有修改迁移，也没有执行 git commit。
