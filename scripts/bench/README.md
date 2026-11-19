# Regex 编程基准

`rename_pilot.py` 在固定版本的 `rust-lang/regex` 上运行三种编程任务。每次尝试
都有独立的 worktree，两个对照组使用相同的模型、步骤预算和工具权限。

| `--task` | 任务 | 默认对照组 |
| --- | --- | --- |
| `rename`（默认） | 跨 crate 将 `regex_syntax::hir::Look` 改名，保留无关的同名类型 | `base,code` |
| `parser-reuse` | 修复同一个 AST parser 解析第二个表达式时 panic 的历史 bug | `base,context_nudge` |
| `find-iter-at` | 为 regex-lite 增加从给定字节偏移开始的匹配迭代 API | `base,context_nudge` |

`base` 使用基础工具，`code` 增加四个代码索引工具，`context` 只增加
`code_context`。`context_nudge` 在相同工具集上增加一条提示，要求先调用一次
`code_context` 再探索或编辑。可用 `--arms base,context,context_nudge` 同时比较自然
采用与提示采用。各组的模型都不带命令执行工具，验收由 runner 在模型退出后运行。
Parser 案例固定到上游修复 `5c7bf41` 的父提交，其余案例沿用原固定提交。

## 前置条件

- macOS/Linux，Python 3.11+，Git；
- `cargo` 可构建 `packages/core`；
- `rust-analyzer` 在 `PATH` 中（`code` 和 `context` 组需要）；
- 一个 OpenAI 兼容的 chat completions 端点。默认读取 Oaklet 桌面配置：
  `~/Library/Application Support/com.oaklet.desktop/config.json`，
  也可用 `--config` 或环境变量 `OAKLET_BENCH_CONFIG` 指定。

API key 不会写进 spec 文件；脚本通过 `OAKLET_BENCH_API_KEY` 传给
`agent::bench::benefit_pilot`。spec、日志和补丁都落在输出目录，默认保存在
`.data/bench/<案例>/<timestamp>/`。重命名沿用 `rename-pilot` 目录名。

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

新增用例分别冒烟，每组一次。

```bash
python3 scripts/bench/rename_pilot.py --task parser-reuse --repo /path/to/regex --runs 1 --jobs 2
python3 scripts/bench/rename_pilot.py --task find-iter-at --repo /path/to/regex --runs 1 --jobs 2
```

加 `--dry-run` 可以在无需模型配置和调用的情况下查看用例与分组。`--resume` 和
`--recheck` 会沿用原结果记录的案例。

不传 `--repo` 时会克隆 `https://github.com/rust-lang/regex`，并 checkout
所选案例的固定提交。建议显式传本地 checkout，避免网络波动。

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
编译检查或行为验收：

```bash
python3 scripts/bench/rename_pilot.py \
  --repo /path/to/rust-lang/regex \
  --cargo-home /path/to/cargo-home \
  --recheck --resume .data/bench/rename-pilot/20260101-120000
```

如果 regex 的依赖不在默认 `~/.cargo`，用 `--cargo-home` 指定。rust-analyzer 与
`cargo check` 共用该缓存，Oaklet 探针仍用原有环境构建。`code` 组在模型运行前
先确认 rust-analyzer 能找到 `regex-automata` 的跨 crate 引用。`context` 组确认
`code_context` 能返回当前案例的语义结果，两种 context 分组使用相同的预检。比较改动后的效果时
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
建议 `--jobs 4` 到 `6`。每个 `code` 或 `context` 尝试会启动一个 rust-analyzer；如果内存
紧张，降低 jobs 或先只跑 `--arms base`。

## 判分

重命名通过需满足三个条件。

1. `cargo check -p regex-syntax -p regex-automata -p regex -p regex-lite`
   通过（漏改会在这里变成 unresolved import / cannot find type）；
2. `regex-syntax/src/hir/mod.rs` 中确实出现 `pub enum LookKind`；
3. `regex-automata/src/util/look.rs`、`regex-automata/src/nfa/thompson/builder.rs`
   和 `regex-lite/` 没有被修改（这些位置有同名但无关的 `Look`）。

第 3 条用于区分“把两个同名类型一起改掉所以能编译”与“只改目标类型”。脚本
同时记录改动文件、编译错误数、`code_*`/`code_references` 调用次数、token 和
耗时。

行为用例在模型退出后从 [cases](cases) 加载 Rust 测试，放入工作区外的独立 Cargo
项目。验收测试通过且改动限于目标 crate 的源码、测试或测试注册才算通过。
目标 crate 的 `Cargo.toml` 可以新增只含 `name` 和 `path` 的 `[[test]]` 条目，路径
必须指向本 crate 的 `tests/` 目录内的文件。现有注册、依赖、features 和其他
manifest 设置须保留。Parser 测试覆盖
捕获状态重置、解析标志与出错后复用。迭代器测试覆盖绝对字节偏移、原文本边界、
空匹配推进和越界行为。

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

`summary.md` 包含逐次结果、每组有效样本的通过率及 95% Wilson 区间、对照组之间
的 Fisher 精确检验 p 值，以及失败原因和无效运行列表。报告同时给出每次成功的
token 成本和每轮的 `grep`、`read_file`、`code_context` 调用次数。另按组报告
至少调用一次 `code_context` 和至少收到一次语义结果的运行比例，预检不计入。
没有采用工具的运行仍纳入通过率和成本统计。默认运行结束后删除
worktree；加 `--keep-worktrees` 可保留现场。

## 注意

- 这是真实模型调用，会产生费用；先用 `--runs 1` 或 `--dry-run` 检查配置。
- 结果只对固定仓库版本、固定模型和固定 prompt 有意义。换模型或换 revision
  后需要重跑两组，不能与旧数据混在一起。
- 如果 API 返回 402（余额不足）或 401/403，脚本会停止剩余任务；已经完成的
  结果仍会写入 `summary.md`。
