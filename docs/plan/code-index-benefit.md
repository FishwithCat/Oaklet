# Agent 语义工具收益试跑（步骤 10）

日期：2026-11-19。状态：试跑完成；步骤 10 未通过，正式验收仍待完成。

## 结论

- 16 次对照运行全部正确：旧名残留 0，新名数量与预期一致，漏改文件 0，额外改动 0。
- 默认配置下**新增工具组的 6 次运行一次也没有调用 `code_*`**；12 次对照运行的第一个工具调用全部是 `grep`。在 system prompt 末尾追加"优先用语义工具"的提示后（1 次试跑）仍为 0。
- 强制只用语义工具（去掉 grep）时任务仍全部正确，但 token 为基线的 5.4 倍（Rust）/ 14.7 倍（TS），耗时 5.1 / 6.8 倍。
- 这不是模型"不听话"：重命名任务用 grep 一次检索即可完成；语义工具要求文件路径和精确行列，冷启动期间返回 `not_ready`，输出也更长。当前形态下少用是理性选择。
- 按本计划门槛（基线零漏改时不宣称通过），**步骤 10 仍未通过**；本次数据只能说明"默认 agent 不会选这些工具"，不能说明它们在该任务类别上无收益。

## 环境

| 项 | 值 |
| --- | --- |
| 模型 | `deepseek-ai/DeepSeek-V4.1-Flash`，OpenAI 兼容接口，`temperature=0` |
| 工程 | Acorn 源码快照（仅 `crates/`、`frontend/`、清单文件，2.4 MB，无 `target/`、无 `.git`） |
| Rust 后端 | rust-analyzer 1.99.1 |
| TS 后端 | typescript-language-server 6.1.0，Node 26.13.0 |
| 真值 | Rust `format_cny` 12 处 / 5 个文件；TS `formatFieldValue` 14 处 / 3 个文件 |

Acorn 快照位于 `/private/tmp/oaklet-bench/base`，来自既有 `/private/tmp/oaklet-acorn-lsp-spike` 的源码副本。原始工程不受影响。

## 方法

三组，提示词、模型、初始代码、预算一致：

| 组 | 工具集 | 说明 |
| --- | --- | --- |
| `base` | 生产基础工具（read/write/edit/patch/glob/grep/list_dir/jobs） | 无索引工具 |
| `code` | 基础工具 + `code_symbols`/`code_definition`/`code_references`/`code_calls` | 默认指导语，未加偏好 |
| `code_only` | 只保留 `code_*` 与 read/list/glob/edit 类工具，去掉 grep | 诊断：强制使用语义工具，测能力而非选择 |

三组都移除了 `run_command`，避免探针触发工程构建；`base` 与 `code` 各跑 3 次，`code_only` 各跑 2 次。另加 1 次 `code` + 偏好提示的试跑（不计入均值）。

任务：

- Rust：把 `format_cny` 全量重命名为 `format_cny_amount`，覆盖定义、`pub use` 重导出、导入、调用点和测试。
- TS：把 `formatFieldValue` 全量重命名为 `formatFieldCell`，范围 `frontend/src`。

计分：运行结束后统计源码中旧名/新名出现次数、与基线逐文件比较（排除 `target/`、`node_modules/`），记录工具调用序列、token 用量、运行耗时，以及索引就绪等待时间（仅 `code*` 组，轮询 `code_definition` 直到成功）。

探针位于 `packages/core/src/agent/bench.rs`（`#[ignore]`，由规格文件驱动）。它复用生产 `ToolRegistry`、工具 schema、guidance、system prompt 和工具实现，但不经过 Tauri 运行循环，因此没有压缩、重试、审批卡片和会话持久化；这是工具选择与导航的对照，不是完整产品链路的验收。

## 结果

| 运行 | 成功 | 旧名残留 | 新名 | 漏改文件 | 额外文件 | 调用数 | code_* | tokens | 秒 | 就绪秒 |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| rust-base-1 | 是 | 0 | 12 | 0 | 0 | 18 | 0 | 39195 | 24.5 | 0.0 |
| rust-base-2 | 是 | 0 | 12 | 0 | 0 | 12 | 0 | 31737 | 18.1 | 0.0 |
| rust-base-3 | 是 | 0 | 12 | 0 | 0 | 18 | 0 | 59492 | 34.0 | 0.0 |
| rust-code-1 | 是 | 0 | 12 | 0 | 0 | 21 | 0 | 86164 | 28.6 | 205.6 |
| rust-code-2 | 是 | 0 | 12 | 0 | 0 | 20 | 0 | 70987 | 34.6 | 7.2 |
| rust-code-3 | 是 | 0 | 12 | 0 | 0 | 19 | 0 | 69829 | 25.1 | 24.2 |
| rust-code_only-1 | 是 | 0 | 12 | 0 | 0 | 37 | 10 | 207228 | 96.9 | 11.3 |
| rust-code_only-2 | 是 | 0 | 12 | 0 | 0 | 33 | 7 | 261094 | 164.9 | 3.8 |
| ts-base-1 | 是 | 0 | 14 | 0 | 0 | 14 | 0 | 50923 | 21.6 | 0.0 |
| ts-base-2 | 是 | 0 | 14 | 0 | 0 | 13 | 0 | 43872 | 22.8 | 0.0 |
| ts-base-3 | 是 | 0 | 14 | 0 | 0 | 16 | 0 | 63264 | 26.7 | 0.0 |
| ts-code-1 | 是 | 0 | 14 | 0 | 0 | 17 | 0 | 99501 | 33.9 | 2.7 |
| ts-code-2 | 是 | 0 | 14 | 0 | 0 | 14 | 0 | 61153 | 25.3 | 2.7 |
| ts-code-3 | 是 | 0 | 14 | 0 | 0 | 10 | 0 | 42232 | 17.1 | 2.7 |
| ts-code_only-1 | 是 | 0 | 14 | 0 | 0 | 68 | 24 | 1185274 | 221.9 | 1.7 |
| ts-code_only-2 | 是 | 0 | 14 | 0 | 0 | 51 | 4 | 361518 | 100.3 | 1.7 |

均值：

| 任务 | 组 | 成功 | tokens | 秒 | 调用数 | code_* |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| Rust | base | 3/3 | 43,475 | 25.5 | 16.0 | 0 |
| Rust | code | 3/3 | 75,660 | 29.4 | 20.0 | 0 |
| Rust | code_only | 2/2 | 234,161 | 130.9 | 35.0 | 8.5 |
| TS | base | 3/3 | 52,686 | 23.7 | 14.3 | 0 |
| TS | code | 3/3 | 67,629 | 25.4 | 13.7 | 0 |
| TS | code_only | 2/2 | 773,396 | 161.1 | 59.5 | 14.0 |

## 观察

1. **天花板效应。** 两个任务都是 grep 友好的重命名，基线 6/6 正确，`code` 组也 6/6 正确。计划写明"基线本来零漏改……不宣称门槛通过"，因此这组数据不能证明收益。
2. **新增工具零采用。** `code` 组 6 次运行没有任何 `code_*` 调用；12 次对照运行的第一步全部是 `grep`。加偏好提示后也没有改变（该次 101,708 tokens、40.4 秒、23 次调用，仍 0 次 `code_*`）。
3. **强制使用更贵。** `code_only` 组虽然正确，但 Rust token 为 base 的 5.4 倍、TS 为 14.7 倍；耗时为 5.1 / 6.8 倍。TS 最差一次 1,185,274 tokens、68 步、24 次 `code_*`。
4. **接口摩擦直接可见。** 强制组出现这些失败：`code_symbols` 传入目录 `frontend/src` 被判为未发现文件；索引刚就绪时 `code_symbols` 仍返回 `NotReady`；`code_definition` 报"Column is outside the saved line or splits a character"；一次 `apply_patch` 因多文件补丁行号不匹配被整体拒绝。
5. **就绪时间波动大。** Rust 的 `readyMs` 为 205.6 / 7.2 / 24.2 秒，TS 稳定在 2.7 秒。205 秒那次与 flycheck 无关（见下），说明冷启动仍不稳定，值得单独排查。
6. **`code` 组 token 反而偏高（+74% / +28%），但没有一次工具调用差异。** 样本只有 3 次，且 guidance 每步都会进入 prompt；更像是推理长度波动，不足以归因。

## 解释

- 对"同名符号全量重命名"，grep 一次就能拿到全部 12/14 处，语义工具却要求"先知道一个已发现文件，再给精确行列"。`code_symbols` 即使只按名字搜索也强制 `path`，把发现场景让给了 grep。
- 索引就绪是"按文件/按查询"逐步可用的，`NotReady` 出现在已通过就绪探测之后；模型遇到一两次就会按 guidance 退回 grep，并且不再尝试。
- `code_only` 中模型为了弥补没有 grep，反复 `code_symbols` + `read_file`，上下文快速膨胀；这解释了 token 的倍数差。

## 限制

- 只测了重命名，不能代表调用链、跨语言边界或"漏改会编译失败"的修改任务；这些正是语义工具理论上的强项。
- Acorn 快照不可构建，没有 `cargo test` / `tsc` 结果；正确性由旧名/新名计数和逐文件 diff 判定。
- 移除了 shell，因此两组都无法运行构建或测试；这是为了控制成本的有意偏差。
- 单模型、单工程、每组 2–3 次；温度 0 仍不能消除推理长度波动。
- 探针不经过 Tauri 运行循环；没有覆盖多窗口、关闭重开、缓存命中、恢复核对和完整链路。
- `readyMs` 由探针轮询得到，不是产品状态接口的时延指标。

## 决策

- 步骤 10 保持未通过。下一步应先选"必须靠引用/调用关系才能找全"的任务（例如签名变更后编译失败、跨 crate trait 实现、Rust↔TS 边界），再按同样方法对照；重命名只适合做工具可用性冒烟。
- 在接口修好之前，不给 agent 加"请优先用语义工具"的硬提示：本次唯一一次偏好提示没有改变选择，反而增加 prompt 开销。
- 优先修三件事：`code_symbols` 的 `path` 在 `query` 模式下可选（或接受目录/项目根）；把"索引可用"做成模型可见的状态；压缩结果，减少 `NotReady` 期间的无效尝试。

## 复现

```bash
# 1) 生成规格（示例：Rust code 组）
python3 /private/tmp/oaklet-bench/make_spec.py \
  --ws /private/tmp/oaklet-bench/workspaces/rust-code-1 \
  --group code --task rust \
  --prompt 'Rename the Rust function `format_cny` to `format_cny_amount` ...' \
  --spec /private/tmp/oaklet-bench/specs/rust-code-1.json \
  --out /private/tmp/oaklet-bench/results/rust-code-1.json \
  --ready-path crates/acorn-core/src/agent/cost.rs --ready-line 52 --ready-column 8

# 2) 运行（core 目录）
OAKLET_BENCH=/private/tmp/oaklet-bench/specs/rust-code-1.json \
  cargo test --lib --offline --locked agent::bench::benefit_pilot -- --ignored --nocapture

# 3) 计分
python3 /private/tmp/oaklet-bench/score.py
```

原始数据：`/private/tmp/oaklet-bench/results/*.json`、`scores.json`、`logs/*.log`。

## 本次附带变更

- `packages/core/src/code_index/lsp.rs`：Rust 初始化选项增加 `check.onSave=false`（及旧键 `checkOnSave=false`）。试跑前 rust-analyzer 在副本中执行了 flycheck，生成 616 MB `target/`；关闭后 16 次运行不再生成。这是产品行为变更（不再触发后台 `cargo check`），建议单独审查和提交。
- `packages/core/src/agent/bench.rs`、`tools/mod.rs` 的 `retain`：仅测试使用的探针和过滤器，不进入生产路径。

## 验证

- `cargo test --offline --locked`：372 通过，7 忽略，0 失败。
- `cargo clippy --all-targets --offline --locked -- -D warnings`：通过。
- `cargo fmt --all -- --check`：通过。
- 未改前端，未运行 desktop 的 `pnpm test` / `pnpm build`。
