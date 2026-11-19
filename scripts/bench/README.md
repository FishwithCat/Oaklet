# rename pilot

`rename_pilot.py` 是步骤 10 收益验收的可重复执行版本。它在固定版本的
`rust-lang/regex` 上让真实模型做一次跨 crate 改名，对比生产工具集（`base`）与
额外注册四个代码索引工具（`code`）的遗漏率。

任务：把 `regex_syntax::hir::Look` 改名为 `LookKind`，同时保留
`regex-automata/src/util/look.rs` 里同名但无关的 `Look`，以及 `regex-lite`
中的同名类型。

## 前置条件

- macOS/Linux，Python 3.10+，Git；
- `cargo` 可构建 `packages/core`；
- `rust-analyzer` 在 `PATH` 中（`code` 组需要）；
- 一个 OpenAI 兼容的 chat completions 端点。默认读取 Oaklet 桌面配置：
  `~/Library/Application Support/com.oaklet.desktop/config.json`，
  也可用 `--config` 或环境变量 `OAKLET_BENCH_CONFIG` 指定。

API key 不会写进 spec 文件；脚本通过 `OAKLET_BENCH_API_KEY` 传给
`agent::bench::benefit_pilot`。spec、日志和补丁都落在输出目录，默认在
`.data/bench/rename-pilot/<timestamp>/`（`.data/` 已被忽略）。

## 运行

```bash
# 1 次 base + 1 次 code 的冒烟测试
python3 scripts/bench/rename_pilot.py \
  --repo /path/to/rust-lang/regex \
  --runs 1 --jobs 2

# 正式样本：每组 16 次，四路并行
python3 scripts/bench/rename_pilot.py \
  --repo /path/to/rust-lang/regex \
  --runs 16 --jobs 4
```

不传 `--repo` 时会克隆 `https://github.com/rust-lang/regex`，并 checkout
脚本内固定的 `PINNED_REV`。建议显式传本地 checkout，避免网络波动。

如果中途因余额、限流或断网中断，用 `--resume` 保留已经通过校验的样本，只补跑
缺失的编号：

```bash
python3 scripts/bench/rename_pilot.py \
  --repo /path/to/rust-lang/regex \
  --runs 16 --jobs 4 \
  --resume .data/bench/rename-pilot/20260101-120000
```

`--resume` 会跳过 `agent_error`、`agent_timeout` 等无效运行，不会把 402 或网络
错误当成有效失败。`error sending request`、`error decoding response body` 和
HTTP 429/5xx 会按 `--retries` 自动重试。

只想重新校验已有补丁、不再次调用模型时，用 `--recheck`。它会按
`summary.json` 中的运行列表重建 worktree、应用 `patches/` 里的 diff，并重跑
编译检查：

```bash
python3 scripts/bench/rename_pilot.py \
  --repo /path/to/rust-lang/regex \
  --cargo-home /path/to/cargo-home \
  --recheck --resume .data/bench/rename-pilot/20260101-120000
```

如果 regex 的依赖不在默认 `~/.cargo`，用 `--cargo-home` 指定。rust-analyzer 与
`cargo check` 共用该缓存，Oaklet 探针仍用原有环境构建。`code` 组在模型运行前
先确认 rust-analyzer 能找到 `regex-automata` 的跨 crate 引用。比较改动后的效果时
使用新输出目录。

## 并行方式

每次尝试都有自己的 `git worktree`，因此编辑互不可见。脚本先构建一次
`agent::bench::benefit_pilot` 测试二进制，然后直接启动该二进制，避免多个
`cargo test` 争用同一个 target 锁。`--jobs` 控制同时进行的模型运行数；
`--check-jobs` 控制最后的 `cargo check` 并发数（默认 1）。

每次编译检查使用独立的 `target-check/<run id>`，不共用 target 目录。cargo 的
dep-info 记录相对路径；共用 target 时，第一次成功构建之后，其他 worktree 的
源文件 mtime 更旧，cargo 会误判为 fresh，把编译错误当成通过。独立 target 让
`--check-jobs` 可以安全并行。

并行度主要受 API 限流、内存和 rust-analyzer 影响。10 核 / 24 GB 的机器上
建议 `--jobs 4` 到 `6`。每个 `code` 尝试会启动一个 rust-analyzer；如果内存
紧张，降低 jobs 或先只跑 `--arms base`。

## 判分

一次运行只有同时满足以下条件才算通过：

1. `cargo check -p regex-syntax -p regex-automata -p regex -p regex-lite`
   通过（漏改会在这里变成 unresolved import / cannot find type）；
2. `regex-syntax/src/hir/mod.rs` 中确实出现 `pub enum LookKind`；
3. `regex-automata/src/util/look.rs`、`regex-automata/src/nfa/thompson/builder.rs`
   和 `regex-lite/` 没有被修改（这些位置有同名但无关的 `Look`）。

第 3 条用于区分“把两个同名类型一起改掉所以能编译”与“只改目标类型”。脚本
同时记录改动文件、编译错误数、`code_*`/`code_references` 调用次数、token 和
耗时。

## 输出

```text
<out>/
  specs/       # 每个尝试的 bench spec（不含 key）
  results/     # benefit_pilot 写出的模型轨迹与 usage
  logs/        # 模型运行的 stdout/stderr，按尝试次数分文件
  patches/     # 每个尝试结束时的 git diff
  checks/      # 每个尝试的 cargo check 输出
  target-check/# 每次尝试独立的编译缓存，--recheck 会重建
  summary.md   # 人读的逐次/分组统计
  summary.json # 机器读的同一份数据
```

`summary.md` 包含逐次结果、每组有效样本的通过率及 95% Wilson 区间、base/code
的 Fisher 精确检验 p 值，以及失败原因和无效运行列表。默认运行结束后删除
worktree；加 `--keep-worktrees` 可保留现场。

## 注意

- 这是真实模型调用，会产生费用；先用 `--runs 1` 或 `--dry-run` 检查配置。
- 结果只对固定仓库版本、固定模型和固定 prompt 有意义。换模型或换 revision
  后需要重跑两组，不能与旧数据混在一起。
- 改名的难点是跨 crate 的同名归属，不是纯文本搜索量；脚本的保护路径和
  编译检查共同定义“正确”。
- 如果 API 返回 402（余额不足）或 401/403，脚本会停止剩余任务；已经完成的
  结果仍会写入 `summary.md`。
