# TodoTool — Phase 10a 设计

最后更新：`2026-05-01` (rev 4 — 吸收第三轮 codex review 反馈)
状态：`spec` (未实现)
关联 Phase：10a (LLM-driven planner — Claude Code 风格 todos-as-tool)

## 背景

Phase 9 系列把 `dscode` 升级成可流式 SSE 输出的本地代码 agent，但 agent loop 仍是
"LLM 选下一个工具 → dscode 执行 → 回填 observation" 的纯反应式循环。**没有显式的多
步规划阶段**。简单任务（"replace X with Y"）走得通，但项目级（"实现 sub-agent 派发
+ failure retry + 项目模板"）的 50+ 步迭代会把 LLM 拖进局部最优，反复调 list_files /
read_file 而不知道整体进度。

Claude Code 的解法是 `TodoWrite` 工具：todos 是 *工具* 不是 *阶段*，LLM 自己决定
何时建/改/完成 task list。dscode 复用同样心智 + 复用 Phase 9 的 streaming + transcript
基础设施。

## 目标

- 给 dscode 加一个 `todo_write` 工具，LLM 可主动维护 task list
- todo 数据 session-scoped：与 transcript 同生死，可 `/save`/`/load`/`/clear`
- 当前 todos 每轮注入到 user prompt 让 LLM 看见自己的进度
- system prompt 强 nudge：3+ 步任务必用 todo_write
- 渲染复用 Phase 9b 的 `paint_tool_result` —— 黄色 tool 调用行 + 绿色 ✓ + 缩进**完整** list body
- 零新依赖

## 非目标 (Phase 10a)

- Sub-agent 派发（Phase 10b）
- 跨进程 / workspace 级 todos 持久化（Phase 10c 候选）
- LLM 自我 replan / 失败自动回路（Phase 10c）
- `dscode init <template>` 项目模板（Phase 10c）
- cargo / npm 专用工具（Phase 10c）
- todos 字段拓展（notes / due / priority — YAGNI）

## 锁定的设计决策

brainstorm 八轮 Q&A 收敛：

1. **风格**：Claude Code 风格（todos 是工具不是阶段）— vs Codex（hard plan 阶段）/ Aider（双模型）
2. **工具接口**：`整体替换`，一次调用 rewrite 整个 list — vs 离散 add/update/remove ops
3. **生命周期**：`session-scoped` —— `dscode run` 任务结束消失；`dscode chat` 跨轮保留，与 transcript 同生死
4. **Schema**：三字段 `content` (imperative) / `activeForm` (present continuous) / `status` (pending|in_progress|completed)
5. **显示**：`paint_tool_result` body 自动渲染**完整** list（user 看；observation replay 看 trim 摘要）
6. **Prompt 注入**：user prompt 加 `Todos:` block（与 `Observations:` 平级）— vs system prompt 注入（破坏 prefix cache）
7. **System nudge**：强风格 5-6 句静态文本（vs 软风格被忽略 / 关键词检测脆弱）
8. **持久化**：SessionSnapshot schema bump v1 → v2，嵌入 `todos` 字段

## 术语澄清

- 本 spec **只动 `src/repl/session.rs`** —— REPL 用的 JSON SessionSnapshot
- `src/core/session.rs` 是另一套 legacy TOML snapshot（与 `dscode resume` 配合），**与本 spec 完全无关**，不动
- 文档里"session"/"SessionSnapshot" 一律指 `repl/session.rs`

## 架构

### 模块边界

```
src/
├── core/
│   ├── todos.rs                # 新：Todo / TodoList / TodoStatus 数据类型
│   ├── mod.rs                  # 改：加 pub mod todos;
│   ├── loop_runtime.rs         # 改:AgentLoop 拥有 Rc<RefCell<TodoList>>; 解耦 user-display vs observation summary
│   └── observations.rs         # 改:KIND_COUNT 7→8、kind_index、summarize_for_kind 加 Todos 紧凑摘要
├── tools/
│   ├── todo.rs                 # 新：TodoWriteTool impl Tool
│   ├── mod.rs                  # 改：加 pub mod todo;
│   └── registry.rs             # 改：default_registry_with_todos(Rc<RefCell<TodoList>>)
├── model/
│   ├── protocol.rs             # 改:ModelRequest.todos; ObservationKind::Todos
│   └── deepseek.rs             # 改:json_object_to_string_args 处理嵌套值; build_user_prompt 加 Todos block; TOOL_SPECS 加 todo_write; system prompt nudge
├── util/
│   └── json.rs                 # 改：加 pub fn json_value_to_string (writer for nested values)
└── repl/
    ├── repl.rs                 # 改:Repl 拥有 Rc<RefCell<TodoList>>; AgentLoopOptions struct literal 改为 ..Default::default(); /clear 清空
    ├── slash.rs                # 改:/todos 命令; /save/load 走 v2
    ├── session.rs              # 改:Schema v2 + v1→v2 迁移（in-memory only）
    └── transcript.rs           # 改：render_for_prompt 对 todo_write input 缩略
```

15 文件，3 新（`core/todos.rs`、`tools/todo.rs`、`docs/todos.md`）+ 12 改。

### 数据契约

#### `core/todos.rs`（新）

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TodoStatus {
    Pending,
    InProgress,
    Completed,
}

impl TodoStatus {
    /// "pending" | "in_progress" | "completed" → variant
    /// 命名为 from_label 而非 from_str 以避开 std::str::FromStr trait 命名约定
    pub fn from_label(s: &str) -> Option<Self>;

    /// variant → "pending" | "in_progress" | "completed"
    pub fn label(&self) -> &'static str;
}

#[derive(Debug, Clone)]
pub struct Todo {
    pub content: String,        // imperative form, e.g. "Run tests"
    pub active_form: String,    // present continuous, e.g. "Running tests"
    pub status: TodoStatus,
}

#[derive(Debug, Clone, Default)]
pub struct TodoList {
    pub items: Vec<Todo>,
}

impl TodoList {
    /// 用新 list 完全替换（旧 items 全部丢）
    pub fn replace(&mut self, items: Vec<Todo>);

    /// "- [pending] Run tests\n- [in_progress] Add feature\n…"
    /// 用于 user prompt 的 Todos block
    /// 空 list 返回 ""
    pub fn render_for_prompt(&self) -> String;

    /// 多行展示。**契约：第一行始终是 render_compact_summary 的输出**，后续行
    /// 是 `[status] (content|active_form)` 一条一行。
    /// 用于 paint_tool_result body（user-facing） + /todos 命令显示。
    /// in_progress 用 active_form，pending/completed 用 content。
    pub fn render_for_display(&self) -> String;

    /// 紧凑摘要单行：用于 transcript replay 的 observation summary。
    /// "5 todos: 2 completed, 1 in_progress, 2 pending"
    /// 空 list 返回 "no todos"
    pub fn render_compact_summary(&self) -> String;

    /// 给 ModelRequest 复制
    pub fn snapshot(&self) -> Vec<Todo>;

    pub fn is_empty(&self) -> bool;
}
```

**契约**: `render_for_display().lines().next() == render_compact_summary()`. 由 M1 单测 pin 住。

#### **重要：`util/json.rs::json_value_to_string` 新增**（修复 codex C1）

当前 `json_object_to_string_args`（`src/model/deepseek.rs:1078-1106`）对嵌套
`Object/Array` 直接 `app_error`。LLM **会**发 `"items": [{...}]`（字面数组），
命中错误路径，返回 `app_error` 后整轮 abort（不只是红 ✗），重试也无意义。

修法：在 `util/json.rs` 加一个递归 JSON writer：

```rust
pub fn json_value_to_string(value: &JsonValue) -> String {
    match value {
        JsonValue::Null => "null".to_string(),
        JsonValue::Bool(b) => if *b { "true" } else { "false" }.to_string(),
        JsonValue::Number(n) => n.clone(),
        JsonValue::String(s) => {
            let mut out = String::with_capacity(s.len() + 2);
            out.push('"');
            out.push_str(&json_escape(s));
            out.push('"');
            out
        }
        JsonValue::Array(items) => {
            let mut out = String::from("[");
            for (i, item) in items.iter().enumerate() {
                if i > 0 { out.push(','); }
                out.push_str(&json_value_to_string(item));
            }
            out.push(']');
            out
        }
        JsonValue::Object(map) => {
            let mut out = String::from("{");
            for (i, (k, v)) in map.iter().enumerate() {
                if i > 0 { out.push(','); }
                out.push('"');
                out.push_str(&json_escape(k));
                out.push_str("\":");
                out.push_str(&json_value_to_string(v));
            }
            out.push('}');
            out
        }
    }
}
```

然后修 `json_object_to_string_args`（在 `src/model/deepseek.rs`）：

```rust
JsonValue::Object(_) | JsonValue::Array(_) => {
    // 把嵌套结构 re-serialize 回 JSON 字符串，让 ToolInput.args 仍是 BTreeMap<String, String>
    // 但工具拿到字符串后可以二次 parse。修复 Phase 10a items 数组传输的 codex C1 阻断。
    result.insert(key.clone(), crate::util::json::json_value_to_string(value));
}
```

修复后**全部工具自动受益** —— 未来任何带嵌套参数的工具都不再被 protocol parser 卡死。
此 fix 单独安全：grep 确认 codebase 没有任何测试 pin 现在的 "scalar only" 错误。

**Round-trip 注意**：`\uXXXX` 输入解码成 UTF-8 后 writer 输出原 UTF-8（不重新编码 \u），
两者**逻辑等价**但**字节不同**。M1 测试明确这点。

#### `tools/todo.rs`（新）

```rust
pub struct TodoWriteTool {
    pub list: Rc<RefCell<TodoList>>,
}

impl Tool for TodoWriteTool {
    fn name(&self) -> &'static str { "todo_write" }

    fn execute(&self, input: ToolInput) -> AppResult<ToolOutput> {
        // INVARIANT: 此函数在 borrow_mut() 期间不会触发对其他 registry tool 的 nested
        // 调用。如 Phase 10b 的 sub-agent dispatch 改变此前提，需切换到 Cell<>+take/replace。
        //
        // 1. items_str = input.args.get("items").ok_or(tool_failure(...))?;
        //    错误信息要 educational：
        //      "todo_write expects an `items` field containing a JSON array of
        //       {content, activeForm, status} objects. The model can emit it as
        //       a literal array or a JSON-stringified one — both work."
        // 2. parse_json_value(items_str) → 必须是 JsonValue::Array
        // 3. 遍历 array，校验每项：
        //      - content: 非空 string
        //      - activeForm: 非空 string
        //      - status: 三合法值之一
        // 4. 上限：items.len() <= 100
        // 5. self.list.borrow_mut().replace(parsed)
        // 6. ToolOutput { summary: list.render_for_display() }
    }
}
```

错误分类（每条都给 LLM 可恢复的提示）：
- `items` 缺失 → `tool_failure("todo_write expects an `items` field with a JSON array of {content, activeForm, status} objects")`
- `items` 不是合法 JSON → `tool_failure("malformed todo items JSON: <detail>; expected JSON array of {content, activeForm, status}")`
- `items` 顶层不是数组 → `tool_failure("`items` must be a JSON array, got <type>")`
- 单 todo 缺字段 → `tool_failure("todo at index N missing field <name>")`
- `status` 非法值 → `tool_failure("todo at index N: status must be pending|in_progress|completed (got <value>)")`
- `>100 items` → `tool_failure("too many todos (got N, max 100)")`

**不强校验** "exactly one in_progress" — 仅 system prompt 引导，dscode 不当 LLM 老师；
渲染时多个 in_progress 会全部显示，用户能看到 LLM 走偏。

#### Tool spec for LLM（OpenAI + Anthropic schema）

`items` 在 schema 里声明为字符串（与 dscode 当前所有工具一致：所有 args 都是字符串）。
LLM 可能发字面数组（修 codex C1 后也能走通）也可能发字符串，**两条路径都被支持**。

```json
{
  "name": "todo_write",
  "description": "Replace the entire todo list with a new set of items. Use proactively for tasks with 3+ steps; mark exactly one item as in_progress at a time.",
  "parameters": {
    "type": "object",
    "properties": {
      "items": {
        "type": "string",
        "description": "JSON array of objects with fields {content: string, activeForm: string, status: \"pending\"|\"in_progress\"|\"completed\"}. content is imperative form (e.g. \"Run tests\"); activeForm is present continuous (e.g. \"Running tests\")."
      }
    },
    "required": ["items"],
    "additionalProperties": false
  }
}
```

加到 `src/model/deepseek.rs::TOOL_SPECS` 数组末尾。

#### `model/protocol.rs` 改动

```rust
pub struct ModelRequest {
    // 现有字段不变
    pub system_prompt: String,
    pub task: String,
    pub profile_name: String,
    pub profile_hints: Vec<String>,
    pub primary_file: Option<String>,
    pub suggested_test_command: Option<String>,
    pub available_tools: Vec<String>,
    pub observations: Vec<Observation>,
    // 新增
    pub todos: Vec<Todo>,
}

pub enum ObservationKind {
    FileExcerpt,
    Listing,
    SearchResults,
    Patch,
    Diff,
    ShellOutput,
    Other,
    Todos,                   // 新增第 8 个
}

impl ObservationKind {
    pub fn from_tool_name(name: &str) -> Self {
        match name {
            "todo_write" => Self::Todos,    // 新增
            // 现有映射不变
            _ => Self::Other,
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Self::Todos => "todos",          // 新增
            // 现有
        }
    }
}
```

#### `core/observations.rs` 改动（修复 codex C2）

**KIND_COUNT 必须从 7 提升到 8**（保持 module-internal 可见性，去掉之前 spec 误标的 `pub`）：

```rust
const KIND_COUNT: usize = 8;        // 7 → 8 (无 pub)

pub fn kind_index(kind: ObservationKind) -> usize {
    let index = match kind {
        ObservationKind::FileExcerpt => 0,
        ObservationKind::Listing => 1,
        ObservationKind::SearchResults => 2,
        ObservationKind::Patch => 3,
        ObservationKind::Diff => 4,
        ObservationKind::ShellOutput => 5,
        ObservationKind::Other => 6,
        ObservationKind::Todos => 7,    // 新增
    };
    debug_assert!(index < KIND_COUNT);
    index
}
```

**`summarize_for_kind` 加 Todos arm — 紧凑摘要**（修复 codex I4）。**只用于 transcript replay
时压缩 observation 体；不影响 user-facing display**：

```rust
pub fn summarize_for_kind(text: &str, kind: ObservationKind) -> String {
    match kind {
        // 现有 7 个 arm 不变
        ObservationKind::Todos => {
            // text 是 TodoList::render_for_display 的输出（首行 = compact summary）
            // 抽首行作为紧凑 observation summary，避免 transcript replay 时
            // 全文反复进 prompt（context-window 泄漏防护）
            // 契约由 render_for_display 保证：first line == render_compact_summary
            text.lines().next().unwrap_or(text).to_string()
        }
    }
}
```

**关键**：此 trim **只**喂给 observation pipeline；user 看到的仍是完整 `output.summary`，
由 `loop_runtime.rs` 解耦实现（见下）。

#### `core/loop_runtime.rs` 改动（关键解耦——修复 codex CR-1）

```rust
pub struct AgentLoopOptions {
    pub steps: usize,
    pub initial_observations: Vec<Observation>,
    pub todos: Rc<RefCell<TodoList>>,    // 新增
}

impl Default for AgentLoopOptions {
    fn default() -> Self {
        Self {
            steps: 4,
            initial_observations: Vec::new(),
            todos: Rc::new(RefCell::new(TodoList::default())),
        }
    }
}
```

`AgentLoop::run_with` 内部 Ok 分支**关键改动**——把 user 显示与 observation summary 解耦：

```rust
// 原 (rev 2 错误版本):
//   let summary = summarize_for_kind(&output.summary, kind);
//   renderer.paint_tool_result(Ok, &tool_name, kind.label(), &summary);  // user 看到 trim 版本 ❌
//   observations.push(Observation::ok(tool_name, summary));

// rev 3 正确版本：
let display_summary = output.summary.clone();                              // user 看完整
let observation_summary = summarize_for_kind(&output.summary, kind);       // observation 走 trim
renderer.paint_tool_result(Ok, &tool_name, kind.label(), &display_summary);
observations.push(Observation::ok(tool_name, observation_summary.clone()));
tool_events.push(ToolEvent {
    output: observation_summary,    // ToolEvent 也走 trim（与 observation 一致，防 transcript 泄漏）
    // ...
});
```

Err 分支同样解耦（`paint_tool_result(Failed, …, &output_text)` 完整；observation 走 trim）。

**`ToolEvent.output` 语义变更**（IM-C 显式记录）：rev 3 之前 `ToolEvent.output` 与
user-facing display 共享同一个 `summary` 变量，rev 4 后 `ToolEvent.output` 是 **trim
版本**（observation summary），与 `repl/transcript.rs::render_for_prompt` 走 LLM
context 一致。下游消费者校验：
- `repl/repl.rs:93-99` `transcript.push_tool(event.output, ...)`：trim 喂给 LLM replay，**正确**
- `cli/commands/run.rs::AgentLoop::run`：`.map(|_| ())` 丢弃 `RunResult.tool_events`，**无关**
- `cli/commands/pr.rs:53/123/169`：仅读 `result.final_message`，丢弃 `tool_events`，**无关**
- 当前没有任何代码消费完整 `output.summary` 后续路径

如未来引入"transcript export"或"agent history view"等需要原始 user-facing 输出的功能，
需要扩 `ToolEvent` 加 `display_summary: Option<String>` 字段（YAGNI 不做）。

**对其他工具无回归**：现有 6 个 `ObservationKind` 在 `summarize_for_kind` 中本来就裁剪
（shell 取尾、文件取头等）。把它们的"裁剪输出 = user 显示"改成"裁剪输出 = observation；
完整输出 = user 显示"是**普遍改进**——之前用户看到的 shell 输出也只是被 summarize 过的尾巴，
现在能看到完整尾巴。

> 注意：当前 `summarize_for_kind` 对 ShellOutput / FileExcerpt 等做了实质裁剪（取尾 N 行、
> 取头 N 行）。把 user-facing 改成 `&output.summary` 完整，会让用户看到 raw output。
> 这不是 dscode 设计上的回归——`output.summary` 本就是 ToolOutput 给用户看的字段；
> summarize 是为 LLM context 量身定制的二次压缩。Phase 10a 借这次解耦把语义理顺。

`build_system_prompt` 末尾追加常量 nudge：

```rust
const TODO_NUDGE: &str = r#"

You have access to a todo_write tool. Use it proactively when the request:
- involves three or more distinct steps,
- spans multiple files or non-trivial refactoring,
- requires running tests or shell commands as part of completion.

Each todo has fields: content (imperative, e.g. "Run tests"), activeForm (present continuous, e.g. "Running tests"), status ("pending" | "in_progress" | "completed").

Mark exactly one todo as in_progress at a time. Update the list (mark completed, add discovered tasks) before moving to the next step. Skip todo_write only for trivial single-step requests."#;
```

skill 的 `system_append` 仍按现规则插在 nudge 之前。

#### `model/deepseek.rs::build_user_prompt` 改动

在 `Available tools:` 行之后、`Observations:` block 之前注入：

```rust
if !input.todos.is_empty() {
    prompt.push_str("Todos:\n");
    for todo in &input.todos {
        prompt.push_str(&format!(
            "- [{}] {}\n",
            todo.status.label(),
            todo.content,
        ));
    }
}
```

**规则**：todos 为空时不注入 `Todos:` 段（不写 "none"）。与现有 `primary_file` /
`suggested_test_command` 同样"有就写、没就不写"模式一致。

prompt 里 todo 用 `[status] content` 简短格式（不带 `active_form`）— LLM 不需要看
active_form，那是给 UI 用的。

#### `repl/transcript.rs::render_for_prompt` 改动（修复 codex I4 / NEW-4）

当前 `transcript.render_for_prompt` 把每个 tool 调用的 `input` 字段原样 inline 进
prompt。`todo_write` 的 `input.args["items"]` 是完整 JSON 数组字符串（多 todo 时
～KB 级），每轮 replay 全文进 prompt — 严重 context-window 泄漏。

修法：在 `render_for_prompt` 渲染 tool 行时特判 `todo_write`：

```rust
let input_repr = if name == "todo_write" {
    let label = input.get("items")
        .and_then(|s| crate::util::json::parse_json_value(s).ok())
        .and_then(|v| match v { JsonValue::Array(a) => Some(format!("items=<{} todos>", a.len())), _ => None })
        .unwrap_or_else(|| "items=<malformed>".to_string());  // NEW-4: 解析失败显示 malformed 而非 0
    label
} else {
    /* 现有逻辑 */
};
```

**`<malformed>` 语义**：覆盖两种情况——(a) `parse_json_value` 解析失败（truly malformed JSON）；
(b) JSON 合法但顶层不是数组（如 `null` / `42` / `"string"`）。这两种都属于 LLM 输出有问题，
统一标签是合适 UX；上层不需要分辨原因（实际产生 tool_failure 的是 `TodoWriteTool::execute`，
有更详细错误信息）。

`trimmed_output` 也走 `summarize_for_kind`（现已对 Todos 走紧凑摘要），双层防泄漏。

#### `repl/session.rs` schema v2（修复 codex C3 + IM-3）

```rust
const SCHEMA_VERSION_LATEST: u64 = 2;

pub struct SessionSnapshot {
    pub name: String,
    pub saved_at: String,
    pub skill: Option<String>,
    pub budget: usize,
    pub transcript: Vec<TranscriptTurn>,
    pub tokens_prompt: u64,
    pub tokens_completion: u64,
    pub todos: Vec<Todo>,                // 新增
}
```

**`/save` 写**：始终写 v2（`"version": 2`），`todos` 字段始终存在（空 list 也存 `[]`）。

**`/load` 行为**（修复 codex C3 — 旧严格 check 不再适用）：

```rust
// 当前 strict check 改为分版本处理：
match version {
    1 => {
        // v1 文件：内存里注入 todos: vec![]，其他字段不变
        // 不修改原文件，下次 /save 才会升级到 v2
        // 不向用户输出警告（保持安静）
        SessionSnapshot {
            todos: vec![],
            // ... 其他字段从 v1 解析
        }
    }
    2 => {
        // v2 文件：直读所有字段
        // 缺 `todos` 字段：app_error("session v2 missing required field `todos`")
        // —— v2 schema 严格，不静默补默认值（避免 round-trip 数据丢失）
    }
    other => {
        return Err(app_error(format!(
            "unsupported session version: {other} (this dscode supports v1 and v2)"
        )));
    }
}
```

**重要语义**（明确写入测试）：
- v1 → v2 是 **opt-in upgrade**：load 不修改原文件，只在内存里补 todos
- 用户不 `/save` 就不会变成 v2 —— v1 老文件无副作用
- v1 加载 → `/save` 同名 → 文件升级为 v2（**M4 IM-3 集成测试覆盖**）
- v1 加载 → `/save` 不同名 → 原 v1 文件不变，新文件 v2
- v2 缺 `todos` 字段是错误（不是 v1）—— 因为 v2 写出来的文件必有此字段；缺了说明是 corruption / 手改坏了
- v3+ 拒绝（保留当前 state，与 v1 时同保守）

#### `repl/repl.rs` 改动（修复 codex IM-2 + NEW-3）

```rust
pub struct Repl {
    pub config: AppConfig,
    pub transcript: Transcript,
    pub budget: usize,
    pub skill: Option<String>,
    pub tokens_prompt: u64,
    pub tokens_completion: u64,
    pub todos: Rc<RefCell<TodoList>>,    // 新增
}
```

**IM-2: 现有 `AgentLoopOptions { steps, initial_observations }` struct literal**
共 **4 个调用点**，M3 PR 必须**原子**全部更新，否则编译失败：

| 文件 | 行 | 上下文 | `todos` 字段值 |
|------|----|--------|----------------|
| `src/repl/repl.rs` | 85 | `Repl::handle_line` 调 `AgentLoop::run_with` | `self.todos.clone()` (`Rc::clone`，与 Repl 共享) |
| `src/cli/commands/pr.rs` | 53 | `pr review` 一次性流 | `Rc::new(RefCell::new(TodoList::default()))` (one-shot 自建) |
| `src/cli/commands/pr.rs` | 123 | `pr fix` 一次性流 | 同上 |
| `src/cli/commands/pr.rs` | 169 | `pr patch` 一次性流 | 同上 |

`cli/commands/run.rs` **无** struct literal（用 `AgentLoop::run` → `AgentLoopOptions::default()`），无需改动。

每个站点改为：

```rust
AgentLoopOptions {
    steps: ...,
    initial_observations: ...,
    todos: <见上表>,
}
```

或用 `..AgentLoopOptions::default()` spread（仅当 todos 想用空 list 时方便）。

M3 land 条件：必须 `grep -rn "AgentLoopOptions {"` 确认 4 站点全部更新。

`Repl::handle_line` 调 `AgentLoop::run_with` 时，把 `todos.clone()`（**`Rc::clone`**：
浅复制 Rc 指针，共享内部 TodoList — 不是深复制 list 内容）传进 `AgentLoopOptions`。

`/clear` 同时重置：transcript / tokens / **todos**。budget 与 skill 不动（与现有规则一致）。

**`/load` 机制澄清**（NEW-3）：`handle_load` 在 `repl/slash.rs:185-205` 当前实现
是 `*repl = loaded;` —— **整体替换 Repl 实例**（含其 `Rc<RefCell<TodoList>>`）。
新 Rc 身份与旧的不同，但 `AgentLoop` 每次 `handle_line` 调用时都从（新的）Repl 拿
当前 Rc，所以行为正确。M4 实现保持现有 `*repl = loaded;` 模式 —— 不引入手写
`*todos.borrow_mut() = ...` 之类的赋值。

#### `repl/slash.rs::/todos`

只读检视命令，不接受参数：

```
> /todos
no todos yet

> /todos
todos (3 items, 1 in progress):
  [completed]   Read existing tools/registry.rs
  [in_progress] Adding TodoTool
  [pending]     Wire AgentLoop
```

写到 stderr（与 `> ` prompt 同 sink）。`render_for_display` 复用 TodoWriteTool 的同款渲染。

要清空 todos 走 `/clear`（同时清 transcript），不另设 `/todos clear`。

### 渲染管线（端到端，rev 3 校正）

LLM 调一次 `todo_write`，TTY 用户屏幕看到（**完整** list，不是 trim）：

```
─── step 2 ───
deepseek-v3.2 plans the work first.
🛠 todo_write(items=[{"content":"Add TodoTool",…)
✓ todo_write [todos]
  3 todos: 0 completed, 1 in_progress, 2 pending
    [in_progress] Adding TodoTool
    [pending]     Wire TodoList into AgentLoop
    [pending]     Add /todos slash command
```

参数 `items` 通过 `abbreviate_for_inline` 自动截到 80 char + `…`。`paint_tool_result(Ok, "todo_write", "todos", &output.summary)` 输出绿色 `✓` + `[todos]` 标签 + 缩进**完整** body。

非 TTY（`dscode run > out.txt`）：

```
─── step 2 ───
deepseek-v3.2 plans the work first.
> todo_write(items=[{"content":"Add TodoTool",…)
OK: todo_write [todos]
  3 todos: 0 completed, 1 in_progress, 2 pending
    [in_progress] Adding TodoTool
    [pending]     Wire TodoList into AgentLoop
    [pending]     Add /todos slash command
```

复用 Phase 9b 的 `is_terminal()` ANSI 自动降级。

`dscode run` 在多次 `todo_write` 时顺序打印多份完整 list（最后一份是真相，前面的是历史）。
非 TTY 输出文件里每段都被 step 分隔包围，可读。

### Transcript replay（context-window 防泄漏）

LLM 第二轮看到的 prompt 中，过去的 `todo_write` 调用被压缩**两层**：

1. `[tool] todo_write(input_repr) -> ok` 的 `input_repr` 是 `items=<5 todos>`（不展开 JSON）
2. observation summary 是 `5 todos: 1 completed, 1 in_progress, 3 pending`（不展开 list）

```
[tool] todo_write(items=<5 todos>) -> ok
5 todos: 1 completed, 1 in_progress, 3 pending

```

不再把完整 JSON 与完整 list 回灌进 prompt。当前 list 走 `Todos:` block 前向注入，单一信源。

### 单线程 ownership 模型

`Rc<RefCell<TodoList>>` 在四处 share：
1. `Repl` 拥有原 Rc
2. `Repl::handle_line` 调 `Rc::clone` 传给 `AgentLoopOptions.todos`
3. `AgentLoop::run_with` 把 Rc clone 传给 `default_registry_with_todos` → `TodoWriteTool.list`
4. `AgentLoop` 每步 `options.todos.borrow().snapshot()` 复制给 `ModelRequest.todos`

dscode 全同步、单线程（无 tokio、无 async fn）—— `Rc<RefCell<>>` 安全；`borrow_mut()`
都在 `TodoWriteTool::execute` 内立即释放，不跨工具调用边界。

**Phase 10b 风险预警**：未来 sub-agent 派发若让一个工具的 `execute` 内部回调 registry，
`borrow_mut()` 会在 nested 调用时 runtime panic。届时需切换为 `Cell<Vec<Todo>>` +
`take`/`replace` 模式。本 spec 不处理（YAGNI），但 `tools/todo.rs` 顶部注释明文 INVARIANT。

## 切片：5 PR、~3-4 天（rev 3 重新平衡）

| PR | 工作 | 估时 | 测试增量 | Land 条件 |
|----|------|------|----------|-----------|
| M1 | `core/todos.rs` 数据类型 + render + validate + `util/json::json_value_to_string` + `model/deepseek.rs::json_object_to_string_args` 嵌套值修复 | 0.5d | +12 | 221 → 233；零 warnings；C1 fix 单测覆盖（嵌套数组 round-trip）；NEW-1 first-line 契约 pin |
| M2 | `model/protocol.rs` ObservationKind::Todos + `core/observations.rs` KIND_COUNT 7→8 + summarize_for_kind 紧凑摘要 + `tools/todo.rs` TodoWriteTool + `tools/registry.rs::default_registry_with_todos` | 0.75d | +9 | 233 → 242；分布: tools/todo 5（成功 + 4 错误路径），protocol+observations 4（含 `kind_index(Todos)==7` 与 `KIND_COUNT==8` 一致性、`from_tool_name`/`label` 映射、`compact_observations` supersede）|
| M3 | `model/deepseek.rs` TOOL_SPECS + build_user_prompt Todos block + system prompt nudge + `core/loop_runtime.rs` AgentLoopOptions.todos + 注入 ModelRequest + **解耦 user-display vs observation summary** + `repl/repl.rs:85` + `cli/commands/pr.rs:53/123/169` 共 4 站点 struct literal 同步更新 | 1.0d | +8 | 242 → 250；CR-1 回归测试 (mock harness 验证 user-display vs observation 解耦)；tool_choice="auto" 单测；端到端 LLM 模拟 todo_write 更新 list；`grep -rn "AgentLoopOptions {"` 确认 4 站点全部更新 |
| M4 | `repl/session.rs` schema v2 + v1→v2 migration（含集成 round-trip）+ `repl/repl.rs::Repl.todos` + /clear 同步重置 + /save/load 走 v2 + `repl/transcript.rs` todo_write input 缩略（含 malformed fallback）| 0.5d | +6 | 250 → 256；v1→v2 集成 round-trip 测试 + transcript malformed fallback |
| M5 | `repl/slash.rs::/todos` 命令 + `docs/todos.md` + `docs/roadmap.md` Phase 10a 标完成 + dogfood | 0.5d | +4 | 256 → 260；slash 4 测试 + 手测 LLM 主动调 todo_write |

总：5 PR、+39 测试（221 → 260）、~3-4 天。

阶段化 land：
- **M1**：基础类型 + parser fix。LLM 此时即便被诱导调 todo_write 也能编译通过，但还没接入 prompt nudge / AgentLoop —— 隐性可用
- **M2**：tool 注册 + observation 系统接入；LLM 调 todo_write 后能 mutate list（但用户还看不到，因为 loop_runtime 还没连）
- **M3**：prompt 注入 + AgentLoop 解耦 + 用户可见。LLM 看到 nudge + 真正能用 + 用户看到完整 list
- **M4**：REPL 集成、可持久化
- **M5**：dogfood + 文档收尾

## 测试策略

### 单测（无外部依赖，+38）

`core/todos.rs` × 8（M1）：
- `TodoStatus::from_label` 三个合法值
- `TodoStatus::from_label` 非法值返 None
- `TodoStatus::label` 与 `from_label` round-trip 一致
- `TodoList::replace` 完全覆写旧 items（不追加）
- `TodoList::render_for_prompt` 输出格式 `- [status] content` per line
- `TodoList::render_for_display` 混合状态格式（in_progress 用 active_form）
- `TodoList::render_compact_summary` 计数正确（"5 todos: 2 completed, 1 in_progress, 2 pending"）
- **NEW-1 契约 pin**：`render_for_display(...).lines().next()` 字面等于 `render_compact_summary(...)`（不论 list 内容）
- `TodoList::is_empty` 状态切换

`util/json.rs::json_value_to_string` × 2（M1）：
- 嵌套 array 与 object 双向 round-trip：parse → write → parse 等价
- 字符串中含特殊字符（quote / newline / 控制字符）正确转义；UTF-8 字符（中文 / emoji）round-trip 正确（注：byte-level 与 \uXXXX 输入不同但逻辑等价）

`model/deepseek.rs::json_object_to_string_args` × 1（M1）：
- 输入嵌套数组 `{"items": [{...}]}` → `args["items"]` 是合法 JSON 字符串（codex C1 修复回归测试）

`model/protocol.rs` + `core/observations.rs` × 4（M2）：
- `ObservationKind::from_tool_name("todo_write")` → `Todos`
- `ObservationKind::Todos.label() == "todos"`
- `kind_index(Todos) == 7`，且与 `KIND_COUNT == 8` 一致（debug_assert 不触发）
- `compact_observations` 中老 `Todos` observation 被新的取代为 superseded

`tools/todo.rs` × 5（M2）：
- `execute` 成功路径（合法 items array + 整体替换）
- `execute` 失败：items 缺失
- `execute` 失败：items 非合法 JSON
- `execute` 失败：todo 缺 content / activeForm / status 任一字段
- `execute` 失败：>100 items

`model/deepseek.rs::build_user_prompt` + `core/loop_runtime.rs::build_system_prompt` × 4（M3）：
- `build_user_prompt` 空 todos → 不输出 Todos 段
- `build_user_prompt` 多状态混合 → 格式 `- [pending] X\n- [in_progress] Y\n- [completed] Z\n` exact
- `build_system_prompt` 含 nudge 关键字
- `build_system_prompt` 与 skill `system_append` 共存时 nudge 在末尾

`model/deepseek.rs` body construction × 1（M3，NEW-2）：
- `respond_remote_openai` / `respond_remote_anthropic` 的 body JSON 包含 `"tool_choice":"auto"`（防 future PR 误改）

`core/loop_runtime.rs` × 3（M3）：
- 端到端：单步循环 LLM 调 `todo_write` 后 `Rc<RefCell<TodoList>>` 状态被更新（mock ModelClient harness）
- `AgentLoopOptions::default()` 给空 TodoList
- **CR-1 回归** (复用上面同款 mock ModelClient 端到端 harness)：单次 `run_with` 后断言 (a) 用 `&mut Vec<u8>` 注入 TtyRenderer 捕获到的输出 body 含 3+ 行明细; (b) `RunResult.tool_events[0].output` 仅 1 行紧凑摘要 ——验证 user-display 与 observation summary 真正解耦

`repl/session.rs` × 5（M4）：
- v2 round-trip（写 + 读 + 校验 todos 字段）
- v1 → v2 自动 migrate：手写 v1 JSON → load → todos 为空 vec、其他字段完整
- v2 缺 todos 字段 → app_error
- v3+ 未知版本拒绝
- **IM-3 集成**：v1 文件 → load → /save 同名 → reload → 文件 version 字段为 2，todos 为 []

`repl/transcript.rs` × 1（M4）：
- `render_for_prompt` 中 `todo_write` 调用的 input：合法 items 显示 `items=<N todos>`；malformed 时显示 `items=<malformed>`（NEW-4）

`repl/slash.rs` × 4（M5）：
- `/todos` 空 list 输出 "no todos yet"
- `/todos` 含 items 输出格式正确
- `/clear` 同时重置 transcript + todos + tokens
- `/save` /  `/load` round-trip 保留 todos

**总计**：9 + 2 + 1 + 4 + 5 + 4 + 1 + 3 + 5 + 1 + 4 = **39**
**对账**：M1: 9+2+1=12；M2: 4+5=9；M3: 4+1+3=8；M4: 5+1=6；M5: 4 → 12+9+8+6+4 = **39** ✓
（M1 包含 NEW-1 的两个独立断言：`render_compact_summary` 计数正确 + `render_for_display` 首行 == `render_compact_summary` 输出）

### 集成 / 手测（M5）

设 `DEEPSEEK_API_KEY` 跑：
```bash
dscode chat
> 实现一个 Phase 10b 的 sub-agent 派发逻辑，分四步走，先列 todos 再开始
[期望:LLM 看到强 nudge + 多步请求 → 主动调 todo_write]
[屏幕看到 🛠 todo_write(items=...) → ✓ todo_write [todos] → 缩进完整 list（每条都看到）]
> /todos                    # 看见当前 list
> /save phase-10b-plan      # 落盘 v2
> /load phase-10b-plan      # 还原
> /clear                    # 清空 todos + transcript
> /todos                    # → "no todos yet"
> /quit
```

- `dscode run > out.txt`：piped 时 ANSI 自动降级，list body 仍正确缩进
- 验证 v1 老 session：手写 v1 JSON → `/load` → `/todos` 空，其他字段完整保留 → `/save` 文件升 v2
- 多模型实测 `nudge` 接受度：DeepSeek-coder / DeepSeek-v3.2 至少 1 个能在 4+ 步任务主动调 todo_write

## 错误分类

延续 P3 `AppErrorKind`：

| 场景 | 分类 |
|------|------|
| `items` 字段缺失 | `tool_failure("todo_write expects an `items` field with a JSON array of {content, activeForm, status} objects")` |
| `items` 非合法 JSON | `tool_failure("malformed todo items JSON: <detail>")` |
| `items` 顶层非数组 | `tool_failure("`items` must be a JSON array")` |
| 单 todo 缺字段 | `tool_failure("todo at index N missing field <name>")` |
| `status` 非法值 | `tool_failure("todo at index N: status must be ... (got <value>)")` |
| `>100 items` | `tool_failure("too many todos (got N, max 100)")` |
| Session schema v3+ | `app_error("unsupported session version: <N> (supports v1 and v2)")` |
| Session v2 缺 todos | `app_error("session v2 missing required field `todos`")` |
| Session 文件读写失败 | 现有 `session.rs` 错误路径不变 |

错误透过 `paint_tool_result(Failed, ...)` 红 ✗ 渲染，与其他工具失败一致。
错误信息**针对 LLM 可读性**优化（包含 hint），下一轮 LLM 能修正自己。

## 风险

| 风险 | 缓解 |
|------|------|
| LLM 不愿意调 `todo_write` 即使有强 nudge | M3 借用 Claude Code 验证过的措辞；M5 dogfood 多个模型实测；不达标时迭代 nudge 文本 |
| `Rc<RefCell<TodoList>>` 在 Phase 10b 嵌套工具调用时 runtime panic | 本 spec 不引入嵌套；`tools/todo.rs::execute` 顶部注释明文 INVARIANT；10b 启动时切换 `Cell<Vec<Todo>>` 模式 |
| schema v1 → v2 migration 破坏现有老数据 | M4 单测覆盖 + IM-3 集成 round-trip；v1 加载只在内存注入 todos: []，不动原文件 |
| LLM 发字面数组 vs 字符串 vs 类型混乱 | C1 修复后 `json_object_to_string_args` 双路径都接受 + re-serialize；educational 错误信息 |
| `items` 嵌套引号 + UTF-8 + 控制字符转义 | Phase 9b C2 修复后 `\u/\b/\f` 都正确解码；`json_value_to_string` 复用 `json_escape` |
| 强 nudge 让 LLM 在简单任务上滥用 todo_write | nudge 末尾明确 "Skip todo_write only for trivial single-step requests"；M5 dogfood 验证 |
| ToolOutput.summary 极长（100 todos）影响 transcript context | `summarize_for_kind(Todos)` 紧凑摘要；`transcript.render_for_prompt` 缩略 todo_write input；compact_observations 自动 supersede 老 Todos observation；user 看完整 (M3 解耦) |
| LLM 把 active_form 写错时态 | active_form 是纯 cosmetic（never 注入回 prompt）；UI 偶尔 awkward 但不影响 agent 行为；不强校验 |
| LLM 标多个 in_progress 同时 | 不强校验；`render_for_display` 全部显示，用户能看到 LLM 走偏 |
| `tool_choice: "auto"` 不能保证 LLM 调 todo_write | 现有设计；用户可 `/clear` 重述；spec 明确"by design no enforcement"；NEW-2 单测 pin "auto" 字面值 |

## 待解项

无。所有 critical / important / new / notes review 反馈已吸收（rev 3）。

## 后续 (Phase 10b/10c 候选)

明确为 10a 之外，不写入此 spec：
- **Phase 10b**: Sub-agent 派发（让 LLM 通过 `dispatch_subagent` 工具派子 agent；切换 `Cell<>` ownership 模型）
- **Phase 10c**: 跨进程 todos 持久化、`/todos pin` workspace 锁、`dscode init <template>` 项目模板、cargo / npm 专用工具、LLM 自我 replan 失败回路
- TodoTool 字段拓展（notes / due / priority — YAGNI 不做）
