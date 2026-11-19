# Agent 语义工具收益试跑：serde 基线

日期：2026-11-19。状态：serde 两个任务试跑完成；按描述/就绪提示/path 可选调整后跑了第二轮，再针对同名符号归属强化 `code_references` 触发后跑了第三轮。第三轮 `code_references` 3/3 被调用、`Data::Struct` 3/3 正确，但 `base` 也是 3/3，基线已在零误改。随后在 regex 跨 crate 同名任务上完成第六、七轮 12+12；修正共用 target 导致的编译校验假通过后，两轮均未出现显著差异，步骤 10 门槛仍未通过。

## 为什么换成 serde

Acorn 快照裁剪过、不可构建，重命名任务让基线直接到天花板。serde 满足三个条件：

- 纯 Rust、无外部服务，`cargo check` 可在副本中快速完成；
- trait/impl 与内部类型引用密集，可用编译结果客观判分；
- 仓库小（浅克隆 2.8 MB），适合多次隔离运行。

本次任务刻意选内部类型而不是公共 trait 方法。第一次尝试是重命名 `Visitor::visit_str`，但 `serde_json` 等依赖实现了该 trait，仓库内无法同步修改，编译必然失败；`Ctxt` 只被 `serde_derive` 内部使用，不会波及其它 crate。

## 任务一：内部类型 `Ctxt`

把 `Ctxt` 类型重命名为 `Diagnostics`。`Ctxt` 是 `serde_derive` 的诊断收集器，定义在 `serde_derive/src/internals/ctxt.rs`，通过 `serde_derive_internals/src -> serde_derive/src/internals` 的符号链接共享给 `serde_derive_internals`。

- 真值：55 处引用、7 个文件。
- 完成条件：`Ctxt` 残留 0、`Diagnostics` 55 处、`cargo check -p serde_derive -p serde_derive_internals` 通过。
- 工作区：`/private/tmp/oaklet-candidates/serde`，commit `e306b0c`；每组运行前 `git checkout -- . && git clean -fd -e target`，`target/` 保留以复用增量编译。

预期改动文件：

```
serde_derive/src/de.rs
serde_derive/src/ser.rs
serde_derive/src/internals/ast.rs
serde_derive/src/internals/attr.rs
serde_derive/src/internals/check.rs
serde_derive/src/internals/ctxt.rs
serde_derive/src/internals/mod.rs
```

## 方法

沿用上一个试跑的探针与三组设计：

| 组 | 工具集 | 说明 |
| --- | --- | --- |
| `base` | 生产基础工具 | 无索引工具 |
| `code` | 基础工具 + 4 个 `code_*` | 默认指导语，不强制 |
| `code_only` | 只保留 `code_*` 与 read/list/glob/edit | 诊断：强制使用语义工具 |

`base`/`code` 各 3 次，`code_only` 2 次。模型 `deepseek-ai/DeepSeek-V4.1-Flash`，temperature 0；rust-analyzer 1.99.1。三组都移除 `run_command`，避免 agent 自行编译；编译验证在每次运行结束后由外部执行。

提示词（base/code 完全一致）：

> Rename the `Ctxt` type to `Diagnostics` across this repository. Update the definition, the re-export, every use, import and type annotation. Do not add a compatibility alias and do not change behavior. When you are done, reply with the list of files you changed.

`code_only` 额外说明 grep 不可用。

## 结果

| 运行 | 成功 | 编译 | Ctxt 残留 | Diagnostics | 改动文件 | 缺失 | 额外 | 调用 | code_* | tokens | 秒 | ready 秒 |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| base-1 | 是 | 0 | 0 | 55 | 7 | 0 | 0 | 29 | 0 | 103,741 | 112.5 | 0.0 |
| base-2 | 否 | 0 | 55 | 0 | 0 | 7 | 0 | 15 | 0 | 39,842 | 36.8 | 0.0 |
| base-3 | 是 | 0 | 0 | 55 | 7 | 0 | 0 | 34 | 0 | 108,887 | 68.8 | 0.0 |
| code-1 | 是 | 0 | 0 | 55 | 7 | 0 | 0 | 37 | 0 | 207,926 | 115.0 | 2.4 |
| code-2 | 是 | 0 | 0 | 55 | 7 | 0 | 0 | 48 | 0 | 242,056 | 103.8 | 2.3 |
| code-3 | 是 | 0 | 0 | 55 | 7 | 0 | 0 | 15 | 0 | 77,368 | 66.0 | 2.3 |
| code_only-1 | 是 | 0 | 0 | 55 | 7 | 0 | 0 | 123 | 28 | 1,550,481 | 465.7 | 2.4 |
| code_only-2 | 是 | 0 | 0 | 55 | 7 | 0 | 0 | 115 | 41 | 1,907,356 | 370.0 | 2.3 |

| 组 | n | 成功 | tokens 均值 | 秒均值 | 调用均值 | code_* 均值 |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| base | 3 | 2/3 | 84,157 | 72.7 | 26.0 | 0.0 |
| code | 3 | 3/3 | 175,783 | 95.0 | 33.3 | 0.0 |
| code_only | 2 | 2/2 | 1,728,918 | 417.8 | 119.0 | 34.5 |

## 观察

1. **新增工具仍然零采用。** `code` 组 3 次运行没有任何 `code_*` 调用；与 Acorn 的 6 次合计为 0/9。`code` 组成功 3/3 只说明 grep 足够，不说明索引有收益。
2. **强制使用约 20 倍 token、5.7 倍耗时。** `code_only` 两次分别为 155 万和 191 万 tokens、466 秒和 370 秒，主要用于反复 `code_symbols`（22/33 次）和 `read_file`（75/56 次）。任务仍全部正确。
3. **`base` 组出现一次停滞。** base-2 读取了 `ctxt.rs` 和 `mod.rs` 后返回空回复并结束，未做任何修改（0 文件、55 处残留），39,842 tokens。这是模型/接口层的空响应，不是工具差异；把 `base` 记为 2/3 是如实的，但 n=3 不足以说明 `code` 组更可靠。
4. **选择任务要避开外部 impl。** `Visitor::visit_str` 的失败尝试说明：公共 trait 方法会被 `serde_json` 等依赖实现，仓库内重命名无法通过编译；内部类型（`Ctxt`）才是可判分的任务。
5. **编译验证很便宜。** 工作区 `target/` 保留后，`cargo check -p serde_derive -p serde_derive_internals` 约 0.4–0.5 秒；比 Acorn 快照的构建成本低得多。

## 解释

`Ctxt` 是一个名字独特、只在 7 个文件里出现的内部类型，`grep -rn '\bCtxt\b'` 一次就能拿全。语义工具的 `code_references` 能给出同一组位置，但还要先知道定义文件和精确行列；在这个任务上没有信息优势，只有额外调用成本。agent 的默认选择是理性的。

## 任务二：同名枚举变体 `Data::Struct`（grep 会漏）

### 任务

把 `serde_derive` 内部 AST 的 `Data::Struct` 变体重命名为 `Data::Record`。

这个任务专门针对文本搜索的两个缺陷：

- 变体声明是裸 `Struct`（`serde_derive/src/internals/ast.rs` 的 `enum Data`），grep `Data::Struct` 找不到声明；
- 同一段文本 `Data::Struct` 还出现在 `syn::Data` 上：`internals/receiver.rs` 直接 `use syn::Data`，第 248 行的 `Data::Struct(data)` 属于 syn；`attr.rs` 另有 8 处 `syn::Data::Struct`。文本搜索无法区分两个 `Data`，正确做法要靠导入解析或语义引用。

统计：`Data::Struct` 文本共 42 处——32 处内部引用、9 处 `syn::Data::Struct`、1 处 receiver 的裸 syn 引用；另有 1 处裸声明。`Style::Struct` 20 处必须保持不变。

完成条件：32 处 `Data::Record`、内部 `Data::Struct` 残留 0、`syn::Data::Struct` 仍为 9、receiver 的裸 `Data::Struct` 仍为 1、`Style::Struct` 仍为 20、`cargo check -p serde_derive -p serde_derive_internals` 通过。

方法沿用任务一：`base`/`code` 各 3 次、`code_only` 2 次，提示词相同；运行结束后在工作区外部编译并比对计数。

### 结果

| 运行 | 成功 | 编译 | Record | 内部残留 | syn | Style | 改动 | 缺失 | 额外 | 调用 | code_* | tokens | 秒 | ready 秒 |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| base-1 | 是 | 0 | 32 | 0 | 9 | 20 | 6 | 0 | 0 | 30 | 0 | 225,865 | 85.2 | 0.0 |
| base-2 | 是 | 0 | 32 | 0 | 9 | 20 | 6 | 0 | 0 | 63 | 0 | 604,440 | 109.9 | 0.0 |
| base-3 | 否 | 101 | 33 | 0 | 9 | 20 | 7 | 0 | 1 | 16 | 0 | 84,583 | 41.2 | 0.0 |
| code-1 | 是 | 0 | 32 | 0 | 9 | 20 | 6 | 0 | 0 | 30 | 0 | 285,274 | 79.9 | 2.3 |
| code-2 | 是 | 0 | 32 | 0 | 9 | 20 | 6 | 0 | 0 | 18 | 0 | 206,812 | 51.2 | 2.3 |
| code-3 | 是 | 0 | 32 | 0 | 9 | 20 | 6 | 0 | 0 | 70 | 0 | 631,628 | 133.5 | 2.3 |
| code_only-1 | 是 | 0 | 32 | 0 | 9 | 20 | 6 | 0 | 0 | 97 | 8 | 940,416 | 303.2 | 2.3 |
| code_only-2 | 是 | 0 | 32 | 0 | 9 | 20 | 6 | 0 | 0 | 98 | 9 | 1,463,970 | 333.6 | 2.3 |

| 组 | n | 成功 | tokens 均值 | 秒均值 | 调用均值 | code_* 均值 |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| base | 3 | 2/3 | 304,963 | 78.8 | 36.3 | 0.0 |
| code | 3 | 3/3 | 374,571 | 88.2 | 39.3 | 0.0 |
| code_only | 2 | 2/2 | 1,202,193 | 318.4 | 97.5 | 8.5 |

### 观察

1. **grep 的歧义是真实的。** base-3 把 `receiver.rs` 里 syn 的 `Data::Struct` 改成了 `Data::Record`，编译报 `no variant Record found for enum syn::Data`；这是 base 组唯一失败，失败点正是文本搜索无法区分的地方。
2. **code 组仍然零采用。** 3 次运行 `code_*` 调用为 0；3/3 成功只能说明 agent 通过读导入绕开了陷阱，不能归因于索引。
3. **强制组正确但昂贵。** `code_only` 2/2，但用了 8–9 次 `code_*`、75/68 次 `read_file`，约 94 万 / 146 万 tokens、303/334 秒，是 base 的约 4 倍 token、4 倍耗时。
4. **方差很大。** base-1 85 秒、base-2 110 秒（63 步、60 万 tokens）、base-3 41 秒即失败；code-3 也用了 70 步、63 万 tokens。n=3 下 2/3 与 3/3 的差异不能单独作为结论。

## 限制

- 两个任务都只有单语言（Rust）、每组 2–3 次；`base` 的一次空回复和一次 grep 误伤放大了方差。
- 只验证编译，没有跑 `cargo test`；重命名不改行为，编译足以判分。
- 探针不经过 Tauri 运行循环；没有压缩、重试、审批和多窗口。
- 没有测跨语言边界、缓存、恢复核对和完整生命周期。

## 结论

- 步骤 10 仍未通过。任务一（`Ctxt`）没有区分度；任务二（`Data::Struct`）`base` 2/3、`code` 3/3，但 `code` 组一次语义工具都没用，成功来自读导入而非索引，不能归因于工具。
- 与 Acorn 试跑一致：**默认 agent 不会选择这些工具；强制使用会显著变贵。** 三个数据集合计，`code` 组 12 次运行、`code_*` 调用 0 次。
- `Data::Struct` 已经证明“grep 会漏”的任务能制造真实失败，但只有当 agent 真的调用 `code_references` 时才能验证索引的收益。下一步应优先解决采用率（工具描述、就绪状态、`code_symbols` 的 path），而不是继续换任务类型。

## 复现

```bash
cd /private/tmp/oaklet-candidates/serde
git checkout -- . && git clean -fd -e target -e Cargo.lock

python3 /private/tmp/oaklet-bench/make_spec.py \
  --ws /private/tmp/oaklet-candidates/serde --group code --task serde-ctxt \
  --prompt 'Rename the `Ctxt` type to `Diagnostics` ...' \
  --spec /private/tmp/oaklet-bench/serde/specs/code-1.json \
  --out /private/tmp/oaklet-bench/serde/results/code-1.json \
  --ready-path serde_derive/src/internals/ctxt.rs --ready-line 12 --ready-column 12

cd /Users/linear/Workspace/Oaklet/packages/core
OAKLET_BENCH=/private/tmp/oaklet-bench/serde/specs/code-1.json \
  cargo test --lib --offline --locked agent::bench::benefit_pilot -- --ignored --nocapture

cd /private/tmp/oaklet-candidates/serde
CARGO_HOME=/private/tmp/oaklet-cargo cargo check -p serde_derive -p serde_derive_internals
```

原始数据：`/private/tmp/oaklet-bench/serde/results/meta.jsonl`、`results/*.json`、`logs/*.log`。

任务二（`Data::Struct`）：

```bash
cd /private/tmp/oaklet-candidates/serde
git checkout -- . && git clean -fd -e target -e Cargo.lock

python3 /private/tmp/oaklet-bench/make_spec.py \
  --ws /private/tmp/oaklet-candidates/serde --group code --task serde-data \
  --prompt 'Rename the `Struct` variant of the internal AST `Data` enum to `Record` ...' \
  --spec /private/tmp/oaklet-bench/serde-data/specs/code-1.json \
  --out /private/tmp/oaklet-bench/serde-data/results/code-1.json \
  --ready-path serde_derive/src/internals/ast.rs --ready-line 28 --ready-column 5

cd /Users/linear/Workspace/Oaklet/packages/core
OAKLET_BENCH=/private/tmp/oaklet-bench/serde-data/specs/code-1.json \
  cargo test --lib --offline --locked agent::bench::benefit_pilot -- --ignored --nocapture

cd /private/tmp/oaklet-candidates/serde
CARGO_HOME=/private/tmp/oaklet-cargo cargo check -p serde_derive -p serde_derive_internals
```

原始数据：`/private/tmp/oaklet-bench/serde-data/results/meta.jsonl`、`results/*.json`、`logs/*.log`。

## 第二轮：描述、就绪状态与 path 可选

第一轮 12 次 `code` 运行中 `code_*` 调用为 0。据此改了四处，再跑同一 `Data::Struct` 任务与 Acorn 重命名任务：

1. 工具描述与 guidance 从“能力 + 免责声明”改为场景触发（“已知符号名、需要全部引用或调用者时先用 code_*，再用 grep 补字符串/宏/配置”），并明确 `code_definition` 需要的列要先用 `code_symbols` 取得。
2. 每次请求在 system prompt 里写入索引状态：`Semantic index ready for Rust` / `starting` / `unavailable`，并按步骤刷新；索引完全不可用时不注册 `code_*`。
3. `code_symbols` 的 `query` 模式 path 可选；Service 层按项目路由并合并结果，保留各语言服务器的排序。

serde（`Data::Struct`，`base` 未改，重跑作同期对照）：

| 组 | 成功 | tokens 均值 | 秒均值 | code_symbols | code_calls | code_references |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| base | 3/3 | 347,810 | 95.5 | 0 | 0 | 0 |
| code | 3/4 | 274,310 | 81.0 | 4 | 0 | 0 |

code 组 4 次都先调 `code_symbols`（一次带 path，三次省略 path），但随后仍用 grep 完成重命名；`code-1` 仍把 `receiver.rs` 的 `syn::Data::Struct` 改掉，编译失败，与第一轮 base-3 的错误相同。调用 `code_references` 本可直接排除这一处，但 4 次都没有调用。

Acorn（`format_cny` 重命名）：

| 组 | 成功 | tokens 均值 | 秒均值 | code_* |
| --- | ---: | ---: | ---: | ---: |
| base | 3/3 | 65,211 | 23.6 | 0 |
| code | 2/2 | 63,491 | 25.1 | 4（3 次 symbols、1 次 calls） |

第三次 code 运行被手动中断，未计入。Acorn 的 code 组两次都以 `code_symbols` 开头；其中一次在重命名后额外用 `code_calls` 检查调用点。

结论：场景触发与就绪提示把 `code_*` 从“从不使用”变成“每个 code 运行至少用一次”，且不再出现 3–4 倍成本；但收益验收仍未通过——`code_references` 0 次，`Data::Struct` 的误改仍发生一次，base 6/6、code 5/6。下一项应针对“同名符号归属”的场景强化 `code_references` 的触发，而不是继续改文案。

## 第三轮：同名符号归属触发

第二轮后 `code_references` 仍为 0。第三轮只改触发，不改任务和工具能力：

1. guidance 增加一条编辑前规则：重命名或修改变体/字段/方法/类型前，先从 `code_symbols`/`code_definition` 取声明位置，调用 `code_references`，只改它返回的位置；跨文件的同名裸 token 可能绑定到另一个类型、crate 或 import。
2. `code_references` 的描述改为“改名/编辑前调用”，并说明返回结果之外的同名文本命中属于另一个符号。
3. `code_symbols` 在 query 结果出现重名时，于输出里追加 `hint`，直接指向 `code_references`；提示使用同一份 JSON，不额外花 prompt 规则。

同一 `Data::Struct` 任务、同一模型，`base` 沿用上一轮的同条件对照（3/3、347,810 tokens、95.5 秒）：

| 运行 | 成功 | code_symbols | code_references | tokens | 秒 | 步数 |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| code-1 | 是 | 4 | 1 | 1,053,977 | 161.2 | 68 |
| code-2 | 是 | 2 | 2 | 385,604 | 69.6 | 42 |
| code-3 | 是 | 1 | 1 | 246,564 | 63.6 | 36 |

三次都先搜 `Data`，读到重名提示后对 `ast.rs:28` 的 `Data` 声明调用 `code_references`，再按返回位置编辑；`receiver.rs` 的 `syn::Data::Struct` 三次都保留，`record=32`、`syn=9`、`style=20` 全部正确。

结论：

- 触发有效。`code_references` 从两轮 0 次变为 3/3 运行使用（共 4 次），误改 0/3；成本中位数与 base 相当，均值被一次 105 万 tokens 的长尾拉高到 56 万。
- 但收益验收仍不能通过：`base` 在这条任务上已经 3/3 零误改，按步骤 10 的“基线不能已是零遗漏”规则，code 组再正确也不构成通过证据。
- 要真正验收，需要一条 base 稳定失败的同类任务（更多同名绑定、或跨 crate 重名），否则只能说明工具可被触发、结果可用，不能说明它降低了真实遗漏率。

## 第四轮：跨 crate 同名任务设计

按“base 会稳定失败”的目标试了 7 种改名任务，全部在 serde 工作区、同一模型、每组 60 步上限下执行：

| 任务 | 同名冲突 | base 结果 |
| --- | --- | --- |
| `SerializeMap` 结构体 → `BufferedMap` | 同文件内与 `serde_core::ser::SerializeMap` trait 同名 | 1/1 通过（只改 4 处结构体） |
| `ast::Field` → `FieldDef` | `syn::Field`、`attr::Field`、`Expr::Field` | 1/1 通过 |
| `Data` 枚举 → `DataBody` | `syn::Data`、receiver 的裸 `Data::Struct` | 1/1 通过（receiver 未动） |
| `SerializeSeq` trait → `SerializeSequence` | 关联类型 `Serializer::SerializeSeq`、私有适配器结构体 | 1/1 通过（关联类型与结构体均未动） |
| 四个类型同时改（`ast`/`attr` 的 `Field`、`Variant`） | 同上，跨 4 个模块 | 1/1 通过 |
| `Data` 枚举 + `Struct` 变体同时改 | 同上，两处陷阱叠加 | 1/1 通过 |
| `ast::Field` 与 `attr::Field` 同时改 | `syn::Field`、`Identifier::Field`、`Expr::Field` | **1/5 通过**：base-1 漏改 `de.rs`/`check.rs`/`ser.rs` 的 `attr::Field`，编译 12 个错误；其余 4 次通过 |

最后一组（双 `Field`）在 code 组跑了 3 次：

| 组 | 成功 | code_symbols | code_references | tokens 均值 | 秒均值 |
| --- | ---: | ---: | ---: | ---: | ---: |
| base (n=5) | 4/5 | 0 | 0 | 589,667 | 175.8 |
| code (n=3) | 3/3 | 2.0 | 1.3 | 248,644 | 73.1 |

code 组两次对 `ast.rs` 和 `attr.rs` 的声明调用 `code_references`，一次直接按 grep 完成；三次都通过编译。base-1 的失败不是无法区分 `syn::Field`（它没碰 syn），而是漏掉了 `attr::Field` 的跨文件限定引用——这属于覆盖遗漏，不是 grep 归属错误。

结论：**没有做出“base 稳定失败”的任务**。serde 里这些同名冲突都能靠读 import 和限定路径解决，base 的 11 次探测只有 1 次遗漏；唯一失败的双 `Field` 任务复现率也只有 1/5，不能称为稳定。跨 crate 同名本身不构成足够强的陷阱，下一步要么换更大的多 crate 仓库（同名冲突更多、单次阅读覆盖不完），要么在受控夹具中注入不可回避的同名歧义；继续在 serde 里微调任务没有意义。

## 第五轮：regex 多 crate 仓库

换到 `rust-lang/regex`（regex、regex-automata、regex-syntax、regex-lite 等 7 个 workspace 成员）：

- 任务：把 `regex_syntax::hir::Look` 改名为 `LookKind`，保留 `regex-automata/src/util/look.rs` 里自己的 `Look`，以及 `regex-lite` 的同名类型。
- 判分：`cargo check -p regex-syntax -p regex-automata -p regex` 通过，且 `regex-automata` 的 `Look` 与 `regex-lite` 未被改。
- 同名陷阱：`Look` 在 workspace 内约 675 次出现，目标只有约 228 次；`regex-automata` 既定义自己的 `Look`，又通过 `regex_syntax::hir::Look` 使用另一个，必须靠 crate/import 归属。

结果：

| 组 | 成功 | tokens 均值 | 秒均值 | code_references |
| --- | ---: | ---: | ---: | ---: |
| base (n=8) | 5/8 | 2.19M | 504 | 0 |
| code (n=3) | 3/3 | ~2.79M | ~454 | 3/3 运行各 1 次 |

base 的 3 次失败（base-1、2、5）都只差一两处 `regex-automata` 的 `regex_syntax::hir::Look` 引用（`use` 或函数参数），编译报 unresolved import / cannot find type；另外 5 次通过。code 组每次先对 `regex-syntax` 的声明查一次引用，三次都完整改到 `regex-automata`，但成本比 base 高约 27%。

结论：样本加到 8 次后，base 失败率是 **3/8（37.5%）**，通过 5/8，仍不是“稳定失败”，而且方差极大（base-8 只用了 18 万 tokens / 84 秒就通过，base-7 用了 259 万 / 742 秒）。它验证了 `code_references` 能补齐 grep 在跨 crate 导入上的覆盖，但 37.5% 的失败率不足以支撑“索引降低遗漏率”的结论。另试过 tokio 的 `mpsc::Sender` 改名：base 3 次里 2 次失败，但 code 组因 rust-analyzer 在 240 秒内未就绪而无法开跑，暂不纳入。

## 第六轮：12+12 样本与可重复并行脚本

第五轮的 8 次样本仍不足，且数据只存在于临时目录。本轮把流程固化为仓库内脚本
[`scripts/bench/rename_pilot.py`](../../scripts/bench/rename_pilot.py)，并说明见
[`scripts/bench/README.md`](../../scripts/bench/README.md)：

- 每次尝试使用独立 `git worktree`，固定到 regex 的 `fde4946`；模型看不到其他尝试的修改。
- 探针只构建一次 `agent::bench::benefit_pilot`，之后直接启动测试二进制，避免多个
  `cargo test` 串行争用 target 锁；`--jobs` 控制并行模型数。
- 判分同时检查：四个 crate 的 `cargo check`、`regex-syntax/src/hir/mod.rs` 中确实出现
  `pub enum LookKind`、以及 `regex-automata` 自己的 `Look`（`util/look.rs`、
  `builder.rs`）和整个 `regex-lite` 未被改动。
- 任务仍是把 `regex_syntax::hir::Look` 改名为 `LookKind`。`regex-lite` 有同名但完全
  独立的私有 `Look`，改它属于越界修改，不是“漏改”，但同样会改变任务范围。

原计划每组 18 次，跑到每组 9 次时 SiliconFlow 返回 HTTP 402（余额不足），其中
每组前 6 次有效；按用户决定改为 **每组 12 次**，用 `--resume` 保留有效样本，只补跑
第 7–12 次。最终 24 次有效运行，另有一次 code-11 网络错误重跑；有效样本为
base 12 次、code 12 次。

| 组 | 有效 n | 通过 | 失败率 | 失败构成 | tokens 均值 | 中位秒数 | code_* / 次 | code_references / 次 |
| --- | ---: | ---: | ---: | --- | ---: | ---: | ---: | ---: |
| base | 12 | 7/12 | 41.7% | 越界改 `regex-lite` 3、编译漏改 1、未编辑 1 | 2,468,554 | 510.3 | 0 | 0 |
| code | 12 | 7/12 | 41.7% | 越界改 `regex-lite` 2、编译漏改 3 | 2,996,398 | 532.7 | 2.25 | 1.00 |

两组通过率的 95% Wilson 区间都是 32.0%–80.7%；双侧 Fisher 精确检验
**p = 1.0**。code 组 token 均值比 base 高约 21.4%，中位耗时基本持平（+4%）。

失败细节：

- base-01、base-03、base-06 都改了 8 个文件：除了目标的 5 个文件，还把
  `regex-lite/src/hir/mod.rs`、`parse.rs`、`nfa.rs` 里的私有 `Look` 一并改成
  `LookKind`。编译通过，但超出任务范围。
- base-07 只改了 4 个文件，漏掉 `regex-automata/src/meta/regex.rs` 的
  `use regex_syntax::hir::Look` 和 `compiler.rs` 的部分引用，`cargo check` 报 12 个错误。
- base-11 只跑了 7 步、3.5 万 tokens，最后停在“Let me get a complete file list…”，
  没有产生任何编辑。
- code-03、code-12 也把 `regex-lite` 的私有 `Look` 改掉；两次都调用了
  `code_references`，但引用结果只覆盖 `regex_syntax::hir::Look`，并不能阻止模型
  额外做一次全局同名替换。
- code-07 把类型重复替换成 `LookKindKind`，`cargo check` 失败；code-08 和 code-10
  改了 `regex-automata` 的部分文件，但仍漏掉旧 `Look` 的引用，编译失败。
- code 组 12/12 次都调用了 `code_symbols` + `code_references`（平均 2.25 次
  `code_*`、1 次 references），但仍有 3 次编译漏改。调用引用查询与正确合并引用
  范围是两件事。

按不同口径拆分：

| 口径 | base | code |
| --- | ---: | ---: |
| 编译漏改 | 1/12（8.3%） | 3/12（25.0%） |
| 越界改 `regex-lite` | 3/12（25.0%） | 2/12（16.7%） |
| 完全未编辑 | 1/12（8.3%） | 0/12 |
| 合并失败 | 5/12（41.7%） | 5/12（41.7%） |

### 编译校验缺陷与第六轮复核

第六轮最初报告 code 10/12、编译漏改为 0，这个结果是错的。检查阶段让所有
worktree 共用 `target-check`；cargo 的 dep-info 记录相对路径，第一次构建完成后，
其余 worktree 的源文件 mtime 更旧，cargo 直接判定为 fresh，没有真正编译。于是
code-07、code-08、code-10 的编译错误被当成通过。

第七轮发现后，`rename_pilot.py` 改为每次运行使用独立的
`target-check/<run id>`，并增加 `--recheck`：从已保存的补丁重建 worktree、重跑
编译检查，不再次调用模型。第六轮按此复核后，base 仍为 7/12，code 从 10/12
降为 **7/12**，`p = 1.0`；code-07/08/10 均为 `compile_error`。原 summary 备份在
`/private/tmp/oaklet-bench/rename-18-pre-recheck/`。该缺陷不影响越界和未编辑的
判分，但直接推翻了“code 编译漏改为 0”的结论。

此外，12 次 code 运行的 `code_references` 输出都带
`"semantic_ready": false` 和默认全 false 的 coverage，尽管查询返回了完整条目。
这是既有实现未给文件级实时查询填写 coverage 的缺口：定义、引用和调用虽然走实时
路径，却没有核对查询文件内容，也没有填 `file_verified`/`project_verified`。
为避免中途改变实验条件，第六轮数据保持修复前的行为；之后已单独修复为：实时
查询返回前重新 hash 查询文件，不一致返回 not_ready，成功时填写 `file_verified`，
项目快照已验证时填写 `project_verified`，`semantic_ready=true` 表示实时后端已回答。
这仍不是完整索引证明，`completeness_known` 继续为 false。

## 第七轮：coverage 修复后的 12+12

coverage 修复会改变 `code` 组看到的工具输出，因此重跑两组，不与第六轮的 code
数据混用。共 24 次有效运行；code-04 首次因网络发送错误无效，脚本按新增的
transient 规则重试后成功，所以实际发起了 25 次模型运行。

| 组 | 有效 n | 通过 | 失败率 | 失败构成 | tokens 均值 | 中位秒数 | code_* / 次 | code_references / 次 |
| --- | ---: | ---: | ---: | --- | ---: | ---: | ---: | ---: |
| base | 12 | 7/12 | 41.7% | 越界改 `regex-lite` 3、编译漏改 2、未编辑 1 | 2,635,999 | 534.9 | 0 | 0 |
| code | 12 | 9/12 | 25.0% | 越界改 `regex-lite` 2、编译漏改 1、未编辑 1 | 2,470,631 | 478.9 | 2.17 | 0.92 |

base 的 95% Wilson 区间为 32.0%–80.7%，code 为 46.8%–91.1%；双侧 Fisher
精确检验 **p = 0.667**，差异不显著。code 组 token 均值比 base 低约 6.3%、
中位耗时低约 10.5%，但均值被 code-12 的 6.8 万 tokens 提前停止拉低，不能据此
认为 code 组更省。

失败细节：

- base-04 没有产生编辑，923k tokens、413.7 秒，停在分析阶段。
- base-07、base-12 都改了 8 个文件，把 `regex-lite` 的三个同名 `Look` 一起改掉。
- base-08 只改了 2 个文件：把定义改成 `LookKind` 并改了 `translate.rs`，但漏掉
  `print.rs` 和 `regex-automata` 的引用，`cargo check` 报 19 个错误。
- base-11 改了 4 个文件，漏掉 `print.rs` 的部分引用（17 个错误），同时越界改了
  `regex-lite/src/hir/parse.rs`。
- code-04 重试后改了 8 个文件，仍然把 `regex-lite` 的三个同名 `Look` 改成
  `LookKind`；它调用了 `code_symbols` 和 `code_references`，但没有约束替换范围。
- code-11 改了 7 个文件，编译报 19 个错误，同时越界改了 `regex-lite` 的
  `parse.rs`/`nfa.rs`。
- code-12 只跑了 10 步、6.8 万 tokens、20.1 秒，查询后没有产生编辑。

按不同口径拆分：

| 口径 | base | code |
| --- | ---: | ---: |
| 编译漏改 | 2/12（16.7%） | 1/12（8.3%） |
| 越界改 `regex-lite` | 3/12（25.0%） | 2/12（16.7%） |
| 完全未编辑 | 1/12（8.3%） | 1/12（8.3%） |
| 合并失败 | 5/12（41.7%） | 3/12（25.0%） |

coverage 输出已按修复后的语义填写：11 次成功的 `code_references` 全部为
`file_verified=true`、`semantic_ready=true`，`project_verified=false`（worktree
的项目快照未标记完整）；11 次工作区级 `code_symbols` 为
`file_verified=false`（无路径）、`project_verified=true`、`semantic_ready=true`。
另有 4 次参数错误或 `not_ready` 的调用，不计入上述成功输出。

进一步检查保留下来的完整工具输出发现：`code_references` 的 `limit=200` 被
`regex-syntax` 同文件内的枚举和变体引用占满（200 条全部落在
`regex-syntax/src/hir/{mod,print,translate}.rs`），`truncated=true`，而
`regex-automata` 的跨 crate 引用一条也没有出现。模型只能回退到 grep，随后
把 `regex-lite` 的同名类型一起改掉。这不是“模型不知道有引用查询”，而是查询
结果在该任务上被截断后不再覆盖需要修改的跨 crate 文件；按当前 guidance
“只改返回位置”执行反而会漏改。下一步应先给引用查询加分页或按文件汇总，再
评估语义引用对漏改的真实作用。目前已经给定义/引用查询加入 `offset` 分页、
`total` 和 `file_counts`（按文件统计完整返回集），并把 location 结果按文件、
行列稳定排序；这属于工具修复，尚未重新跑模型，不能当作收益数据。

结论：**步骤 10 仍未通过。** 修复 coverage 后，code 组从 7/12 回到 9/12，
编译漏改从 2 次降到 1 次，但 base 仍有 7/12 直接通过，差异不显著（p=0.667）。
这条任务的主要失败模式仍是“把 `regex-lite` 的同名私有类型一起改掉”，
`code_references` 只返回目标类型的引用，对全局同名替换没有约束力。继续增加
样本只会收紧区间，不会把 base 变成稳定失败。

原始数据：第六轮 `/private/tmp/oaklet-bench/rename-18`（复核后），第七轮
`/private/tmp/oaklet-bench/rename-19`（`summary.md`、`summary.json`、`results/`、
`patches/`、`checks/`、`logs/`）。复现：

```bash
# 第七轮：coverage 修复后的全新 12+12
python3 scripts/bench/rename_pilot.py \
  --repo /private/tmp/oaklet-candidates/regex \
  --runs 12 --jobs 6 \
  --cargo-home /private/tmp/oaklet-cargo \
  --out-dir /private/tmp/oaklet-bench/rename-19

# 只重跑某次无效样本
python3 scripts/bench/rename_pilot.py \
  --repo /private/tmp/oaklet-candidates/regex \
  --runs 12 --jobs 1 \
  --cargo-home /private/tmp/oaklet-cargo \
  --resume /private/tmp/oaklet-bench/rename-19

# 不调用模型，只按补丁重跑编译检查并重写 summary
python3 scripts/bench/rename_pilot.py \
  --repo /private/tmp/oaklet-candidates/regex \
  --cargo-home /private/tmp/oaklet-cargo \
  --recheck --resume /private/tmp/oaklet-bench/rename-19
```
