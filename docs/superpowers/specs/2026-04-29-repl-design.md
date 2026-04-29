# REPL (Claude Code CLI mode) — Phase 9a 设计

最后更新：`2026-04-29`
状态：`spec` (未实现)
关联 Phase：9a (REPL 第一阶段)

## 背景

`dscode` 当前是单次执行 CLI：每次调用跑一个 task，4 步预算之后退出。`dscode chat` 是 placeholder，只是把 task 喂进同样的 single-shot agent loop。

Anthropic 的 `claude` CLI 是持久化 REPL：进入后保持运行，多轮对话共享上下文，每条用户消息触发完整推理 + 工具调用流，slash 命令处理本地操作（save/load/clear/cost）。

本 spec 描述把 `dscode chat` 升级为 REPL 模式 — 是 Claude Code-like 体验的第一阶段，不含流式 token 和上下箭头（v2 候选）。

## 目标

- `dscode chat` 进入持久化 REPL，提示符 `> `
- 用户消息触发 agent loop（默认 20 步预算，可调）
- 完整 chat transcript 跨轮持久（user / assistant / tool 三类 turn）
- 每轮 transcript 完整渲染进 LLM prompt（Q1=C 决议）
- 9 个 slash 命令：`/quit /help /clear /budget /skill /diff /save /load /cost`
- session 持久化为 JSON 文件 (`<workspace.session_dir>/<name>.json`)，可 `/save` 后重启 `/load` 恢复

## 非目标 (v1)

- 流式 token 输出（v2，需 SSE 解析）
- 上下箭头历史 / readline（v2，需 rustyline 或 raw mode 自实现）
- Ctrl+C 优雅中断（v2，需 signal handler + curl 超时联动）
- `/sessions` 列表 / `/load` 命令补全
- session 自动保存
- 流式 LLM 工具调用展示（每个 step 的输出在 step 完成后才打）

## 架构

### 模块边界

```
src/
├── repl/                    # 新顶级模块
│   ├── mod.rs               # pub use
│   ├── repl.rs              # Repl 主循环 + handle_line
│   ├── transcript.rs        # Turn / Transcript / render_for_prompt
│   ├── slash.rs             # try_handle_slash + 9 命令实现
│   └── session.rs           # save / load JSON I/O
├── util/json.rs             # 现有 parser + 新增 writer (M1)
├── core/loop_runtime.rs     # AgentLoop::run_with 改返回 RunResult
├── model/deepseek.rs        # respond 增加 (TokenUsage) 返回
└── cli/commands/chat.rs     # 改为 Repl::new(config, skill).run()
```

### 集成思路（已选）

**选项 3：`Repl` 类型与 `AgentLoop` 并列。** Repl 拥有 transcript / budget / slash dispatch / I/O；每轮调用一次 `AgentLoop::run_with`。AgentLoop 维持单次执行模型不变。

放弃方案：
- 把 transcript 塞进 `initial_observations` (类型错配，会让 `compact_observations` 误处理)
- 把 transcript 搬进 AgentLoop 让其状态化 (污染 single-shot 命令的语义)

### 数据契约

```rust
// src/repl/transcript.rs

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TurnRole {
    User,
    Assistant,
    Tool,
}

#[derive(Debug, Clone)]
pub struct Turn {
    pub role: TurnRole,
    pub content: String,                              // user/assistant text; empty for tool
    pub tool_name: Option<String>,                    // tool turns only
    pub tool_input: Option<BTreeMap<String, String>>, // tool turns only
    pub tool_output: Option<String>,                  // tool turns only
    pub status: ObservationStatus,                    // Ok / Failed (tool only meaningful)
}

#[derive(Debug, Clone, Default)]
pub struct Transcript {
    pub turns: Vec<Turn>,
}

impl Transcript {
    pub fn push_user(&mut self, content: impl Into<String>);
    pub fn push_assistant(&mut self, content: impl Into<String>);
    pub fn push_tool(
        &mut self,
        name: impl Into<String>,
        input: BTreeMap<String, String>,
        output: impl Into<String>,
        status: ObservationStatus,
    );
    pub fn render_for_prompt(&self) -> String;        // see §3.5
    pub fn clear(&mut self);
}
```

```rust
// src/repl/repl.rs

pub struct Repl {
    config: AppConfig,
    transcript: Transcript,
    budget: usize,                                    // default 20
    skill: Option<String>,
    tokens_prompt: u64,
    tokens_completion: u64,
}

pub enum ControlFlow {
    Continue,
    Quit,
}

impl Repl {
    pub fn new(config: AppConfig, skill: Option<String>) -> Self;
    pub fn run(&mut self) -> AppResult<()>;          // reads stdin until EOF or /quit
    pub fn handle_line(&mut self, line: &str) -> AppResult<ControlFlow>;
}
```

```rust
// src/core/loop_runtime.rs (modified)

pub struct RunResult {
    pub final_message: String,
    pub tool_turns: Vec<Turn>,
    pub usage: Option<TokenUsage>,
}

impl AgentLoop {
    pub fn run(&self, ctx: TaskContext) -> AppResult<()>;            // unchanged signature; drops RunResult
    pub fn run_with(&self, ctx: TaskContext, options: AgentLoopOptions) -> AppResult<RunResult>;
}
```

```rust
// src/model/deepseek.rs (modified)

pub struct TokenUsage {
    pub prompt: u64,
    pub completion: u64,
}

impl ModelClient for DeepSeekClient {
    fn respond(&self, input: ModelRequest) -> AppResult<(ModelResponse, Option<TokenUsage>)>;
}
```

### Slash 命令

| 命令 | 别名 | 行为 | 错误 |
|---|---|---|---|
| `/quit` | `/q` `/exit` | 退出 REPL；exit 0 | 无 |
| `/help` | `/h` `/?` | 打印 9 命令帮助 | 无 |
| `/clear` | — | 清 transcript + tokens；保 budget + skill | 无 |
| `/budget [N]` | — | 无参显示当前；带 N 设值 (1..=200) | 越界 / 非数字 → 内联错误，Continue |
| `/skill [name\|-]` | — | 无参显示；name 切换；`-` 清掉 | skill 不存在 → 错误 + 列已知 |
| `/diff` | — | spawn `git diff`，dump | 非 git repo → 错误；无改动 → "no pending changes" |
| `/save <name>` | — | 写 `<sessions>/<name>.json`；原子 | 文件名违规 → 错误 |
| `/load <name>` | — | 读取并替换当前 transcript / budget / skill / tokens | 文件不存在 / 解析失败 / version mismatch → 错误，**不破坏当前状态** |
| `/cost` | — | 打印 `prompt: <N>, completion: <N>, total: <N>` | 无 LLM 调用 → "no remote calls yet" |

#### 文件名校验
```
非空；不以 `.` 开头；不含 `/` 或 `\`；不含 `..`；无控制字符
```

#### Slash 不进 transcript / 不计 budget
所有 slash 都是 client-side 操作，不喂 LLM。`/load` 例外：会替换 transcript（视为热替换 REPL 状态）。

### JSON Schema (v1)

```json
{
  "version": 1,
  "name": "fix-pr-42",
  "saved_at": "2026-04-29T10:30:00Z",
  "skill": "pr-review",
  "budget": 20,
  "transcript": [
    { "role": "user", "content": "fix the failing test in apply_patch" },
    {
      "role": "tool",
      "name": "read_file",
      "input": { "path": "src/tools/apply_patch.rs", "max_lines": "40" },
      "output": "   1 use crate::error...",
      "status": "ok"
    },
    { "role": "assistant", "content": "Found the issue at line 73..." }
  ],
  "tokens": { "prompt": 12345, "completion": 6789 }
}
```

字段：
- `version` u32 必填；当前 1；mismatch → reject `unsupported session version: <N>`
- `name` string 必填；与文件名匹配但不强校验
- `saved_at` RFC3339 string 必填；仅展示
- `skill` string \| null 必填
- `budget` u32 必填；0..=200
- `transcript` array 必填；零长合法
- `transcript[].role` `"user"` \| `"assistant"` \| `"tool"`；其他 → reject
- `transcript[].content` user/assistant 必填；tool 可空
- `transcript[].name` `transcript[].input` `transcript[].output` `transcript[].status` 仅 tool 必填
- `tokens.prompt` `tokens.completion` u64 必填

### LLM Prompt 组装

每轮 user 输入后，Repl 调用 `Transcript::render_for_prompt()` 得到字符串，作为 `TaskContext::new(rendered, skill)`。AgentLoop 透明转发到 `build_user_prompt`（已有路径）。

渲染示例（§3.5 详细）：

```
Conversation so far:

[user 1]: fix the failing test in apply_patch

[tool] read_file({"path": "src/tools/apply_patch.rs", "max_lines": "40"}) → ok
   1 use crate::error...

[assistant 1]: Found the issue at line 73...

[user 2]: also rerun cargo test after applying the fix

(end of conversation; respond to the latest user message above)
```

### 老 turn 裁剪

| Turn role | 渲染规则 |
|---|---|
| user | 永不裁剪 |
| assistant | 最新 3 条全文；更早取首句 + `(truncated assistant turn N)` |
| tool | output 走 `summarize_for_kind(output, ObservationKind::from_tool_name(&name))` |

裁剪只发生在 `render_for_prompt`；`transcript.turns` / save 后的 JSON 始终全文。

### Token 累计

- `DeepSeekClient::respond` 解析 OpenAI `usage.prompt_tokens`/`completion_tokens` 或 Anthropic `usage.input_tokens`/`output_tokens`，返回 `Option<TokenUsage>`
- 离线 fallback 返回 None
- `AgentLoop::run_with` 在 RunResult 中累加单次的多 step usage
- `Repl` 跨轮累加 `tokens_prompt` / `tokens_completion`

### 错误分类（沿用 P3 `AppErrorKind`）

| 场景 | 分类 |
|---|---|
| 非交互 stdin（无 TTY） | `policy_denied("dscode chat requires a TTY; use `dscode run` for one-shot tasks")` |
| `/save` 文件名违规 | `policy_denied("session name cannot contain `..`/...")` |
| `/load` 文件不存在 | `app_error("session not found: <path>")` |
| `/load` JSON 解析失败 | `app_error("could not parse session: <reason>")` |
| `/load` version 不支持 | `app_error("unsupported session version: <N>")` |
| `/budget` 非数字 / 越界 | 内联打印错误 + Continue（不分类） |
| `/skill` 不存在 | 内联打印 + Continue |
| `/diff` git 报错 | `tool_failure(stderr)` |

## 流程图

```
$ dscode chat
> hello
Step 1: ...
Tool `list_files` output [listing]:
...
Step 4: <assistant final message>
> /budget 30
budget set to 30 (was 20)
> fix bar.rs to use Result
Step 1: ...
...
> /save my-session
saved → .dscode/sessions/my-session.json
> /quit

$ dscode chat
> /load my-session
loaded my-session (transcript: 2 turns, tokens: 12345 / 6789)
> /cost
prompt: 12345, completion: 6789, total: 19134
> /quit
```

## 测试策略

### 单测（无外部依赖）
- `transcript.rs`：`push_user/assistant/tool` × 3；`render_for_prompt` × 3 (含/不含/截断)
- `slash.rs`：9 命令路径 × 1 + 别名 × 1 + 未知 × 1 + 5 个 file-name 边界
- `session.rs`：序列化 round-trip；version mismatch；unknown role；缺 field
- `repl.rs`：`Repl::handle_line` 普通文本/slash 分发 (用 `Cursor` 模拟 stdin)

### 集成
- `dscode chat` 接 stdin pipe (`echo -e "hello\nbye\n" | dscode chat`)，断言 4 步 banner + 2 个 prompt
- `/save x → /load x` round-trip：保 budget/skill/transcript/tokens

### 手测
- 真 PR / 真编辑任务，跑 3 轮 conversational loop
- `dscode chat` + `DEEPSEEK_API_KEY` 跑一次 live LLM（v2 之前唯一的 live 验证机会）

## 切片：8 PR、~4 天

| PR | 工作 | 估时 | Land 条件 |
|---|---|---|---|
| M1 | `util::json` writer + `json_escape` 提升 | 0.5d | 132 测试通过；doctor 不变 |
| M2 | `Repl` / `Turn` / `Transcript` 骨架 | 0.5d | + Repl 单测；`dscode chat` 启动后 stub 错误 |
| M3 | REPL 主循环 + 普通文本路由 | 0.5d | stdin pipe 多轮跑通 |
| M4 | Slash dispatch + 5 核心命令 (`/quit /help /clear /budget /skill`) | 0.5d | 5 命令手测 + 单测 |
| M5 | Tool 回写 transcript + render_for_prompt | 0.5d | end-to-end 跨轮 tool 输出可见 |
| M6 | `/diff` `/cost` + token 累计 | 0.5d | usage 解析 3 测试 + 手测 |
| M7 | `/save` `/load` JSON 持久化 | 1d | round-trip 8 测试 + 手测 |
| M8 | `docs/repl.md` + roadmap + dogfood | 0.5d | 文档完整；3 轮真实 dogfood |

阶段化 land：
- **M1+M2**：基础设施
- **M3+M4+M5**：最小可用 chat
- **M6+M7+M8**：完整体验 + 文档

## 风险

| 风险 | 缓解 |
|---|---|
| 流式 token 输出不在 v1 → 单次 chat 体验仍是"卡住等" | 文档明示是非流式；后续 phase 加 SSE |
| 上下箭头历史无 → 不像 Claude Code | 文档明示 v2 候选；`rlwrap dscode chat` 可临时凑合 |
| Ctrl+C 中断 → 当前会让 LLM 调用阻塞到 curl 超时 | 已知 v1 限制；用户可 Ctrl+\ 强 kill |
| `/save` 直接覆盖既有文件 | v1 接受；v2 加 `--no-clobber` |
| Transcript 渲染 token 爆炸 | 复用 `summarize_for_kind` 的 per-kind 裁剪 |
| 非 TTY stdin（CI 调用 `dscode chat`） | 启动时 `IsTerminal` 检查，非 TTY → reject |
| 测试 stub stdin | `Repl::run` 接受 `BufRead` trait 而非具体 stdin；测试用 `Cursor::new(...)` |

## 待解项

无。所有交互式问题在 brainstorming 中收敛：
- transcript 全量传进 LLM (Q1)
- 默认 20 步 + `/budget` 调 (Q2)
- 9 个 slash 命令完整集 (Q3)
- 单 JSON 文件 (Q4)
- 选项 3：Repl 与 AgentLoop 并列

## 后续 (v2 候选)

- 流式 token 输出 (SSE 解析 DeepSeek 响应)
- 上下箭头历史（rustyline 或自写 raw mode）
- Ctrl+C 优雅中断（signal-hook + curl --max-time 联动）
- `/sessions` 列表 + `/load` 自动补全
- session 自动保存（每 N 轮 / 退出前）
- `/replay <name>` 把 session 当参考材料但不接管状态
- 跨命令共享 history（`dscode run` 后 `dscode chat` 拾起）
