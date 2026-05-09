# Roadmap 与状态

最后更新：`2026-05-07`

## 当前状态

`DeepseekCode` 已经从纯设计文档阶段进入到“可运行的本地 agent 原型”阶段。

当前代码具备这些基础能力：

- Rust CLI 骨架可运行
- 本地 `skill` / `profile` 可加载
- 可执行基础工具：
  - `list_files`
  - `read_file`
  - `search_text`
  - `apply_patch`
  - `run_shell`
  - `git_diff`
- 可运行离线 planner loop
- DeepSeek 远端传输层已接入：
  - `OpenAI-compatible` 路径支持正式 `tools` / function-calling
  - `Anthropic-compatible` 路径支持正式 `tool_use` content block，输入支持字符串与数值
- 工具执行受策略约束：
  - `allowed_tools`
  - `shell_allowlist`
  - 写入/命令审批：交互式 TTY prompt（非 TTY 默认拒绝），env 可放行
  - 错误分类 `PolicyDenied` / `ToolFailure` / `Other`
- 本机可验证性已具备：
  - `doctor` 输出 workspace / model / api key / network / hints 五段
  - `smoke` 可对 OpenAI 与 Anthropic 兼容路径单独发起最小远端请求

## 已完成

### 基础工程

- 初始化 git 仓库与项目文档
- 建立 Rust 项目目录结构
- 建立 `cli / core / model / tools / language / skills / config / ui` 模块边界
- 在无外网依赖场景下保持工程可离线构建和测试

### CLI 与配置

说明：当前用户入口以 `deepseek` 为主；历史 phase 记录中仍可能保留旧命令名 `dscode`。

- 支持：
  - `deepseek`
  - `dscode`
  - `deepseek "task"`
  - `deepseek run "task"`
  - `deepseek benchmark --manifest <file> --out <report>`
  - `deepseek dogfood run "task"`
  - `deepseek dogfood report --out <report>`
  - `deepseek diff`
  - `deepseek resume`
  - `deepseek config`
  - `deepseek doctor`
  - `deepseek smoke`（支持 `--flavor openai|anthropic` 与 `--prompt`）
- 支持 `--skill`
- 支持简单 `.dscode/config.toml` 配置读取：
  - `model.base_url`
  - `model.model`
  - `model.api_key_env`
  - `approval.require_write_confirmation`
  - `approval.require_shell_confirmation`
  - `workspace.config_dir`
  - `workspace.session_dir`

### Tooling

- `list_files`
  - 目录扫描
  - 深度和数量限制
  - 常见目录跳过
- `read_file`
  - 读取文本文件并返回带行号片段
- `search_text`
  - 仓库全文搜索
  - 结果数量限制
- `run_shell`
  - 本地受控执行
  - 内建安全前缀限制
- `git_diff`
  - 查看工作区 diff
- `apply_patch`
  - 文本替换模式
  - unified diff patch 模式（多文件）
  - patch dry-run 校验
  - patch 头路径归一化（支持 git `a/` `b/` 前缀与 `/dev/null`）
  - cwd 路径范围校验（拒绝 `..` 逃逸与 cwd 外绝对路径）
  - 失败诊断分类（缺文件 / hunk 失败 / 已应用 / 格式错误）
  - 成功摘要按 modified / created / deleted / renamed 分项列出

### Planner / Runtime

- 已实现本地离线 planner loop：
  - 组装任务上下文
  - 基于 profile 和 observations 决策下一步
  - 执行工具
  - 回填 observation
  - 最终结束
- 已支持简单编辑任务：
  - `replace "a" with "b" in path`
  - 默认走 patch 模式（构造 unified diff，含正确 `cwd` 与基名）
  - patch 不可构造时（多匹配 / 多行 / 缺文件）回退到文本替换
- 已支持 skill 提示增强
- 已支持 session snapshot 保存
- Observation 已分类（file_excerpt / listing / search_results / patch / diff / shell_output / other）
  - 按类裁剪：shell 取尾部、文件/搜索/列表取头部、diff 保留 hunk header
  - 同类型只保留最新内容，旧观察被替换为 `(superseded ...)` 桩，降低上下文污染

### Skill / Policy

- 本地 `skills/*.toml` 可解析：
  - `name`
  - `description`
  - `allowed_tools`
  - `system_append`
  - `suggested_steps`
  - `policy.require_write_confirmation`
  - `policy.require_shell_confirmation`
  - `policy.shell_allowlist`
- `allowed_tools` 已接入工具可见性控制
- `policy` 已接入执行约束
- 环境变量支持：
  - `DSCODE_AUTO_APPROVE_WRITES=1`
  - `DSCODE_AUTO_APPROVE_SHELL=1`

### DeepSeek 远端传输

- 已接入 `DEEPSEEK_API_KEY` 检测
- 远端调用失败时自动回退离线 planner
- `OpenAI-compatible` 路径：
  - `/chat/completions`
  - 正式 `tools` / function-calling
- `Anthropic-compatible` 路径：
  - `/messages`
  - 正式 `tools` 数组 + `tool_use` content block 解析，输入接受字符串/数值/布尔/null

## 当前限制

这些是已经明确存在、但还未完成的部分：

- 当前执行环境无法直接验证外网访问
  - 真实 DeepSeek 在线调用代码已接好，但未在当前会话里做 live API 验证
- `apply_patch` 多文件 + 路径范围 + 失败诊断已落地
- planner 编辑路径默认走 patch 模式，不可构造时回退到文本替换
- 工具失败已转为观察项，agent loop 不再因错误退出
- patch 应用后自动 git_diff 复核（仅在成功时触发）
- patch 模式失败可单次回退到文本替换重试
- 失败重试策略仍较窄（仅 apply_patch 单次 patch→text 回退）
  - 其他工具失败后只是被记录为观察项并继续走启发式

## 已验证

这些能力已经在当前本地环境里验证过：

- `cargo check --offline`
- `cargo test --offline`（198 项单测全部通过）
- `cargo run --offline -- doctor` 输出五段诊断（workspace / model / api key / network / hints）
- `cargo run --offline -- smoke` 与 `cargo run --offline -- smoke --flavor anthropic` 在缺少 key 时给出预检失败
- `cargo run --offline -- "inspect repository"`
- `cargo run --offline -- run --skill fix-tests "fix tests"`
- 文本替换式编辑在放行写入审批时可执行
- unified diff patch 的基础单测已通过

这些能力已经接入代码，但尚未在当前会话里完成在线验证：

- DeepSeek `OpenAI-compatible` 真实远端 tool-calling
- DeepSeek `Anthropic-compatible` 真实远端调用
- GitHub 远端创建与推送

原因：

- 当前执行环境的外网访问受限，无法直接访问 `api.deepseek.com` 或 `api.github.com`

## 近期计划

### P0: 本机可验证性

这部分优先级最高，直接决定这个项目能不能在真实环境里快速试用。

- 扩展 `doctor`：已完成
  - 检查 `DEEPSEEK_API_KEY` 是否存在并以掩码显示
  - 显示当前 `base_url` 模式（OpenAI vs Anthropic 兼容）与对应 endpoint
  - 提示 OpenAI / Anthropic 两条路径
  - 通过 curl HEAD 探测联网前置条件
- 增加 `smoke` 命令：已完成
  - 单次最小远端调用，输出 http_status / duration_ms / 助手回复（截断）
  - 通过 `--flavor openai|anthropic` 单独验证两条路径
  - 缺 key、curl 缺失、非 2xx 都给出明确错误
- 提供 `.dscode/config.toml` 示例文件：已完成
  - `.dscode/config.example.toml` 含完整 key 注释与两种 base_url 模式说明
  - 通过 `cp .dscode/config.example.toml .dscode/config.toml && deepseek doctor` 验证可解析

### P1: 真实编辑能力增强：已完成

- 扩展 `apply_patch`：已完成
  - 失败诊断分类（缺文件 / hunk 失败 / 已应用 / 格式错误）
  - 多文件 patch 支持，成功摘要按 modified / created / deleted / renamed 分项
  - cwd 路径范围限制（拒绝 `..` 逃逸与 cwd 外绝对路径）
  - 支持 git 风格 `a/` `b/` 前缀与 `/dev/null` 创建/删除标记
- 让 planner 能生成 patch 模式编辑：已完成
  - 离线 planner 通过 `build_single_line_diff` 构造单行 unified diff，附正确 `cwd` 与基名
  - patch 不可构造（多匹配 / 多行 / 缺文件）时回退到文本替换
- 在 patch 应用后自动查看 diff、必要时继续修复：已完成基础版
  - `Observation` 增加 `Ok` / `Failed` 状态，agent loop 把工具异常转为观察项而非中断
  - `git_diff` 复核仅在 `apply_patch` 成功后才触发
  - patch 模式失败 + 同一编辑可文本替换时单次回退重试

### P2: Anthropic 兼容路径补齐：已完成

- ~~将 Anthropic 路径从 JSON plan 回退升级为正式 tool use~~（已完成）
- ~~对齐 OpenAI-compatible 路径的能力边界~~（已完成；同一组工具描述同时下发）
- ~~统一远端结果到同一个 `ModelAction` 抽象~~（已完成；两条路径都返回 `ModelAction::CallTool` / `Finish`）

### P3: 审批与执行体验：已完成基础版

- ~~增加真正的审批交互~~（已完成）
  - `apply_patch` 写入前在 stderr 输出 `Apply patch in <path>? [y/N]:` 并读 stdin
  - `run_shell` 执行前输出 `Run shell command in <cwd>: \`<command>\`? [y/N]:`
  - 非 TTY 默认拒绝（安全 fallback）；env `DSCODE_AUTO_APPROVE_*=1` 仍可一次性放行
- ~~区分”策略拒绝”和”工具失败”~~（已完成）
  - `AppErrorKind` 枚举：`Other / PolicyDenied / ToolFailure`
  - `policy_denied()` / `tool_failure()` 构造器
  - agent loop renderer 输出 `✓/✗/⊘ name [observation_kind]`（TTY）或 `OK:/ERR:/DENIED: name [observation_kind]`（非 TTY），区分 PolicyDenied 与 ToolFailure
- 更清晰的错误输出：进行中（DENIED 与 FAILED 区分已落地）

### P4: 上下文与稳定性：已完成基础版

- ~~更细的 observation 类型划分~~（已完成，含 file_excerpt / listing / search_results / patch / diff / shell_output / other 七类）
- ~~更稳定的摘要与裁剪策略~~（已完成；shell 尾部 / 文件头部 / diff 保留 hunk header）
- ~~降低大输出反复回填造成的上下文污染~~（已完成；同类只保留最新观察，旧的转为 superseded 桩）

## 完整 Roadmap

### Phase 0: 项目打底

- 项目定位和范围确定
- 架构分层
- 文档建立

状态：已完成

### Phase 1: Rust CLI 骨架

- CLI 命令入口
- 配置和 session 基础设施
- 模块边界和目录结构

状态：已完成

### Phase 2: 本地工具闭环

- 文件读取
- 搜索
- patch
- shell
- diff

状态：已完成基础版

### Phase 3: 离线 planner loop

- 任务输入
- observation 回填
- 工具调用循环
- skill 提示增强

状态：已完成基础版

### Phase 4: DeepSeek 远端能力

- 远端调用接入
- OpenAI-compatible path
- Anthropic-compatible path
- 回退机制

状态：

- OpenAI-compatible：已完成基础版，已接正式 tool-calling
- Anthropic-compatible：已完成基础版，已接正式 `tool_use` content block

### Phase 5: 执行策略与安全

- `allowed_tools`
- `shell_allowlist`
- 写入/命令审批

状态：已完成基础版

### Phase 6: 体验打磨

- doctor：已完成扩展版（workspace / model / api key / network / hints / github 六段输出）
- smoke：已完成（OpenAI 与 Anthropic 兼容路径独立验证）
- diff 展示：基础版已具备
- 更好的报错：已完成基础版
  - `AppError` 增加 `hint` 与 `source` 字段
  - 关键失败模式自动附 actionable 提示（gh auth / branch mismatch / non-TTY 拒绝 / 等 9 类）
  - 因果链通过 `<dyn Error>::source()` 暴露给下游
- 更好的上下文摘要：已完成基础版
  - superseded 观察保留首行 + 行数（截 80 字符）而非通用桩，给 planner 留弱信号

状态：基础版完成

### Phase 7: 更强编辑能力

- 多文件 patch：已完成（含路径范围校验与失败分类）
- 更稳定的 edit-retry loop：已完成基础版（apply_patch patch→text 单次回退；工具异常变观察项）
- 更像真实 code agent 的最小步编辑策略：已完成基础版
  - `build_single_line_diff` 直接成功时 planner 跳过 list_files / read_file
  - 简单 `replace "X" with "Y" in path` 任务 4 步压到 2 步（apply_patch + git_diff）

状态：基础版完成

### Phase 8: 高级能力

- PR/CI 集成（v1，已完成基础版）
  - `deepseek pr review <pr>` —— 只读 review，输出 markdown 到 stdout / 文件 / `gh pr comment`
  - `deepseek pr fix <pr>` —— 抓首个失败 CI job，本地复现并迭代修复（12 步预算）
  - `deepseek pr patch <pr>` —— 提改动到工作区；`--commit` 在干净工作区时自动 commit（不 push）
  - 三命令共享 `gh auth` 检查、PR 上下文获取、prefilled observations 注入
  - 所有写入与 shell 仍走 P3 confirm
  - `deepseek doctor` 新增 `[github]` 段，显示 `gh` 版本与 auth 状态
- 更强语言特化：未开始
- IDE 集成：未开始
- 多 agent：未开始

状态：进行中（PR/CI 一项基础版完成）

### Phase 9: 交互式体验

- REPL (`deepseek` / `deepseek chat`)：v1 已完成
  - 持久化 stdin 循环 + `> ` 提示符 + TTY 守门
  - 跨轮 transcript 完整传给 LLM (user / assistant / tool 三类 turn)
  - 9 个 slash 命令：`/quit /help /clear /budget /skill /diff /save /load /cost`
  - 默认 20 步预算，`/budget N` 可调 (1..200)
  - JSON 单文件 session 持久化（`/save` 原子写入；`/load` 严格校验）
  - Token usage 累计（OpenAI / Anthropic 兼容路径）
  - 老 turn 裁剪：assistant 保留最新 3 条全文；tool 输出走 `summarize_for_kind`
- v2 已完成：流式 token 输出（DeepSeek SSE，2026-04-30）
  - `util::sse::read_frame` 通用 SSE 框解析器
  - `StreamEvents` trait + `TtyRenderer`（cyan / yellow / green / red ANSI conditional on TTY）
  - `ModelClient::respond` 接收 `&mut dyn StreamEvents`
  - DeepSeek 流式 OpenAI + Anthropic 双协议（curl `-N`）
  - 离线 planner 也走 `StreamEvents`，颜色一致
  - 175 → 198 测试，0 新依赖
- v3 候选（未开始）：
  - 上下箭头历史（rustyline 或自写 raw mode）
  - Ctrl+C 优雅中断
  - 自动保存 / `/sessions` 列表

状态：v2 完成（streaming SSE）

### Phase 10a — TodoTool

- `todo_write` 工具：Claude Code 风格 task list；LLM 主动维护，跨 REPL 轮持久
- `Todos:` 块每轮注入 user prompt（`render_for_prompt`）
- 强 nudge 注入 system prompt（3+ 步任务必用、`in_progress` 唯一性、active_form 用于 in_progress 渲染）
- session schema v1 → v2，自动迁移；v1 加载到空 todos，下次 `/save` 升级为 v2
- `/todos` slash 命令读检视当前列表
- transcript elision：旧的 `todos` ObservationKind 同类只保留最新
- CR-1 解耦：user 看完整 list（`output.summary`），observation/transcript 走 trim（防 context 泄漏）
- AgentLoop 增 `run_with_client<C: ModelClient>` 注入 seam（regression test 验证 CR-1）
- 221 → 264 tests，0 新依赖

状态：已完成（2026-05-01）

### Phase 10c — Agent loop 实用性补强（dogfood 驱动）

`dscode run` 多 agent dashboard dogfood（2026-05-02）暴露 4 类需求：

**已完成：**

- **10c-1 (`feat/todo-tool` merged)** — `recent_steps` replay：`AgentLoop::run_with_client` 把最近 3 步 assistant message 注入下一轮 `ModelRequest`，`build_user_prompt` 渲染 "Recent agent steps" block。补齐 `dscode run` 与 REPL transcript 的能力差。+2 tests。
- **10c-1 周边** — `dscode run --budget N` flag（与 REPL `/budget` 对齐 1..=200）；`run_shell` allowlist 扩 `curl/wget/gh/mkdir/cat/echo/head/tail`（agentic 调研工作流）。
- **10c-2 (`feat/loop-progress`)** — repeat-call detection：滑窗 3 步内同 `(tool_name, args)` 指纹，第 2 次执行后 observation summary 追加 `[stuck-warning]`，第 3 次直接短路返 `tool_failure(repeated identical tool call detected)`。+3 tests, 269 total。
  - dogfood 实测（2026-05-03 retry research）：机制完全工作，stuck-warning 正确触发，第 3 次正确短路。**但暴露下一层问题**：v4-pro 写"Let me start fresh with actual research"却继续做 mkdir/todo_write 振荡（ABAB 模式绕过 fingerprint 检测），**从未调用 gh/curl**。LLM planning 短板，不是机制问题。

**已完成：**

- **10c-3 (`main`, 2026-05-05)** — Empty workspace bootstrap：当 `workspace` 为空、task 命中 research 关键词、且 `run_shell` 可用时，agent loop 注入强制 research bootstrap nudge。
  - Step 1 必须是真实 research 调用：`gh search ...` 或 `curl -sSL ...`
  - 禁止以 `todo_write / mkdir / list_files / setup-only shell` 开局
  - 仅在“空工作区 + 调研任务”触发，避免污染正常代码任务
  - 单测覆盖关键词识别、空目录判定、`run_shell` 可用性守门
- **10c-4 (`main`, 2026-05-05)** — LLM-driven planner（v1）：复杂任务进入 explicit planning mode，先产出 todo plan，再按 `in_progress` 步执行。
  - `AgentLoop` 基于 task/skill/tool 可用性启发式开启 `planning_mode`
  - 首轮 system prompt 强制 `todo_write` 产出 3-7 条 concrete plan；已有 plan 时切换为 execution nudge
  - `build_user_prompt` 渲染 `Execution plan` 与 `Current plan step`
  - 离线 fallback planner 在 `planning_mode` 下也先生成 `todo_write` 计划，保持远端/离线路径一致
  - 300 tests 全绿，含 planning heuristic / prompt rendering / offline bootstrap 回归
- **10b (`main`, 2026-05-05)** — Sub-agent dispatch（v1）：新增 `dispatch_subagent` tool，把独立子任务派发给 child loop，带独立 todo list / budget / transcript。
  - `ToolRegistry` 按 depth 注入 `dispatch_subagent`，默认只允许一层 child，避免无限递归
  - child loop 复用同一套 runtime / skill / policy 逻辑，但关闭 banner、stream 输出与 session 持久化
  - `dispatch_subagent` 支持 `task`、可选 `skill`、可选 `steps`，返回 child tool calls + final message 摘要
  - system prompt 新增 sub-agent delegation nudge，仅在工具可用时出现
  - 离线 planner 在“已有 plan + 探索型 todo + 尚未 dispatch”场景下会主动派一次 child，避免该能力只在强模型上可见
  - child 成功返回后，若命中当前 `in_progress` exploration step，父 todo 会自动标完成并推进到下一条 pending
  - 319 tests 全绿，含 registry depth guard、tool schema、nested loop 回归、offline dispatch heuristic、parent todo auto-advance

**dogfood 累计发现的真实数据：**
- ✅ DeepSeek v4-pro 在 **bounded 多步任务**上完全 work（todo_write 列表、状态切换 in_progress→completed、跨步 transcript replay 都正确）
- ❌ DeepSeek v4-pro 在 **open-ended bootstrap** 上做不到从 setup 切到 research（mkdir+todo_write 振荡，10c-2 机制对但 LLM 不用）
- ❌ DeepSeek v4-flash 不主动用 todo_write（v4-pro 主动用）— nudge 在小模型上效果弱
- ✅ DeepSeek v4-flash + v4-pro 都用 OpenAI 并行 tool calls — Phase 10a C3 fail-loud 守门救了我们
- ✅ `dscode chat` REPL transcript 始终工作正常；`dscode run` 多步任务能力差距由 10c-1 部分修复，10c-2 进一步加固

状态：进行中（10b + 10c-1 + 10c-2 + 10c-3 + 10c-4 完成）

### Phase 10d — Skills 拓展

**10d-1 (`feat/skills-expansion`) — 已完成 (2026-05-03)**：
- 12 个新 skill toml ship 到仓库 `skills/` （research / refactor / debug / write-tests / dependency-update / rust-clippy / python-mypy / pr-fix-feedback / brainstorm / verify-changes / commit-message / readme-update）
- 用户级目录 `~/.config/dscode/skills/` 加载支持，可经 `workspace.user_skills_dir` 配置
- last-wins 撞名语义（user override repo）
- `SkillRegistry::load_dirs(&[paths])` + `LoadStats` 报告 per-path 计数 + override 列表
- `dscode doctor` 加 `[skills]` 段
- 273 → 285 tests, 0 新依赖

**10d-2 (`main`, 2026-05-05) — 已完成**：
- `SkillSpec` schema v2：新增 `triggers` / `initial_todos` / `references`
- 手写 TOML loader 兼容老 schema，同时支持 `[[initial_todos]]` tables
- `AgentLoop` 选中 skill 时可用 `initial_todos` seed 首轮 todo plan
- skill `references` 进入 prompt 和 CLI 输出，便于给模型稳定上下文
- 仓库代表性 skills（research / debug / refactor / write-tests / verify-changes）已升级到 v2 字段
- 300 tests 全绿，含 schema 解析、todo seed、prompt rendering 回归

**10d-3 (`main`, 2026-05-05) — 已完成**：
- 当用户未显式传 `--skill` 时，`resolve_skill` 会基于 `triggers` 从 task 文本自动匹配最相关 skill
- 显式 `--skill` 仍然优先，不会被 auto-select 覆盖
- `AgentLoop` 会打印 skill 来源：`explicit` 或 `auto (trigger match)`
- auto-select 与 10d-2 的 `initial_todos` / `references` 联动，自动选中的 skill 也能 seed todo 和补 prompt 上下文
- 覆盖 resolver 排序、显式优先、runtime auto-seed 回归；全量测试更新到 308+

状态：10d-1 + 10d-2 + 10d-3 完成

### Phase 10e — Benchmark / Dogfood 基线

**10e-1 (`main`, 2026-05-05) — 已完成基础版**：
- 新增 `benchmark` CLI，读取无依赖 manifest，顺序执行一组 task case，并输出 markdown report
- manifest 支持 `name / task / skill / budget / expect_tool / expect_message_contains`
- 默认路径：
  - manifest: `.dscode/benchmarks.txt`
  - report: `.dscode/benchmarks/latest.md`
- 新增示例文件 [`.dscode/benchmarks.example.txt`](/home/willamhou/codes/DeepseekCode/.dscode/benchmarks.example.txt)
- 新增 `deepseek` binary launcher，和 `dscode` 共用同一入口逻辑

**10e-2 (`main`, 2026-05-05) — 已完成基础版**：
- benchmark manifest 扩展了更强约束：
  - `forbid_tool`
  - `min_tool_calls`
  - `max_tool_calls`
  - `max_failed_tools`
  - `notes`
- report 现在会输出：
  - 总 tool calls
  - 总 failed tool calls
  - 每 case 的 failure summary
- 仓库新增默认基线 [`.dscode/benchmarks.txt`](/home/willamhou/codes/DeepseekCode/.dscode/benchmarks.txt)，覆盖 repo inspection / code search / roadmap read / explicit planning 四类只读任务
- 该基线允许“带失败地暴露回归”，不是只追求全绿；它的作用是给 planner/recovery 提供稳定对比面

**10e-3 (`main`, 2026-05-05) — 已完成基础版**：
- planner 对 lookup / planning-only 任务做了首轮收敛：
  - code lookup 类 task 优先 `search_text`，不再先 `dispatch_subagent` 或错误地从 `Cargo.toml` 开始
  - “before acting / report execution steps” 类 task 在 `todo_write` 后直接收尾，不再继续 repository inspection
- 默认 benchmark 基线从 `2/4` 提升到 `4/4`
- 该轮优化直接减少了无效 tool hops，为下一步 failure-recovery 留出更干净的 baseline

**10e-4 (`main`, 2026-05-05) — 已完成基础版**：
- runtime 现在会在常见断点后注入结构化 `recovery_hint` observation，而不是只把错误文本丢给模型自己发挥：
  - `search_text` 无结果
  - `read_file` 失败
  - `dispatch_subagent` 失败
  - `run_shell` 非零退出 / 失败
- offline planner 会优先消费最新的 `recovery_hint`，走确定性的恢复路径：
  - `search_text -> list_files`
  - `read_file -> search_text`
  - `run_shell -> git_diff/read_file/search_text`
- lookup 路径也补了正常收敛：已有 `search_text` 命中时，优先 `read_file` 读取匹配文件，而不是退回 `Cargo.toml` 或重复 `list_files`
- 默认 benchmark 基线保持 `4/4`，说明 recovery 没把前一轮压下来的 tool hops 拉回去
- 全量测试更新到 `332 passed, 0 failed`

**10e-5 (`main`, 2026-05-05) — 已完成基础版**：
- benchmark manifest 新增 `expect_tool_sequence`，可验证关键 tool 链路是否按顺序出现，而不只是“有没有调用过”
- benchmark report 新增 `Tool Trace` 列，直接输出每个 case 的实际 tool 序列，便于看 planner 是否退化成额外 hops
- 默认基线新增 `recover-empty-search` case，稳定覆盖自然恢复路径：
  - `todo_write -> search_text -> list_files -> read_file`
  - 关键断言是 `search_text -> list_files`
- `search-subagent-flow` 也收紧成顺序断言：要求至少出现 `search_text -> read_file`
- 默认 benchmark 基线扩到 `5` 个 case，并保持 `5/5` 通过

### Phase 10f — Failure Recovery / Validation Signals

**10f-1 (`main`, 2026-05-05) — 已完成基础版**：
- `run_shell` 现在会在输出头部附带结构化元数据：
  - `meta.command_kind`
  - `meta.exit_code`
  - `meta.result`
  - `meta.failure_kind`
  - `meta.failed_tests`
  - `meta.stderr_summary`
- shell observation trim 不再把这些头部字段截掉；即使 stdout/stderr 很长，planner 仍能稳定看到结构化信号
- 当前支持的失败类型至少区分：
  - `test_failure`
  - `lint_failure`
  - `build_failure`
  - `command_failure`
- cargo test / pytest 的失败测试名现在会被抽取到 `meta.failed_tests`
- recovery reason 也会消费这些字段：test failure 会把 failing test 名带进 hint，而不是只给一个泛化的 “command failed”
- 全量测试更新到 `338 passed, 0 failed`

**10f-2 (`main`, 2026-05-05) — 已完成基础版**：
- runtime 现在会在两类场景下注入结构化 `replan_hint` observation：
  - 最近步骤里连续出现多个 `recovery_hint`
  - `dispatch_subagent` 返回 `child outcome: blocked`
- `dispatch_subagent` 摘要新增：
  - `child failed tool calls`
  - `child outcome`
- offline planner 遇到最新 observation 为 `replan_hint` 时，会优先回到 `todo_write` 重排父计划，而不是沿着旧 todo 继续硬顶
- replan 后的新计划会以 “Reassess the plan using the latest blocker or recovery signal” 开头，再接常规搜索 / 阅读 / 修改 / 验证步骤
- 全量测试更新到 `342 passed, 0 failed`

**10f-3 (`main`, 2026-05-05) — 已完成基础版**：
- benchmark 现在支持 `seed_observations`，可以在 case 开始前注入结构化 observation，稳定复现 recovery / replan 前态，而不需要依赖真实失败环境
- manifest 支持 `\n` 转义，因此可以内嵌多行 shell observation（含结构化 `meta.*` 字段）
- 默认 benchmark 基线新增 2 个 seeded recovery case：
  - `recover-read-file-failure`
  - `recover-failed-validation`
- 这样当前默认基线一共覆盖 7 个 case，其中恢复类同时覆盖：
  - 自然 `search_text -> list_files`
  - seeded `read_file failed -> search_text`
  - seeded `failed validation -> git_diff`
- benchmark report 新增 `Notes` 列，便于直接区分 baseline / natural recovery / seeded recovery 的意图
- 默认 benchmark 基线更新为 `7/7` 通过

**10f-4 (`main`, 2026-05-05) — 已完成基础版**：
- shell recovery 现在会按 `failure_kind` 分流，而不是对所有 `run_shell` 失败统一走 `git_diff / read_file / search_text`
- `test_failure` 路线：
  - 若刚成功过 `apply_patch` 且可用 `git_diff`，优先复核 diff
  - 若 `meta.failed_tests` 能提取出文件路径，优先 `read_file <that file>`
  - 否则退回 `primary_file` 或通用恢复路径
- `lint_failure / build_failure` 路线：
  - 若 `meta.stderr_summary` 能提取标识符或引用符号，优先 `search_text <derived query>`
  - 否则优先回到 `primary_file`
- `recovery_hint` 现在可携带结构化 `query=` 与 `path=`；offline planner 会严格消费这些字段，而不是重新从 task 文本猜参数
- 默认 benchmark 基线扩到 `9` 个 case，新增：
  - `recover-lint-failure`
  - `recover-test-file-path`
- 默认 benchmark 结果提升并稳定在 `9/9` 通过
- 全量测试继续保持全绿，并覆盖 failure-kind-aware routing 与 planner 参数消费回归

### Phase 10g — Todo Matching / Fixture Realism

**10g-1 (`main`, 2026-05-05) — 已完成基础版**：
- parent todo auto-advance 不再靠 `delegated_task.contains(todo.content)` 这种宽松字符串包含关系命中
- planner 生成的 `dispatch_subagent` 任务现在显式携带：
  - `Delegated todo step: <exact parent todo>`
  - `Parent task: <full user task>`
- `TodoList::complete_in_progress_matching_subagent_task` 优先解析并精确匹配这个 delegated marker；只有旧格式 fallback 才允许“规范化后完全相等”的保守匹配
- 这避免了“内容相似但语义不同”的 delegated task 错误地把父级当前 `in_progress` todo 自动标为完成
- 新增回归覆盖：
  - marker 精确命中会推进
  - 相似但不同的步骤不会误命中
  - 旧格式 fallback 仍兼容 exact normalized match
- 默认 benchmark 保持 `9/9` 通过
- 全量测试更新到 `351 passed, 0 failed`

**10g-2 (`main`, 2026-05-05) — 已完成基础版**：
- benchmark manifest 新增 `workdir` 字段，case 可以切到 manifest 相对路径下的真实 fixture 目录运行，而不再只依赖 seeded observations
- benchmark runner 现在会：
  - 将 `workdir` 解析为 manifest-relative 路径
  - 在受锁保护的临时 cwd 中执行单个 case
  - 在 report 里输出 `Workdir` 列，便于直接区分 repo-root baseline 与 fixture baseline
- 仓库新增真实小仓库 fixture：
  - [`.dscode/fixtures/rust-cli-mini`](/home/willamhou/codes/DeepseekCode/.dscode/fixtures/rust-cli-mini/Cargo.toml)
- 默认 benchmark 基线扩到 `13` 个 case，其中新增 4 个 fixture case，覆盖：
  - read-only inspection
  - `search_text -> read_file`
  - 自然 `search_text -> list_files` recovery
  - seeded `test_failure -> read_file` recovery in fixture workdir
- 这一轮还顺手修了一个 planner 偏差：lookup-heavy task 不再因为 Rust profile 自动把 `cargo test` 塞进 todo plan 或执行路径
- 默认 benchmark 更新为 `13/13` 通过

**10g-3 (`main`, 2026-05-05) — 已完成基础版**：
- `dispatch_subagent` 摘要现在在顶部输出结构化 `meta.child_*` 行，而不只是自由文本：
  - `meta.child_task`
  - `meta.child_skill`
  - `meta.child_budget`
  - `meta.child_tool_calls`
  - `meta.child_failed_tool_calls`
  - `meta.child_outcome`
  - `meta.child_files`
  - `meta.child_final_message`
- child files 会从 subagent 的 `read_file / search_text / list_files` 结果里抽取去重后的文件列表，减少父 planner 从长文本里猜上下文
- parent runtime 的 blocker 检测现在优先按 `meta.child_outcome=blocked` 解析，而不是只做脆弱的字符串包含判断
- 这给后续 parent planner 消费 child findings / files / blockers 留出了稳定落点

**10g-4 (`main`, 2026-05-06) — 已完成基础版**：
- 新增 `dogfood` CLI：
  - `deepseek dogfood run [--skill <name>] [--budget <n>] [--outcome success|failed|stuck|manual] [--manual-intervention] [--notes "..."] "<task>"`
  - `deepseek dogfood report [--out <file>] [--limit <n>]`
- live dogfood ledger 采用 append-only `jsonl`：
  - [`.dscode/dogfood/ledger.jsonl`](/home/willamhou/codes/DeepseekCode/.dscode/dogfood/ledger.jsonl)
  - 每条记录包含 task / skill / budget / model / workdir / outcome / manual_intervention / tool_calls / failed_tool_calls / repeated_call_failures / used_subagent / final_message / tool_trace
- `dogfood run` 会在真实 `AgentLoop` 执行后自动：
  - 推导默认 outcome（`stuck` 优先于 `failed`，否则 `success`）
  - 追加 ledger 记录
  - 刷新 markdown 汇总报告 [`.dscode/dogfood/latest.md`](/home/willamhou/codes/DeepseekCode/.dscode/dogfood/latest.md)
- `dogfood report` 会重新汇总历史记录，输出：
  - success rate
  - failed rate
  - stuck rate
  - manual intervention rate
  - average tool calls
- 这一步把 benchmark 之外的真实任务结果也纳入可比对面，后续 planner / recovery / subagent 迭代不再只靠 synthetic baseline

**10h-1 (`main`, 2026-05-06) — 已完成基础版**：
- parent planner 现在会先消费 `dispatch_subagent` 的结构化 child findings，再决定下一步：
  - 优先读 `meta.child_files`
  - 若没有显式 child files，也会从 `meta.child_final_message` 里回收形如 `src/main.rs` 的路径
  - 只有任务本身没有 query、child 也没给文件、而且 parent 还没进入 `read_file` 时，才会把 `meta.child_final_message` 里的符号当成 fallback `search_text` query
- 这轮顺手收紧了 child query 的使用顺序，避免 parent 在已经读过相关文件后，又被 child final message 拖去多跑一轮 `search_text`
- 新增回归覆盖：
  - child summary 直接给文件路径时，parent 会优先 `read_file`
  - child summary 只给符号查询时，parent 才会 fallback 到 `search_text`
  - parent 已经读取相关文件后，不会再回头消费 child query
  - child final message 内嵌文件路径时，也能被提取成 follow-up read target
- 默认 benchmark 保持并验证为 `13/13` 通过，其中 real fixture inspection case 回到预期链路：
  - `todo_write -> dispatch_subagent -> list_files -> read_file`

**10h-2 (`main`, 2026-05-06) — 已完成基础版**：
- benchmark fixture 家族从单一 Rust read-only 样例扩到三类真实小仓库：
  - Rust read/recovery fixture：[`rust-cli-mini`](/home/willamhou/codes/DeepseekCode/.dscode/fixtures/rust-cli-mini/Cargo.toml)
  - Python CLI fixture：[`python-cli-mini`](/home/willamhou/codes/DeepseekCode/.dscode/fixtures/python-cli-mini/pyproject.toml)
  - JavaScript CLI fixture：[`js-cli-mini`](/home/willamhou/codes/DeepseekCode/.dscode/fixtures/js-cli-mini/package.json)
- benchmark manifest 新增 `isolate_workdir = true`，runner 会把该 fixture 复制到临时目录执行，再清理副本：
  - 这让 write+validate case 可以真实调用 `apply_patch` 和 `run_shell`
  - 同时避免污染源 fixture 或仓库工作区
- isolated case 在 runner 内会临时开启 `DSCODE_AUTO_APPROVE_WRITES=1` 和 `DSCODE_AUTO_APPROVE_SHELL=1`，消除非交互 benchmark 被 confirm prompt 卡死的问题
- 新增真实 write+validate fixture：
  - [`rust-write-mini`](/home/willamhou/codes/DeepseekCode/.dscode/fixtures/rust-write-mini/Cargo.toml)
  - 基线链路稳定为 `apply_patch -> git_diff -> run_shell`
- 这一轮还顺手修了两个真实会影响线上行为的问题：
- direct edit parser 现在支持反引号 quoted segments，并会截断 trailing `and validate ...` / `then run ...` 子句
- skill auto-select 在 task 已经明确要求 direct edit 时，会跳过不允许 `apply_patch` 的 skill，避免 `validate` 误触发 `verify-changes` 把写入任务带偏
- 默认 benchmark 基线扩到 `18` 个 case，并保持 `18/18` 通过

**10h-3 (`main`, 2026-05-06) — 已完成基础版**：
- `dogfood` CLI 新增：
  - `deepseek dogfood export-benchmark [--out <file>] [--limit <n>] [--outcome success|failed|stuck|manual]`
- dogfood ledger 现在除了 task / outcome / tool_trace 之外，还会持久化 `benchmark_seed_observations`
  - 内容来自最近 3 个 tool events 的可回放 seed 串
  - 格式直接兼容 benchmark manifest 的 `seed_observations = "... || ..."` 写法
- `dogfood report` 新增 `Benchmark seed candidates` 统计，能快速看当前 live dogfood 里有多少失败/stuck/manual 运行适合反推成 benchmark case
- `dogfood export-benchmark` 会把失败 / stuck / manual 运行导出成可直接追加到 manifest 的草稿：
  - 自动生成 case 名
  - 保留 task / skill / budget
  - 尽量把 repo-root workdir 转成相对路径
  - 写出可复用的 `seed_observations`
- 这轮让 dogfood -> benchmark 不再是纯手工抄写，而是有了稳定的“先跑真实任务，再抽取 regression seed”出口

**10h-4 (`main`, 2026-05-06) — 已完成基础版**：
- fixture-backed write + validate 路线现在不只覆盖 happy path，还覆盖了真实 failed-validation recovery：
  - 成功链路：`apply_patch -> git_diff -> run_shell`
  - 失败链路：`apply_patch -> git_diff -> run_shell -> read_file`
- 为了让这条失败链路稳定收敛，planner recovery 做了两个收紧：
  - 若 failed validation 的 `recovery_hint` 仍指向 `git_diff`，但 diff 在本轮已经看过，就直接回到刚刚 patch 过的文件
  - `preferred_read_path` 现在会优先识别最近成功 `apply_patch` 的真实输出，包括 `patched <path>` 和 unified patch 的 `Applied unified patch in ... / modified:` 组合，避免 recovery 退回到无关的 `primary_file`
- 新增真实 isolated fixture case：
  - `fixture-recover-write-validate-rust-mini`
  - 任务会故意把 `a - b` 改成 `a * b`，让 `cargo test` 失败，再验证 planner 是否会回读 `src/lib.rs`
- 默认 benchmark 基线扩到 `19` 个 case，并保持 `19/19` 通过

**10h-5 (`main`, 2026-05-06) — 已完成基础版**：
- benchmark 结果现在不再只保留 latest report，还会把每次运行的汇总指标 append 到：
  - [`.dscode/benchmarks/history.jsonl`](/home/willamhou/codes/DeepseekCode/.dscode/benchmarks/history.jsonl)
- 每条历史记录会持久化：
  - manifest 路径
  - `passed/cases`
  - `total_tool_calls`
  - `total_failed_tool_calls`
  - `duration_ms`
  - 运行当时的 dogfood snapshot（`runs/success/failed/stuck/manual`）
- benchmark report 现在新增两块趋势信息：
  - `Previous benchmark: ... Δ ...`，直接比较最近两轮的通过数、tool calls、failed tools
  - `## Recent Runs`，展示最近 5 次 benchmark 历史
- 这让 benchmark 和 dogfood 不再只是“看 latest.md”，而是开始有连续的趋势面

**10h-6 (`main`, 2026-05-06) — 已完成基础版**：
- `dogfood` CLI 新增：
  - `deepseek dogfood promote-benchmark [--manifest <file>] [--limit <n>] [--outcome success|failed|stuck|manual] [--dry-run]`
- promotion workflow 现在是闭环的：
  - 从 dogfood ledger 读取 replayable `benchmark_seed_observations`
  - 对照正式 benchmark manifest 做去重
  - 自动处理 case name 冲突
  - 非 dry-run 模式下直接 append 到目标 manifest
- 去重不是只看 case name，而是按 `task + skill + workdir + seed_observations` 做 identity 判断，避免同一条 regression seed 被重复提升
- 这一步把 `dogfood run -> export seed -> 手工拷贝到 benchmarks.txt` 进一步收敛成 `dogfood run -> promote-benchmark` 的直接路径

**10h-7 (`main`, 2026-05-06) — 已完成基础版**：
- real fixture 的 failed-validation 路线现在不只会“读回刚 patch 的文件”，还会在任务明确要求“修到通过”为止时继续给出第二次修复尝试
- planner 新增了一个很窄的 retry 通道：
  - 仅在 task 明确包含 `until the tests pass` / `keep fixing` 这类意图时开启
  - 仅在已经发生 `apply_patch -> git_diff -> run_shell(failed) -> read_file` 后触发
  - 当前只对简单 arithmetic replace 做修复推断，依据 failing test 名里的 `add/sub/mul/div` 语义选择目标 operator
- `git_diff` 和 `run_shell` 现在不再是“每轮只允许一次”，而是按“每次成功 patch 后最多再跑一次”计数，这让 retry patch 后的 diff / validation 能稳定继续执行
- `run_shell` 会补上 `~/.cargo/bin` 到 PATH，避免 fixture benchmark 在非 login shell 下把 `cargo test` 错判成环境缺失
- 新增真实 isolated fixture case：
  - `fixture-retry-write-validate-rust-mini`
- 预期链路：`apply_patch -> git_diff -> run_shell -> read_file -> apply_patch -> git_diff -> run_shell`
- 默认 benchmark 基线扩到 `20` 个 case，并提升到 `20/20` 通过

**10i-1 (`main`, 2026-05-06) — 已完成基础版**：
- benchmark 现在不再只展示历史，而是会对“同 manifest + 同 case 数”的最近可比运行做 trend gate
- gate 规则保持很窄，优先抓明显回归：
  - 当前 `passed` 低于最近窗口里的可比 best 时直接判回归
  - 当前 `total_failed_tool_calls` 高于可比中位数时判回归
  - 当前 `total_tool_calls` 高于可比中位数加容忍度时判回归
    - 容忍度当前取 `max(3, ceil(median * 10%))`
- gate 只在有足够历史时启用：
  - 仅比较最近 `5` 次可比运行
  - 至少需要 `3` 次 prior comparable runs，否则 report 标成 `skipped`
- benchmark report 现在会直接输出：
  - `Trend gate: pass ...`
  - 或 `Trend gate: FAILED ...`
  - 或 `Trend gate: skipped ...`
- CLI 语义也收紧了：
  - report 和 history 仍然会照常写出
  - 但如果 trend gate 失败，`deepseek benchmark`（兼容别名 `dscode benchmark`）会返回非零退出，便于直接挂到 CI 或本地回归门禁上

**10i-2 (`main`, 2026-05-06) — 已完成基础版**：
- `promote-benchmark` 现在不再默认把所有 replayable non-success seed 都提升进正式基线，而是增加了一层 promotion policy
- 默认 policy 只接受更像真实 regression 的记录：
  - outcome 默认只接受 `failed` / `stuck`
  - 必须有真实失败信号：`failed_tool_calls > 0`，或 `repeated_call_failures > 0`
  - 必须有真实 tool trace，且总 tool calls 不超过 `8`
- `manual` 记录不会再被默认 promote；只有显式传 `--outcome manual` 时才允许进入 promotion 流程
- `export-benchmark` 仍保持宽松，继续作为“先导出草稿再人工挑选”的出口；收紧只发生在 `promote-benchmark`
- promotion 命令输出现在会把筛选结果拆开显示：
  - `duplicates skipped`
  - `policy skipped`
  - `selected`
  这样更容易看出是“没有候选”，还是“候选被策略拦住了”

**10i-3 (`main`, 2026-05-06) — 已完成基础版**：
- benchmark case schema 新增了：
  - `expect_last_tool_output_contains = "..."`
- 这让 fixture write+validate case 的判定不再只停留在“tool trace 对了”，而是能直接要求最终关键工具输出满足某个结果语义
- 默认基线里三条 Rust write/validate 相关 case 已切到更强的成功语义：
  - `fixture-write-validate-rust-mini` 现在要求最后一个 `run_shell` 输出包含 `meta.result=ok`
  - `fixture-recover-write-validate-rust-mini` 现在要求最后一个 `read_file` 真的读回了坏改动后的内容
  - `fixture-retry-write-validate-rust-mini` 现在要求最后一个 `run_shell` 输出包含 `meta.result=ok`
- 这一步让 benchmark 开始验证“最终命令真的成功/失败到了预期状态”，而不是只验证 planner 有没有走过看起来像对的链路

**10i-4 (`main`, 2026-05-06) — 已完成基础版**：
- benchmark case schema 现在支持显式 `category = "..."`，默认基线已经按能力切成：
  - `read_only`
  - `write_validate`
  - `recovery`
  - `subagent`
  - `planning`
- benchmark history 现在不只落总量，还会把每个 category 的：
  - `cases`
  - `passed`
  - `total_tool_calls`
  - `total_failed_tool_calls`
  一起写进 history record
- benchmark report 新增 `## Category Slices` 表，直接展示每个 slice 的：
  - 当前通过数 / case 数
  - 当前 tool calls / failed tools
  - 相对上一次 benchmark 的 delta
  - 该 slice 自己的 trend gate 状态
- trend gate 也从“只看总量”升级成“总量 + category slice”双层判断：
  - overall 规则保持不变
  - 每个 category 会单独对比最近可比运行
  - 如果某个 slice 的 tool calls / failed tools / passed 出现明显回归，即使总量看起来没坏，也会把 gate 打红
- 这一步解决了一个关键盲点：read-only baseline 的稳定性不再能掩盖 write/recovery/subagent 的真实退化

**10i-5 (`main`, 2026-05-07) — 已完成基础版**：
- `dogfood promote-benchmark` 现在不只输出：
  - `selected`
  - `duplicates skipped`
  - `policy skipped`
  还会在存在 policy reject 时打印 `policy skip reasons`
- explainability 现在按原因聚合，并为每类原因带一条示例 task，当前覆盖的 policy reason 包括：
  - `manual outcome requires --outcome manual`
  - `tool trace too long (>8 calls)`
  - `missing real tool trace`
  - `missing failed/stuck/manual signal`
- policy 本身没有放宽；改动只发生在 explainability 层，所以 promotion 结果不会因为这轮变化而变得更松或更紧
- dogfood promotion 的 case identity 和导出 block 仍然保留 `category`，因此不同 slice 的 regression seed 不会因为 explainability 改动被错误去重
- 这一步解决的是“为什么没 promote 进去”不可见的问题，让人工挑 seed 时能快速判断：
  - 是默认 policy 太严格
  - 还是这条 dogfood 记录本身就不像一个好 benchmark regression seed

**10i-6 (`main`, 2026-05-07) — 已完成基础版**：
- benchmark manifest 新增：
  - `expect_tool_output_contains = "tool_name:needle"`
- 语义是：目标 tool 最后一次调用的输出必须包含给定 `needle`
- 这让 benchmark 不再只能断言：
  - final message
  - last tool output
  而是可以稳定检查“中间某一步关键工具到底产出了什么”
- 默认 write+validate 基线已经接上这套更细的断言：
  - `fixture-recover-write-validate-rust-mini` 现在会显式要求 `run_shell` 输出包含 `meta.failure_kind=test_failure`
  - `fixture-retry-write-validate-rust-mini` 现在会显式要求中间那次 `read_file` 真的读回了坏改动内容
- 这一步把 benchmark 从“路径看起来像对了”进一步推到“关键中间状态也真的出现了”

**10i-7 (`main`, 2026-05-07) — 已完成基础版**：
- dogfood ledger 现在会直接持久化：
  - `benchmark_category`
- 新产生的 dogfood 记录会在写入时就把 benchmark category 算好，而不是等到 `export-benchmark` / `promote-benchmark` 时再临时推断
- `dogfood export-benchmark` 现在会直接输出：
  - `category = "..."`
  并优先使用 ledger 里的真实 category
- `dogfood promote-benchmark` 也同样优先吃 ledger category；老的 ledger 行如果没有这个字段，会在读取时自动 fallback 推断并补回内存对象
- 这一步的价值不在“今天导出的文本多一行”，而在于：
  - category 决策点前移到 dogfood 采集时
  - export / promote / de-dup / later analytics 都开始共享同一份 category truth，而不是各自猜一遍

**10i-8 (`main`, 2026-05-07) — 已完成基础版**：
- benchmark 的 category slice trend gate 现在支持 warmup：
  - 当历史里既有老的 `version=1` overall run，又只有少量新的 `version=2` category-aware run 时
  - 会优先用已有的 category-aware baseline 去保守投影旧 v1 run 的 slice metrics
- 这个投影只用于：
  - 让 `comparable_runs` 更快达到门槛
  - 让 `planning / read_only / recovery / subagent / write_validate` 更早开始做 slice-level gate
- 它不会用当前 run 反推自己，也不会在完全没有真实 category baseline 时硬猜：
  - 没有至少一条历史 category-aware 记录时，slice gate 仍然保持 `skipped`
- 真实效果是：
  - category 区块已经从 `skipped (0/3)` 收紧成 `pass vs 5 runs`
  - 而不是继续等更多纯 v2 history 自然积累
- 这一步的价值在于：
- mixed-history 迁移阶段更快进入可比状态
- slice regression 更早暴露
- 但 overall/category 的 pass/fail 阈值没有被放松

**10i-9 (`main`, 2026-05-07) — 已完成基础版**：
- benchmark manifest 现在支持：
  - `assertion_bundle = "..."`
- bundle 只提供默认断言组合，case 里显式写的字段仍然优先覆盖，因此它的作用是：
  - 收短重复的 `expect_tool / expect_tool_sequence / max_tool_calls / max_failed_tools`
  - 但不损失那些 case-specific 的中间状态断言
- 当前内置 bundle 覆盖了最常见的几类链路：
  - `read_only_inspect`
  - `read_only_search`
  - `recovery_search_fallback`
  - `recovery_readback_then_search`
  - `recovery_diff_then_readback`
  - `recovery_search_then_readback`
  - `write_validate_ok`
  - `write_validate_failure_readback`
  - `write_validate_retry_ok`
  - `planning_todo_only`
- 默认 benchmark manifest 和 example manifest 已经切到这套 bundle：
  - 重复断言明显减少
  - 但像 `read_file:2     a * b` 这种 case-specific output check 仍然保留在具体 case 上
- parser 回归覆盖了三条关键边界：
  - bundle 默认值会生效
  - 显式字段可以覆盖 bundle 默认值
  - 未知 bundle 会直接报错
- 真实 benchmark 继续保持 `20/20` 通过，说明这是结构收敛，不是语义漂移

**10i-10 (`main`, 2026-05-07) — 已完成基础版**：
- dogfood markdown report 现在新增：
  - `## Category Breakdown`
- 会按 ledger 里的 `benchmark_category` 聚合每个 slice 的：
  - `runs`
  - `success`
  - `failed`
  - `stuck`
  - `manual`
  - `avg tool calls`
  - `seed candidates`
- 明细表也新增了 `Category` 列，因此从 summary 到单条 run 都能直接看到真实任务落在哪个 slice
- 这一步的意义是把 benchmark 的 category 视角延伸到 live dogfood：
  - benchmark 已经知道 `read_only / recovery / write_validate / planning / subagent`
  - dogfood 现在也能按同一套切面展示真实任务分布和成功率
- 当前真实 ledger 样本还很小，所以 category 分布暂时不代表趋势；但报表结构已经稳定，可以开始持续积累
- 这一步还顺手暴露了一个后续该修的问题：
  - 当前部分 read-only dogfood 任务因为 trace 里先出现 `todo_write`，会被 heuristic 归到 `planning`
  - 这是 category inference 精度问题，不是 report 聚合问题

**10i-11 (`main`, 2026-05-07) — 已完成基础版**：
- dogfood category inference 不再把“任何带 `todo_write` 的任务”都粗暴归到 `planning`
- 新的优先级更接近 task 语义，而不是只看第一步用了什么 tool：
  - `write_validate` 仍优先由 `apply_patch / run_shell` 判定
  - `recovery` 仍优先由 `recovery_hint / failed_tool_calls / repeated_call_failures` 判定
  - `subagent` 主要看 task 是否真的在讨论 subagent / parent-child loop
  - `planning` 只保留给明确的 plan-only task，或 trace 里只有 `todo_write` 这类前置 planning 行为
  - 其他正常只读分析任务回到 `read_only`
- 为了不让旧 ledger 长期带着错误标签，读取 dogfood 记录时还加了一层保守纠偏：
  - 如果历史记录存的是 `planning`
  - 但按新规则它明显不是 planning
  - report/export/promote 会优先用纠偏后的 category
- 真实效果是：
  - 现有 dogfood report 里那条 `inspect repository layout ...` 已经从 `planning` 修正为 `read_only`
  - planning-only case 仍然保持 `planning`

**10i-12 (`main`, 2026-05-08) — 已完成基础版**：
- benchmark trend gate 现在已经能挂到更接近真实开发入口的命令上，而不只是在手工跑 `deepseek benchmark` 时才生效
- 新增显式 hook：
  - `deepseek dogfood run --benchmark-gate ...`
  - `deepseek pr fix ... --benchmark-gate`
  - `deepseek pr patch ... --benchmark-gate`
- 语义保持很直接：
  - 先执行原始 dogfood / PR 任务
  - 再自动跑默认 benchmark baseline
  - 如果 benchmark trend gate 失败，整个入口命令也会非零退出
- 这一步没有默认把 gate 强塞到所有入口，仍然要求显式打开：
  - 避免把一次普通探索或 review 变成意外的长链 benchmark 运行
  - 但需要时已经可以把“真实任务 + 基线回归门禁”串成同一条命令链
- 真实验证已经跑过：
  - `deepseek dogfood run --benchmark-gate "inspect repository layout ..."` 会在 dogfood ledger/report 写完后继续跑 benchmark，并打印 trend gate 结果

**10i-13 (`main`, 2026-05-08) — 已完成基础版**：
- benchmark history 现在不只会顺手带一个 dogfood 总量 snapshot，还会把 live dogfood 的 category slices 一起落盘
- 新增的 benchmark report 区块：
  - `## Dogfood Slices`
  - 会按 category 展示 `runs / success / failed / stuck / manual / avg tool calls`
- `## Recent Runs` 也会追加 `Dogfood Categories` 摘要列，方便直接看最近几轮 benchmark 对应看到了哪些真实 dogfood slice
- 这一步把“实验室 benchmark slice”与“真实任务 dogfood slice”第一次放进了同一份 benchmark history/report：
  - benchmark 的 `Category Slices` 负责看基线能力是否退化
  - `Dogfood Slices` 负责看最近真实任务主要落在哪些能力面上
- review 后顺手补了一层兼容：
  - 对旧 dogfood ledger，如果缺少显式 `benchmark_category`
  - benchmark 侧会复用 dogfood 的 fallback inference
  - 避免 report 里继续积累 `unknown` slice
- 真实验证已经跑过：
  - `deepseek benchmark --out /tmp/deepseek-bench-dogfood-slices.md`
  - 当前 report 已经稳定显示 `Dogfood Slices`，且最新一轮不再出现新的 `unknown` category

**10i-14 (`main`, 2026-05-08) — 已完成基础版**：
- dogfood report 不再只有一份静态 `Category Breakdown`，现在新增了 `## Category Trend`
- 这块趋势视图直接基于现有 ledger 计算：
  - recent 5 runs
  - previous 5 runs
  - 不额外引入新的 dogfood history 文件
- 当历史足够时，会按 category 输出：
  - `Recent Runs / Prev Runs`
  - `Recent Success / Prev Success`
  - `Δ Success pp`
  - `Recent Avg Tools / Prev Avg Tools`
  - `Δ Tools`
  - `Recent Seeds / Prev Seeds`
- 当历史不够时，不会硬算趋势，而是显式打印：
  - `Status: insufficient history`
- 这一步的目标是先把“真实任务的 slice 级变化”稳定展示出来：
  - 方便和 benchmark 的 `Category Slices` 对照看
  - 后续如果要做 dogfood-side gate，也有稳定的窗口语义可以直接复用
- 真实验证已经跑过：
  - `deepseek dogfood report --out /tmp/deepseek-dogfood-trend.md`
  - 当前真实 ledger 样本只有 2 条，所以 report 会正确显示 `insufficient history`

**10i-15 (`main`, 2026-05-08) — 已完成基础版**：
- `benchmark gate` 现在不只挂在 `dogfood run` 和 `pr fix/patch` 上，`run` 主入口也支持显式打开
- 新增 CLI 语义：
  - `deepseek run --benchmark-gate "..."`
- 行为与前两条入口保持一致：
  - 先执行原始任务
  - 再自动跑默认 benchmark baseline
  - 如果 trend gate 失败，`run` 命令本身也会非零退出
- 这一步保持 opt-in，而不是默认打开：
  - 避免把普通一次性任务都拖进一轮完整 baseline
  - 但当需要“真实任务后立即做回归门禁”时，主入口已经能直接承担这件事
- 真实验证已经跑过：
  - `deepseek run --budget 4 --benchmark-gate "inspect repository layout and summarize the main entrypoints for a new contributor"`
  - 命令会先跑主任务，再自动跑 benchmark，并打印 trend gate 结果

**10i-16 (`main`, 2026-05-08) — 已完成基础版**：
- benchmark 的复杂任务样本库不再主要集中在 Rust
- 新增了 3 条真实 isolated cross-language write+validate case：
  - `fixture-write-validate-python-mini`
  - `fixture-recover-write-validate-python-mini`
  - `fixture-write-validate-js-mini`
- 对应新增 fixture：
  - `.dscode/fixtures/python-write-mini`
  - `.dscode/fixtures/js-write-mini`
- 这让 `write_validate` slice 从原来的 `3` 条扩到 `6` 条，并且覆盖：
  - Python `pytest`
  - JavaScript `npm test`
  - failed-validation readback（Python）
- 真实 baseline 已经跑过：
  - `deepseek benchmark --out /tmp/deepseek-bench-complex-samples.md`
  - 结果是 `23/23` 通过
- 这一步的意义不是单纯增加 case 数，而是确认：
  - planner 的“replace -> diff -> validate”闭环不是 Rust-only 假象
  - recovery 逻辑至少已经在第二种语言上真实成立

**10i-17 (`main`, 2026-05-08) — 已完成基础版**：
- subagent follow-up 现在不再只盯第一条 `meta.child_files`
- parent planner 会基于“最新一次 dispatch_subagent 之后已经发生了多少次成功 `read_file`”来推进 child file 列表
- 结果是：
  - child 返回多个相关文件时
  - parent 会继续读下一个还没消费的 child file
  - 而不是过早退回 `list_files` 或盲搜
- 这一步主要收紧的是复杂探索任务里的 orchestration：
  - 减少无效 hop
  - 提高 child 结果 merge-back 的利用率
- 定向验证已经跑过：
  - 新增单测覆盖“读完第一个 child file 后继续读第二个 child file”
  - baseline benchmark 继续保持通过

**10i-18 (`main`, 2026-05-08) — 已完成基础版**：
- baseline benchmark 现在新增了 `pr_workflow` slice
- 默认 manifest 已覆盖两条 seeded PR 任务：
  - PR review readback
  - PR fix recovery
- `dogfood` 的 benchmark category inference 现在会优先把：
  - `pull request`
  - `review feedback`
  - `failed ci`
  - `pr #...`
  - `github pr`
  这类任务归到 `pr_workflow`
- 这一步的意义不是“CLI 里有 pr 子命令”而已，而是：
  - benchmark history 开始对 PR 向任务单独记账
  - trend gate 可以看见这条任务线有没有退化
- 定向验证已经跑过：
  - 默认 baseline 扩到 25 个 case
  - 连续 warmup 后 `pr_workflow` slice 已进入 `pass vs 3 runs`

**10i-19 (`main`, 2026-05-08) — 已完成基础版**：
- `run_shell` 现在会把 `node --test` 识别成正式 `test` 命令，而不是普通 shell
- `meta.failed_tests` 已支持从 Node TAP 风格输出抽取失败用例与文件路径
- 新增了 JavaScript recovery baseline：
  - seeded `node --test` failure
  - planner 应该先 `read_file` 读取失败测试文件，再扩展搜索
- 这一步的价值是把：
  - Rust `cargo test`
  - Python `pytest`
  - JavaScript `node --test`
  三条语言路径统一进同一套 failure-kind / recovery_hint 机制
- 定向验证已经跑过：
  - Node test failure parsing 单测通过
  - 默认 baseline 扩到 26 个 case 并保持通过

**10i-20 (`main`, 2026-05-08) — 已完成基础版**：
- category slice trend gate 现在会先检查 `category.cases` 是否与当前运行可比
- 如果历史里同名 slice 的 case 数不同，就不再直接拿旧 `total_tool_calls` 强比
- 这样可以避免一种假回归：
  - baseline 新增了 recovery case
  - category 总 tool calls 自然上涨
  - 但 gate 却把“分母变了”误判成性能退化
- 修正后，这类结构变化会先回到 warmup / insufficient history，再等待新的同构历史积累
- 定向验证已经跑过：
  - 新增单测覆盖“同 manifest 同 category 但 case 数不同”时跳过强比较
  - 默认 baseline trend gate 已恢复通过

**10i-21 (`main`, 2026-05-08) — 已完成基础版**：
- JavaScript write+validate 现在不只支持 happy path，也支持真实 retry 闭环
- `run_shell` 已支持解析 Node 默认测试输出里的：
  - `test at test/foo.test.js:1:1`
  这类失败文件路径
- JS failed-validation recovery 现在会优先读取失败测试文件，而不是只看 diff
- retry planner 也不再只靠 `meta.failed_tests` 里的测试名推断修复方向；必要时会从刚读回的测试文件内容继续推断
- 结果是 `npm test` 场景现在也能走通：
  - `apply_patch -> git_diff -> run_shell -> read_file(test) -> apply_patch -> git_diff -> run_shell`
- 定向验证已经跑过：
  - Node 默认输出解析单测通过
  - JS recovery directive 单测通过
  - JS retry planner 单测通过
  - 临时 JS retry benchmark 已真实走通 7-step 链路

**10i-22 (`main`, 2026-05-08) — 已完成基础版**：
- `pr_workflow` baseline 不再只有：
  - PR review readback
  - failed test fix
- 现在又补上了第三类样本：
  - failed CI lint/build log -> search -> read_file
- 这一步的重点不是增加 case 数，而是把 PR / CI 任务的入口扩成三种不同恢复模式：
  - diff 驱动 review
  - failing test path 驱动 readback
  - stderr symbol 驱动 search/read
- 定向验证已经跑过：
  - 临时 seeded CI lint benchmark 通过
  - 已收进默认 baseline，等待同构 history warmup

**10i-23 (`main`, 2026-05-08) — 已完成基础版**：
- PR patch / review-feedback 路径现在也有了专门 baseline
- offline planner 对这类任务做了两处收紧：
  - 有 `git_diff + list_files` 的 PR 上下文时，优先 `read_file` 读取 changed file
  - 不再把单引号 PR title 里的自然语言短语误当成代码搜索词
- 同时，对这类“上下文已经足够明确”的 PR 任务，planner 不再强制先 `todo_write`
- 结果是 seeded patch case 从：
  - `todo_write -> dispatch_subagent -> search_text -> list_files`
  收敛成：
  - `read_file`
- 定向验证已经跑过：
  - 新增单测覆盖“PR patch 优先读 changed file”
  - 新增单测覆盖“读完 changed file 后不再搜索 PR title”
  - 新增单测覆盖“PR patch 在 planning mode 下也跳过初始 todo_write”
  - 临时 patch benchmark 已压到单步 `read_file`

**10i-24 (`main`, 2026-05-08) — 已完成基础版**：
- `pr_fix` 路径也收紧成了“targeted readback first, then stop”
- 对已经能从 PR / CI 上下文精确定位文件的离线任务：
  - 不再继续 `search_text`
  - 不再追加 `list_files`
  - 也不再掉进无意义的 `todo_write` replanning
- 结果是：
  - Rust `pr_fix` seeded case 从 `read_file -> search_text -> todo_write -> todo_write` 收敛到 `read_file`
  - JavaScript `pr_fix` seeded case 也能直接落到失败测试文件 `read_file`
- 这一步把 `pr_workflow` 从“多形态但主要是 Rust”推进成了“至少开始跨语言”
- 定向验证已经跑过：
  - 新增单测覆盖“PR fix 在 targeted readback 后直接 finish”
  - 临时 Rust / JS PR fix benchmark 都通过
  - JavaScript case 已并入默认 baseline

**10i-25 (`main`, 2026-05-08) — 已完成基础版**：
- `pr_workflow` 不再只覆盖 seeded readback；现在开始进入真实 `patch + validate` 链路
- 关键修正不是 patch tool 本身，而是 explicit planning gate：
  - 之前只对 `replace ... with ... in ...` 开头的任务跳过 `todo_write`
  - PR 风格的 direct edit 任务即使能明确解析出 edit request，也会先掉进 `todo_write -> dispatch_subagent`
- 现在 planning heuristic 改成：
  - 只要 task 能解析出 direct edit request，就直接关闭 explicit planning
- 结果是 PR 风格的 replace+validate 任务从：
  - `todo_write -> dispatch_subagent -> list_files -> read_file -> apply_patch -> run_shell`
  收敛成：
  - `apply_patch -> git_diff -> run_shell`
- 默认 baseline 现在新增了一条真实 `pr_workflow` patch+validate case：
  - `fixture-pr-patch-validate-rust-mini`
- 定向验证已经跑过：
  - 新增单测覆盖“PR 风格 direct edit task 也跳过 explicit planning”
  - 临时 Rust PR patch+validate benchmark 已真实走通 3-step 链路
  - 已并入默认 baseline

**10i-26 (`main`, 2026-05-08) — 已完成基础版**：
- `pr_workflow` 现在不只会做一次 patch + validate，也开始覆盖 failed-validation 后的一次 retry 收敛
- 新增真实 baseline：
  - `fixture-pr-retry-validate-rust-mini`
- 它走的不是 seeded readback，而是完整链路：
  - `apply_patch -> git_diff -> run_shell -> read_file -> apply_patch -> git_diff -> run_shell`
- 这说明前一轮为 PR 风格 direct edit 任务收紧的 planning gate 不只是能过 happy path，也不会挡住后续 retry 逻辑
- 定向验证已经跑过：
  - 临时 Rust PR retry benchmark 已真实走通 7-step 链路
  - 已并入默认 baseline

**10i-27 (`main`, 2026-05-08) — 已完成基础版**：
- `pr_workflow` 的 retry 闭环现在不只是一门语言
- 新增真实 baseline：
  - `fixture-pr-retry-validate-js-mini`
- 它和 Rust retry case 保持同一条结构：
  - `apply_patch -> git_diff -> run_shell -> read_file -> apply_patch -> git_diff -> run_shell`
- 这一步的意义是把：
  - Rust `cargo test`
  - JavaScript `npm test`
  在 PR 风格 direct edit + failed-validation retry 这条线上对齐
- 定向验证已经跑过：
  - 临时 JavaScript PR retry benchmark 已真实走通 7-step 链路
  - 已并入默认 baseline

**10i-28 (`main`, 2026-05-08) — 候选验证，未晋级默认 baseline**：
- 尝试把 `pr_workflow` 的 direct-edit retry 扩到 Python：
  - `fixture-pr-retry-validate-python-mini`
- 单 case benchmark 能真实走通：
  - `apply_patch -> git_diff -> run_shell -> read_file -> apply_patch -> git_diff -> run_shell`
- 但在完整 baseline 里会出现不稳定：
  - 单跑通过
  - 全量 baseline 中偶发掉到 `last tool run_shell output did not contain meta.result=ok`
- 处理结论：
  - 保留它作为候选 case
  - 暂不放进默认 benchmark gate，先保证主基线稳定

**10i-29 (`main`, 2026-05-08) — 已完成基础版**：
- `dogfood run` 现在支持 `--workdir`
- 行为是：
  - 任务执行目录可以切到指定 fixture / repo
  - 但 dogfood ledger、report、benchmark gate 仍然落在当前仓库
- 这让“主仓库记账 + 临时 fixture 执行”第一次真正可用
- 已做真实 live 验证：
  - 在临时复制的 `rust-write-mini` 里跑 `pr_workflow` retry 任务
  - 真实走通 `apply_patch -> git_diff -> run_shell -> read_file -> apply_patch -> git_diff -> run_shell`
  - 记录已写回主仓库 `.dscode/dogfood/ledger.jsonl`
  - `post-task benchmark gate` 也通过
- 当前 dogfood report 已不再只有 `read_only`：
  - `pr_workflow` live history 已开始积累

**10i-30 (`main`, 2026-05-08) — 已完成基础版**：
- benchmark / dogfood 对 tool output 的判定现在使用真实原始输出，而不是 observation summary
- 原因是之前 `ToolEvent.output` 复用了给模型的摘要版：
  - shell 输出较长时，`meta.result=ok` 这类头部信号有机会在摘要裁剪里丢失
  - 结果就是 planner 真实成功，但 benchmark 断言偶发误判
- 现在 runtime 改成：
  - observation 继续走 `summarize_for_kind(...)`
  - `ToolEvent.output` 保留原始工具输出
- 这一步的价值不是增加功能，而是把 benchmark / dogfood 的“验证面真值”与“prompt 压缩视图”解耦
- 定向验证已经跑过：
  - 新增 benchmark 单测覆盖“长 shell 输出也能命中 `meta.result=ok`”
  - 默认 33-case baseline 继续保持通过

**10i-31 (`main`, 2026-05-08) — 已完成基础版**：
- `pr_workflow` 的 merge-back 现在不再要求任务文本里显式写出 `subagent` / `child loop`
- 只要任务本身就是 PR workflow，并且 parent 已收到 `dispatch_subagent` 的 `meta.child_files`：
  - 读完第一份 child file 后
  - 父循环也会继续读第二份 child file
- 这一步的意义是把：
  - “有 child files，但只有带 `continue from the subagent findings` 字样的任务才继续消费”
  收紧成：
  - “PR workflow 天然允许继续消费 child files”
- 已补回归：
  - 新增单测覆盖“PR 任务无 subagent wording 也继续读第二个 child file”
  - 新增 seeded baseline `fixture-pr-followup-rust-cli-mini`
- review / 收尾：
  - baseline 首轮失败不是 merge-back 逻辑错误，而是 benchmark manifest 的 `expect_tool_sequence = ["..."]` 语法被当成原始字符串切分
  - benchmark parser 现已兼容 bracketed string arrays，与文档/示例保持一致
  - `fixture-pr-followup-rust-cli-mini` 也改成结果导向断言：
    - 只要求 follow-up `read_file` 真的读到第二个 child file
    - 不再把随后的一步 `list_files` 探索误判成失败
  - 修正后默认 baseline 回到 `34/34`，`pr_workflow` slice gate 恢复为 `pass vs 5 runs`

**10i-32 (`main`, 2026-05-08) — 已完成**：
- `pr_workflow` 的 child-file merge-back 又收紧了一层：
  - parent 在 PR follow-up 场景里把最后一个 `meta.child_files` 文件读完后
  - 现在会直接停止并总结
  - 不再掉回通用 `list_files` 探索
- 这一步把 `fixture-pr-followup-rust-cli-mini` 从“能继续读第二个 child file”推进成了“读完 child files 就收敛”
- 已补回归：
  - 新增单测覆盖“PR child files 全部消费后直接 Finish”
  - seeded baseline 断言也重新收紧：
    - `forbid_tool = "list_files"`
    - `max_tool_calls = 2`
- 验证结果：
  - 默认 benchmark 继续 `34/34`
  - `fixture-pr-followup-rust-cli-mini` trace 收到 `todo_write -> read_file`
  - 全量测试通过

**10i-33 (`main`, 2026-05-08) — 已完成**：
- `dogfood run` 现在支持 `--isolate-workdir`
- 打开后会把指定 `--workdir` 复制到临时目录执行：
  - live dogfood 任务可以直接跑 fixture-backed write/validate / pr_workflow
  - 同时不污染仓库内的固定 fixture
- 默认行为不变：
  - 不传 `--isolate-workdir` 时仍在原 workdir 直接运行
- 已补回归：
  - CLI 解析覆盖 `--isolate-workdir`
  - `prepare_run_workdir` 单测覆盖“隔离模式会复制 fixture”
- 已做真实 dogfood：
  - 用 `rust-write-mini` 跑通一条 isolated fixture-backed `pr_workflow` retry 任务
  - benchmark gate 继续通过
  - live dogfood 报表现在累计到 `5` 条记录，其中 `pr_workflow = 3`

**10i-34 (`main`, 2026-05-08) — 已完成**：
- `dogfood run` 现在支持 `--from-benchmark <case>`
- 这条路径会直接复用 benchmark manifest 里的：
  - `task`
  - `skill`
  - `budget`
  - `workdir`
  - `isolate_workdir`
  - `notes`
- 同时保持命令行 override 优先：
  - 如果手动传了 `--skill / --budget / --workdir / --isolate-workdir / --notes`
  - 就不会被 manifest 默认值覆盖
- review / 收尾：
  - 首轮真实 replay 暴露出 `workdir` 解析口径不一致：
    - benchmark case 的 `workdir` 是相对 manifest 目录
    - `dogfood run --from-benchmark` 一开始误按 repo 根目录解析
  - 现已修正为按 manifest 目录解析 inherited `workdir`
- 已补回归：
  - CLI 解析覆盖 `--from-benchmark` / `--manifest`
  - 单测覆盖“从 benchmark case 继承默认值”
  - 单测覆盖“显式命令行参数优先于 benchmark 默认值”
- 已做真实 dogfood：
  - `deepseek dogfood run --from-benchmark fixture-pr-retry-validate-rust-mini --benchmark-gate`
  - 已真实跑通一条 fixture-backed `pr_workflow` retry live run
  - 默认 benchmark gate 继续通过
  - live dogfood 报表现在累计到 `6` 条记录，其中 `pr_workflow = 4`

**10i-35 (`main`, 2026-05-08) — 已完成**：
- 新增 `deepseek dogfood replay-benchmark`
- 这条命令会批量重放 benchmark manifest 中“真正可 live replay”的 case：
  - 必须有真实 `workdir`
  - 必须没有 `seed_observations`
  - 可选 `--category`
  - 可选 `--limit`
  - 可选 `--benchmark-gate`
- 这一步的目标不是替代 benchmark，而是更快把：
  - `write_validate`
  - `pr_workflow`
  - 后续的其它 fixture-backed 类别
  的 live dogfood 历史做厚
- 已补回归：
  - CLI 解析覆盖 `replay-benchmark`
  - 单测覆盖“seeded-only case 不应进入 live replay”
  - 单测覆盖 `category + limit` 过滤
- 已做真实 replay：
  - `deepseek dogfood replay-benchmark --category write_validate --limit 2 --benchmark-gate`
  - 已真实追加两条 `write_validate` live 记录
  - 默认 benchmark gate 继续通过

**10i-36 (`main`, 2026-05-08) — 已完成**：
- `dogfood` 的失败判定现在不再只看 `ObservationStatus::Failed`
- 对于 `run_shell` 这类“工具调用本身成功返回，但结构化结果是失败”的场景：
  - 只要输出里有 `meta.result=failed`
  - dogfood ledger 也会把它计入 `failed_tool_calls`
  - 默认 `outcome` 也会从 `success` 修正成 `failed`
- review / 收尾：
  - 这条修复不是从单测里猜出来的，而是 `replay-benchmark` 的真实 replay 暴露的：
    - `fixture-recover-write-validate-rust-mini` 一开始被误记成 `success`
    - 修正后同一条 live run 已正确落成 `failed`
- 已补回归：
  - 新增单测覆盖“`meta.result=failed` 应计为 failed outcome”
- 修正后的 live dogfood 报表：
  - 现在累计 `9` 条记录
  - `write_validate = 3`，其中 `failed = 1`
  - `Benchmark seed candidates = 1`

**10i-37 (`main`, 2026-05-09) — 已完成**：
- `recovery` 现在不再只有一条自然 fixture replay 路径
- 默认 benchmark 新增：
  - `fixture-recover-empty-search-js`
  - 用小型 JavaScript CLI fixture 自然覆盖 `search_text -> list_files -> read_file`
- review / 验证：
  - 默认 benchmark 扩到 `35` 个 case，结果 `35/35`
  - 因为 case 数从 `34` 变成 `35`，benchmark trend gate 进入新的 warmup；这不是退化，而是 comparability 集合变化
  - 真实 replay 已跑：
    - `deepseek dogfood replay-benchmark --category recovery --limit 2`
  - replay 后 live dogfood 已真实追加两条 recovery 记录

**10i-38 (`main`, 2026-05-09) — 已完成**：
- `dogfood` 的 category 纠偏又补了一层：
  - 自然 search-miss fallback 类型任务
  - 例如 “if there are no matches inspect the repository layout instead”
  - 现在会被识别成 `recovery`
- 这一步的价值不是新增能力，而是把 live dogfood 报表里的 recovery slice 口径纠正到和 benchmark / replay 语义一致
- review / 收尾：
  - 首轮 replay 后，这两条自然 recovery 记录被旧 ledger 标签显示成了 `read_only`
  - 修正后：
    - `benchmark_case_category` 会把旧 `read_only` 标签纠偏到 `recovery`
    - `dogfood report` 已重新渲染，分类正确
- 验证结果：
  - 全量测试通过，`434 passed, 0 failed`
  - 最新 dogfood 报表累计 `11` 条记录
  - 其中：
    - `pr_workflow = 4`
    - `recovery = 2`
    - `write_validate = 3`
    - `read_only = 2`

**10i-39 (`main`, 2026-05-09) — 已完成**：
- `recovery` 现在新增了一条非 seeded 的自然 failing-test fixture：
  - `fixture-recover-failing-js-test`
  - 基于新的 `js-cli-failing-mini`
  - 真实覆盖 `run_shell -> read_file`
- 这轮不是只加 fixture，也顺手把两处真实误配收紧了：
  - skill auto-select 不再把 failing-test / lint recovery 任务误路由到 `research`
  - offline planner 对这类任务会先复现失败，再读 failing file，然后直接停止，不再漂移到无根据的 `search_text`
- review / 收尾：
  - 首轮 benchmark 暴露出这条任务先被 `research` skill 吃掉，甚至把 `npm test` 跑成 policy denied
  - 修正后：
    - auto-skill 会优先落到 `debug`
    - plan 的首步会变成 `Reproduce the failing validation command`
    - 真实 dogfood 已稳定走到 `run_shell -> read_file -> finish`
- 验证结果：
  - 默认 benchmark 扩到 `36/36`
  - trend gate：`pass against 4 comparable runs`
  - 全量测试通过，`438 passed, 0 failed`

**10i-40 (`main`, 2026-05-09) — 已完成**：
- `dogfood` 的 category 纠偏又补了一层：
  - 像 “investigate why npm test fails ...” 这种 failure repro / readback 任务
  - 即使 trace 里有 `run_shell`
  - 只要没有 `apply_patch`，并且本质是 recovery
  - 就不再被误记成 `write_validate`
- 同时 benchmark baseline 也和新 planner 行为对齐了：
  - `recovery_readback_then_search` 这组旧 case 不再强行要求 `read_file -> search_text`
  - 现在按“readback 后即可停止”的新语义判定
- review / 收尾：
  - 首轮 dogfood report 暴露出 3 条历史 failing-test replay 被错误压进了 `write_validate`
  - 修正后：
    - `dogfood report` 已重新渲染
    - recovery slice 现在累计 `7` 条，其中 `failed = 3`
    - write_validate slice 回落到真正的 patch+validate 任务
- 验证结果：
  - 最新 dogfood 报表：
    - `recovery = 7`
    - `write_validate = 3`
    - `pr_workflow = 4`
    - `read_only = 2`
  - 全量测试通过，`439 passed, 0 failed`

状态：10e-1 + 10e-2 + 10e-3 + 10e-4 + 10e-5 + 10f-1 + 10f-2 + 10f-3 + 10f-4 + 10g-1 + 10g-2 + 10g-3 + 10g-4 + 10h-1 + 10h-2 + 10h-3 + 10h-4 + 10h-5 + 10h-6 + 10h-7 + 10i-1 + 10i-2 + 10i-3 + 10i-4 + 10i-5 + 10i-6 + 10i-7 + 10i-8 + 10i-9 + 10i-10 + 10i-11 + 10i-12 + 10i-13 + 10i-14 + 10i-15 + 10i-16 + 10i-17 + 10i-18 + 10i-19 + 10i-20 + 10i-21 + 10i-22 + 10i-23 + 10i-24 + 10i-25 + 10i-26 + 10i-27 + 10i-29 + 10i-30 + 10i-31 + 10i-32 + 10i-33 + 10i-34 + 10i-35 + 10i-36 + 10i-37 + 10i-38 + 10i-39 + 10i-40 完成

## Phase 11 进展（Claude / Codex gap closure）

**11a-1 (`main`, 2026-05-09) — 已完成**：
- `deepseek` 已收敛为主入口：
  - `Cargo.toml` 默认运行目标切到 `deepseek`
  - `deepseek` / `deepseek chat` / `deepseek repl` / `deepseek interactive` 同入口
  - 主文档、PR/CI 文档、streaming/todos 文档、关键 runtime 提示统一为 `deepseek`
  - `dscode` 退回兼容别名，不再作为主品牌展示
- review / 收尾：
  - 非 TTY 交互提示已统一为 `deepseek`
  - `doctor` / `smoke` / `pr patch` 的用户可见字符串已完成品牌切换
- 验证结果：
  - 针对 `deepseek` 入口别名与 REPL binary-name 的单测通过
  - 后续全量测试已覆盖在本轮 Phase 11 review 里

**11c-1 (`main`, 2026-05-09) — 已完成**：
- natural failure repro recovery 再收紧一层：
  - 对 `investigate why ... test fails` 这类任务
  - 只要有 `suggested_test_command`
  - planner 就会优先 `run_shell` 复现失败，而不是先漂到 `search_text` / `list_files`
- 这轮直接修掉了 benchmark 里唯一的 natural recovery miss：
  - `fixture-recover-failing-js-test`
  - 现在稳定走 `run_shell -> read_file -> finish`
- review / 收尾：
  - 默认 benchmark 从 `35/36` 回到 `36/36`
  - trend gate 恢复为通过
- 验证结果：
  - 新增 recovery 回归单测，覆盖“即使已有 repo signal 也要先 repro”
  - benchmark：`36/36`

**11b-1 (`main`, 2026-05-09) — 已完成**：
- `pr_workflow` baseline 新增真实 second-round review feedback fixture：
  - [`rust-review-feedback-mini`](/home/willamhou/codes/DeepseekCode/.dscode/fixtures/rust-review-feedback-mini/Cargo.toml)
  - 初始状态模拟“上一轮错误 patch 已经落在工作区”
  - 当前任务再执行一次真实 `apply_patch -> git_diff -> run_shell`
- 新增 benchmark case：
  - `fixture-pr-second-round-feedback-rust-mini`
  - 这让 `pr_workflow` 不再只有 patch / retry / child-file follow-up，还多了一类真实 follow-up repair
- review / 收尾：
  - 默认 benchmark 扩到 `37` 条 case
  - `pr_workflow` slice 扩到 `10` 条 case
- 验证结果：
  - benchmark：`37/37`
  - trend gate：因 case 数变化进入新的 warmup，属预期行为

**11b-2 (`main`, 2026-05-09) — 已完成**：
- `pr_workflow` baseline 继续补厚，新增真实 JavaScript PR fix+validate case：
  - `fixture-pr-fix-validate-js-cli-failing-mini`
  - workdir 复用自然 failing fixture [`js-cli-failing-mini`](/home/willamhou/codes/DeepseekCode/.dscode/fixtures/js-cli-failing-mini/package.json)
  - 真实链路稳定为 `apply_patch -> git_diff -> run_shell`
- 这轮顺手收紧了 direct edit parser：
  - `trim_edit_path_suffix` 现在支持 `and rerun` / `then rerun`
  - `derive_edit_request` 会取最后两个 quoted segment，避免 PR / CI task 前半段的反引号噪声污染替换对
- review / 收尾：
  - 首轮 benchmark 暴露出新 case 漂成 `list_files -> read_file -> apply_patch -> run_shell`
  - 修正后回到标准 direct-edit validate 路径
- 验证结果：
  - 默认 benchmark：`38/38`
  - 全量测试：`449 passed, 0 failed`

**11c-2 (`main`, 2026-05-09) — 已完成**：
- `dogfood` 对“诊断型成功”已真实打通：
  - failure repro / readback 任务现在可记录 `diagnostic_expected_failure=true`
  - 自然 JS failing-test replay 已真实写入 ledger
  - 该类 run 不再一律算作 `failed`
- 最新 dogfood report：
  - `Runs: 17`
  - `Diagnostic expected-failure rate: 1/17 (5.9%)`
  - `recovery: 8 runs, 5 success, 1 diagnostic, 3 failed`
- 验证结果：
  - `deepseek dogfood run --from-benchmark fixture-recover-failing-js-test`
  - `deepseek dogfood report --out /tmp/deepseek-dogfood-phase11.md`

**11e-1 (`main`, 2026-05-09) — 已完成**：
- `dogfood run --from-benchmark` 在 isolated fixture replay 场景下现在会临时开启：
  - `DSCODE_AUTO_APPROVE_WRITES=1`
  - `DSCODE_AUTO_APPROVE_SHELL=1`
- 作用范围是刻意收窄的：
  - 只对 benchmark replay 生效
  - 只对 isolated fixture workdir 生效
  - 普通 dogfood / 手工 run 不改 approval 语义
- 这让 live replay 测到的是 agent workflow，而不是非交互 confirm prompt
- review / 收尾：
  - 同一条 `fixture-pr-second-round-feedback-rust-mini` dogfood replay
  - 修复前会因为非交互 auto-deny 记成 `failed`
  - 修复后稳定走 `apply_patch -> git_diff -> run_shell` 并写成 `success`
- 验证结果：
  - 新增 `benchmark_replay_auto_approve_env_is_temporary` 单测
  - 全量测试：`448 passed, 0 failed`
- 最新 live dogfood：
    - `Runs: 20`
    - `pr_workflow: 7 runs, 6 success, 1 failed`

**11f-1 (`main`, 2026-05-09) — 已完成**：
- 新增产品级最小版本/安装入口：
  - `deepseek version`
  - `deepseek --version`
  - `deepseek -V`
- CLI 不再只靠 README 猜版本或 binary 来源，安装后可直接做：
  - `deepseek version`
  - `deepseek doctor`
- 文档新增：
  - [安装指南](./install.md)
  - README 增加 `cargo install --path .` 的快速开始
- review / 收尾：
  - 版本命令输出当前包版本，例如 `deepseek 0.1.0`
  - 解析覆盖 subcommand 与 flag 两条入口
- 验证结果：
  - `cargo run --bin deepseek -- version`
  - `cli_from_argv_routes_version_subcommand`
  - `cli_from_argv_routes_version_flags`

**11d/11e/11f 收口前状态回填（2026-05-09）**：
- 本轮重新跑默认 benchmark 后，当前真实 baseline 是：
  - cases：`39`
  - passed：`38/39`
  - total tool calls：`127`
  - total failed tools：`0`
  - trend gate：`skipped`，因为 39-case comparable history 只有 1 条
- 当前唯一 benchmark 红点：
  - `fixture-pr-reproduce-fix-rust-cli-failing-mini`
  - trace 已到 `run_shell -> read_file -> apply_patch -> git_diff`
  - 缺口是 patch 后没有最后一次 `run_shell` validate
- 当前 live dogfood snapshot：
  - `Runs: 20`
  - `Success: 15`
  - `Failed: 5`
  - `Stuck: 0`
  - `Manual: 0`
  - `pr_workflow: 7 runs, 6 success, 1 failed`
  - `write_validate: 6 runs, 2 success, 4 failed`
- 这说明 Phase 11 后半段不能只补文档：
  - `11d` 要把 subagent v2 的 child summary / next-action / parent merge-back 收成稳定契约
  - `11e` 要让 benchmark failed expectation 和 dogfood 新增 live failure 都能非零退出，真正成为阻断门禁
  - `11f` 要把 release / upgrade / rollback 路径写清楚，而不是停留在 `cargo install --path .`

**11d-1 (`main`, 2026-05-09) — 已完成**：
- subagent v2 的 summary schema 增加 `meta.child_next_action`：
  - `read_file:<path>`
  - `search_text:<query>`
  - `replan_parent`
  - `continue_parent`
- parent planner 现在优先消费 `meta.child_next_action`：
  - `read_file:<path>` 会排在 `meta.child_files` 前面
  - `search_text:<query>` 会排在 child final message 的自由文本猜测前面
- `dispatch_subagent` 文本摘要也显示 `child next action`，方便人工看 trace
- 新增 seeded baseline：
  - `subagent-next-action-mergeback`
  - 目标是验证 parent 能按 child next-action 继续读回关键文件
- 验证结果：
  - targeted tests 覆盖 next-action summary / parent merge-back
  - benchmark subagent slice 扩到 `2` 条 case

**11e-2 (`main`, 2026-05-09) — 已完成**：
- benchmark gate 现在真正具备阻断能力：
  - 任意 benchmark case expectation 失败都会让 `deepseek benchmark` 非零退出
  - 不再只是在 markdown report 里显红
- live gate 已接入 benchmark：
  - 当前 benchmark run 会读取 dogfood ledger snapshot
  - 与最近一次 benchmark 保存的 dogfood snapshot 对比
  - 如果 failed / stuck / manual 计数增加，gate 失败并非零退出
  - category 级 failed / stuck / manual 增量也会写进失败原因
- report 新增：
  - `Live gate: ...`
  - 失败原因会明确写出是 overall 还是某个 category 增加
- 同步修复了 `fixture-pr-reproduce-fix-rust-cli-failing-mini`：
  - repro-first 任务里的第一次 `run_shell` 不再误抵消 patch 后 validation
  - 现在稳定走 `run_shell -> read_file -> apply_patch -> git_diff -> run_shell`
- 验证结果：
  - 默认 benchmark：`40/40`
  - trend gate：`pass against 4 comparable runs`
  - live gate：`pass (no new dogfood records since previous snapshot, runs=20)`
  - 全量测试：`463 passed, 0 failed`

**11f-2 (`main`, 2026-05-09) — 已完成**：
- 安装文档从“能安装”补到完整 release / upgrade story：
  - 发布前检查：`cargo fmt --check` / `cargo test` / `deepseek benchmark` / `deepseek version` / `deepseek doctor`
  - release binary 路径：`cargo build --release`
  - 发布产物应包含 binary、commit SHA、版本输出、平台说明、安装升级说明
  - 源码安装升级：`git pull` + `cargo install --path . --force`
  - release binary 升级前保留 rollback copy
  - 回滚流程覆盖 binary 回滚与源码 commit 回滚
- README 快速开始补了源码升级命令，避免用户只知道首次安装。

## 建议的下一个顺序

当前这一轮按顺序列出的 11d / 11e / 11f 收口任务已经完成。下一阶段更值得做的是：

1. 继续补真实 PR / review / CI fix 方向的 fixture 或 live dogfood 样本，而不是只做仓库内局部代码任务
2. 把 `pr_workflow` 从“已有 Rust/JS patch + validate + retry + child-file follow-up”继续推向更完整的 live follow-up / merge-back 链路，并单独排查 Python retry 的 baseline 稳定性
3. 继续积累非 `read_only` 的 live dogfood category history，尤其是 `pr_workflow / write_validate / recovery`

**Phase 11+ baseline hardening (`main`, 2026-05-09) — 已完成**：
- 按上述下一阶段顺序，先补 Python retry baseline，确认不是只靠 Rust / JavaScript 路径过关：
  - 新增 `fixture-retry-write-validate-python-mini`
  - 新增 `fixture-pr-retry-validate-python-mini`
- 两条 case 都跑在 isolated `fixtures/python-write-mini` 上，覆盖：
  - `apply_patch -> git_diff -> run_shell -> read_file -> apply_patch -> git_diff -> run_shell`
  - pytest 失败后的 readback 与 corrective patch
  - 普通 `write_validate` 和 PR review feedback 语境下的 retry
- 最新 benchmark：
  - 默认 benchmark：`42/42`
  - total tool calls：`145`
  - failed tool calls：`0`
  - trend gate：`pass against 3 comparable runs`
  - live gate：`pass (no new dogfood records since previous snapshot, runs=22)`
- 已把两条 Python retry baseline replay 成 live dogfood：
  - `fixture-retry-write-validate-python-mini` 记为 `write_validate / success`
  - `fixture-pr-retry-validate-python-mini` 记为 `pr_workflow / success`
  - live dogfood 当前累计 `22` runs，`17` success，`5` failed，`0` stuck，`0` manual
- replay 过程中暴露并修正了一个 dogfood 口径问题：
  - retry 流里的第一次 failed validation 不再把最终通过的任务误记成 `failed`
  - `failed_tool_calls` 仍然保留中间失败次数，用于诊断和 seed 候选判断
  - benchmark snapshot 与 dogfood report 现在共用同一套 category 纠偏；category 重分桶不会在没有新增 run 时误触发 live gate
- 这一步把 roadmap 里“单独排查 Python retry baseline 稳定性”的风险点收掉，并把对应 live dogfood 样本补进 ledger。

## 最近里程碑

- `d9b3ae4` `Initialize project docs`
- `589a5c6` `Bootstrap Rust CLI scaffold`
- `5cd434a` `Implement basic repository tools`
- `f20534f` `Add offline planning loop`
- `3a8d633` `Wire skills into CLI flow`
- `6d01256` `Add DeepSeek transport and policy enforcement`
- `efdb191` `Upgrade patching and remote protocol parsing`
- `a1c45fb` `Use tool calling for OpenAI-compatible DeepSeek`
- `046106c` `Document roadmap and project status`
