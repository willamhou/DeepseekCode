# Roadmap 与状态

最后更新：`2026-04-30`

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

- 支持：
  - `dscode`
  - `dscode "task"`
  - `dscode run "task"`
  - `dscode diff`
  - `dscode resume`
  - `dscode config`
  - `dscode doctor`
  - `dscode smoke`（支持 `--flavor openai|anthropic` 与 `--prompt`）
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
  - 通过 `cp .dscode/config.example.toml .dscode/config.toml && dscode doctor` 验证可解析

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
  - `dscode pr review <pr>` —— 只读 review，输出 markdown 到 stdout / 文件 / `gh pr comment`
  - `dscode pr fix <pr>` —— 抓首个失败 CI job，本地复现并迭代修复（12 步预算）
  - `dscode pr patch <pr>` —— 提改动到工作区；`--commit` 在干净工作区时自动 commit（不 push）
  - 三命令共享 `gh auth` 检查、PR 上下文获取、prefilled observations 注入
  - 所有写入与 shell 仍走 P3 confirm
  - `dscode doctor` 新增 `[github]` 段，显示 `gh` 版本与 auth 状态
- 更强语言特化：未开始
- IDE 集成：未开始
- 多 agent：未开始

状态：进行中（PR/CI 一项基础版完成）

### Phase 9: 交互式体验

- REPL (`dscode chat`)：v1 已完成
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

## 建议的下一个顺序

建议严格按下面顺序推进：

1. ~~`doctor` 扩展~~（已完成）
2. ~~`smoke` 命令~~（已完成）
3. ~~`.dscode/config.toml` 示例文件~~（已完成）
4. ~~`Anthropic-compatible` 正式 tool use~~（已完成）
5. ~~`apply_patch` 多文件和失败诊断~~（已完成）
6. ~~planner 生成 patch 模式编辑~~（已完成）
7. ~~patch 应用后自动 git_diff 复核与失败重试~~（已完成基础版）
8. ~~审批交互~~（已完成基础版）
9. ~~observation / context 管理增强~~（已完成基础版）

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
