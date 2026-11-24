# Oaklet Tree-sitter 实施计划

首版交付面向 Agent 的单文件代码大纲。Agent 确定目标文件后，调用 `file_outline` 找到函数、方法或类型的行范围，再用现有的 `read_file` 读取相关实现。先支持已经内置语法解析器的 Rust、JavaScript、JSX、TypeScript 和 TSX，按需解析工作区中已保存的文件。验证定位效果后，再把同一套符号提取用到 Git Diff 和 Files。

Tree-sitter 提供声明、调用和源码位置。跨文件定义解析、完整引用查找和重命名由语言服务承担。结构化搜索发现的 IPC 调用、注册和实现位置可以为语义分析提供候选证据。 [Tree-sitter 代码导航](https://tree-sitter.github.io/tree-sitter/4-code-navigation.html)

## 1. 交付 Agent 单文件大纲

1. **抽出共享解析入口。** 把扩展名与 grammar 的映射、parser 初始化移到 Rust core 的 `syntax` 模块。模块输入路径和源码，由调用方决定源码来自工作区还是 Git。现有注释检查使用同一入口，沿用原有的注释筛选、写入拒绝和错误报告。 [Core 入口](../packages/core/src/lib.rs) · [注释检查](../packages/core/src/tools/comments.rs)
2. **实现声明提取。** 用各 grammar 随包提供的 `TAGS_QUERY` 起步，只取声明捕获，按首版范围补齐或去重。Rust 覆盖函数、`impl` 方法、结构体、枚举、trait、类型别名和模块；JS/TS 覆盖函数、赋给变量的箭头函数、类、方法、接口和类型别名。为方法和嵌套声明保留所属容器，便于区分同名符号。源码存在解析错误时，保留可识别的声明并标明结果可能不完整。
3. **注册 `file_outline`。** 参数为必填的 `path`。工具声明为只读，经现有注册表提供给普通与只读规划回合，同步更新注册表的工具名清单测试。路径用 `Workspace::resolve` 校验，读取已保存的 UTF-8 普通文件，沿用文件工具的 2,000,000 字节上限。解析在阻塞任务中执行，接入现有工具取消与执行期限。工具说明引导 Agent 在需要定位文件内部实现时使用大纲，随后按行读取源码。 [工具注册表](../packages/core/src/tools/mod.rs) · [工作区路径校验](../packages/core/src/tools/workspace.rs) · [文件工具](../packages/core/src/tools/fs.rs)

### 首版接口

调用 `file_outline({"path":"packages/core/src/tools/fs.rs"})`。工具返回工作区相对路径和紧凑的符号列表，每行包含符号种类、名称、所属容器及 1-based 的闭区间 `start_line–end_line`。条目按源码顺序排序，起始行对应声明，结束行包含声明的最后一行。Agent 用 `offset = start_line` 和 `limit = end_line - start_line + 1` 调用 `read_file`。

单次最多返回 200 条，并将总输出控制在现有工具输出截断阈值以内。达到任一上限时明确标记截断。支持的文件中没有声明时返回空大纲，存在语法错误时带上解析状态。遇到不支持的扩展名、非 UTF-8、非普通文件、超限文件或越界路径时返回明确原因。Agent 可用 `grep` 或 `read_file` 继续定位可读文件。首版按次计算，无需在打开工作区时扫描仓库或写入索引。

### 首版验收

| 场景 | 通过条件 |
| --- | --- |
| Rust 同名方法与嵌套声明 | 返回正确的所属容器，按给出的行范围能读到目标声明和完整方法体。 |
| JS、JSX、TS、TSX | 用 JS 导出函数、JSX/TSX 组件和 TS 类型声明验证范围。字符串、注释和函数调用均不进入大纲。 |
| 不完整语法与行号 | 文件中有语法错误时，可识别的声明仍可用。覆盖 CRLF、Unicode、无末尾换行与声明结束位置，确保行范围和 `read_file` 对齐。 |
| 路径与输出边界 | 越界路径和指向工作区外的符号链接被拒绝。空文件、无声明和输出截断有可区分的结果。 |
| Agent 工具可用性 | 普通和只读规划回合都能通过工具注册表实际调用大纲。现有写入工具的注释检查通过回归测试。 |

用同一模型和相同提示，对照两组工具配置。任务可选「找出补丁工具在哪个阶段拒绝新增注释」和「找到 Files 编辑器把改动交回上层的位置」。每组记录定位是否正确、工具往返次数、读取的源码行数和工具结果字符数。对照结果应显示大纲在定位未知名称的实现时能减少无关源码读取，且定位正确率至少持平。另用已知符号名验证直接 `grep` 的路径。

## 2. 给 Git Diff 增加符号导航

- 先接入 Source control 的 Changes。用 diff 对应的两份完整源码提取符号。按实际增删行映射到范围最小的声明，新增行用新版本，删除行用旧版本。同一 hunk 触及多个声明时列出实际命中的符号。保留现有 unified diff 与分页行为。 [Git 命令](../packages/core/src/git.rs) · [Diff 解析](../packages/desktop/src/features/source-control/git-diff.ts) · [Source control 数据流](../packages/desktop/src/features/source-control/useSourceControl.ts)
- 用既有的 diff 基线处理 `HEAD`、工作树新文件及暂存版本。删除和重命名分别使用对应版本的路径。遇到二进制文件、缺失版本、超限或工作树在生成 diff 后发生变化时，显示原始 diff。先验证新增、删除、重命名、仅暂存内容和单段跨多个符号，再复用到 Commits，提交视图使用被比较提交及其第一父提交的源码。

## 3. 给 Files 增加大纲跳转

- 在打开的代码文件旁提供按需展开的符号列表，点击条目让 CodeMirror 滚动并定位到声明起始行。通过随窗口校验的 Tauri 命令调用 Rust 提取器。大纲基于已打开的保存快照，打开、重载和保存后按需刷新。 [文件命令](../packages/core/src/files.rs) · [Tauri 命令注册](../packages/core/src/lib.rs) · [Files 页面](../packages/desktop/src/features/files/Files.tsx) · [编辑器](../packages/desktop/src/features/files/FileEditor.tsx)
- 请求与工作区、文件路径及内容版本绑定。切换文件或保存后，迟到的结果不能覆盖当前大纲。存在未保存编辑时提示先保存再跳转，避免旧行号指向错误位置。验收覆盖点击和键盘跳转、快速切换文件、保存后行号更新，以及不支持语言时的正常编辑流程。

## 4. 增加结构化搜索

- 先提供少量内置查询，例如 `invoke(...)` 调用、带 `#[tauri::command]` 的定义以及带 `#[test]` 的测试。输入包含查询类型及可选目录或 glob，返回路径、行号和匹配类型。结果按工作区路径和行号稳定排序。 [Tree-sitter 查询文档](https://tree-sitter.github.io/tree-sitter/using-parsers/queries/index.html)
- 复用 `grep` 的文件枚举、忽略规则、取消和扫描预算，按需解析命中的候选文件，到达预算时标记结果不完整。以 Oaklet 和一个独立工程验证命中，覆盖注释、字符串、忽略目录和取消。 [搜索实现](../packages/core/src/tools/search.rs)

## 5. 给 Agent 编辑结果加语法提示

- 在 `write_file`、`edit_file` 和 `apply_patch` 路径比较编辑前后的 `ERROR` 与 `MISSING` 节点。将旧诊断位置映射到新版本，再提示与本次变更相交的新增诊断。新文件与空基线比较。复用注释检查已生成的语法树，避免同一版本重复解析。 [语法错误节点](https://tree-sitter.github.io/tree-sitter/using-parsers/queries/1-syntax.html) · [补丁规划](../packages/core/src/tools/patch/plan.rs)
- 写入成功时随工具结果返回疑似错误的位置。验收覆盖新引入的缺括号、已有错误仅因插入行而移位，以及一次补丁修改多个文件。记录误报后再评估哪些诊断适合拦截。

## 验证命令与交付顺序

第 1 步可独立交付，包含共享解析、工具注册、验证记录和 README 工具说明。第 2 至 5 步以第 1 步为基础，可以各自开发和验收。每一步单独评审。

```sh
pnpm --dir packages/architecture-helper build
(cd packages/core && cargo test && cargo clippy --all-targets)
```

涉及 Git Diff 或 Files 的桌面改动时，追加前端检查并在 macOS 实际点选跳转。

```sh
pnpm --dir packages/desktop test
pnpm --dir packages/desktop build
```

## 实施验证记录

- Rust 全量测试 422 项通过，Clippy 通过。用临时 Git 仓库验证了重命名、删除、未跟踪文件、仅暂存版本、历史提交和过期 diff。编辑工具测试覆盖新增语法错误、旧错误移位和补丁最终状态。
- 桌面测试 146 项通过，生产构建成功。使用模拟 Tauri IPC 的浏览器冒烟测试验证了大纲按需加载、跳转、保存后刷新、文件切换，以及 Changes 和 Commits 的符号导航。
- 原生 WebView 实机验收与同模型收益对照仍待验证。
