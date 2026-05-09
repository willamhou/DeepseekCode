# Claude/Codex Gap Closure — Phase 11 设计

最后更新：`2026-05-09`
状态：`active follow-up`（Phase 11 已收口，Phase 11+ 按 gap review 继续推进）
关联 Phase：11（把 `DeepseekCode` 从强原型推进到可长期主用的 CLI code agent）

## 背景

截至 `2026-05-09`，`DeepseekCode` 已完成：

- `deepseek` 直接进入交互 REPL
- explicit planning / todo execution
- skill schema v2 + auto-select
- subagent v1 + parent/child merge-back
- recovery / replan / failed-validation retry
- fixture-backed benchmark
- dogfood ledger / promotion / trend gate / category slices

当前基线（2026-05-09 Phase 11+ Go PR CI reproduce fixture 后复测）：

- benchmark：`47/47`
- 全量测试：`540 passed, 0 failed`
- benchmark trend gate：`skipped (need at least 3 prior comparable runs, found 0)`，原因是默认 baseline 从 `46` 条扩到 `47` 条，当前没有同 case 数历史
- dogfood live gate：`pass (no new dogfood records since previous snapshot, runs=33)`
- 当前已收掉的红点：
  - `fixture-pr-reproduce-fix-rust-cli-failing-mini` 已稳定为 `run_shell -> read_file -> apply_patch -> git_diff -> run_shell`
  - Python retry baseline 已补齐到 `write_validate` 和 `pr_workflow`
  - retry 后最终验证通过的 dogfood run 不再因中间 failed validation 被误记为 `failed`
  - benchmark snapshot 与 dogfood report 的 category 纠偏口径已统一
  - benchmark case expectation 失败现在会非零退出
  - dogfood live gate 会阻断上次 benchmark snapshot 后新增的 failed / stuck / manual live 记录
- live dogfood 主要 category：
  - `pr_workflow`
  - `recovery`
  - `write_validate`
  - `read_only`
  - `diagnostic expected-failure`（已在 recovery replay 中出现）

当前 live snapshot：

- dogfood：`33 runs`
- `pr_workflow`: `8 runs`, `7 success`, `1 failed`
- `recovery`: `10 runs`, `6 success`, `3 failed`, `1 stuck`
- `write_validate`: `13 runs`, `9 success`, `4 failed`
- `read_only`: `2 runs`, `2 success`

当前已完成的 Phase 11 进展：

- `11a`：`deepseek` 已成为主入口，主文档和关键运行时提示已统一到 `deepseek`，`dscode` 退回兼容别名
- `11b`：`pr_workflow` baseline 已覆盖 seeded review/fix/patch、CI lint/test、second-round feedback 和 Rust / JavaScript / Python / Go fixture-backed patch/fix/retry/reproduce cases
- `11b`：`pr_workflow` baseline 继续新增真实 Python 与 Go PR reproduce+fix+validate case，当前 category 为 `16` 条 case
- `11c`：natural JS failing-test recovery 已收紧为 `run_shell -> read_file -> finish`，benchmark 回到全绿；dogfood 也已把这类诊断型成功单独记账
- `11d`：subagent v2 收口到 `meta.child_next_action`，parent 可按机器可读 next-action 消费 child summary
- `11e`：`dogfood run --from-benchmark` 在 isolated fixture replay 场景下会临时开启 auto-approve，避免非交互审批把 live replay 误记成 workflow 失败
- `11e`：benchmark expectation failure 和 dogfood 新增 failed/stuck/manual live record 都会成为阻断条件
- `11f`：新增 `deepseek version` / `--version` / `-V`，并补了独立安装文档 `docs/install.md`
- `11f`：安装文档补齐 release / upgrade / rollback story
- Phase 11+ baseline hardening：新增 Python retry benchmark：
  - `fixture-retry-write-validate-python-mini`
  - `fixture-pr-retry-validate-python-mini`
- Phase 11+ live dogfood：上述两条 Python retry case 已 replay 到 live ledger，均为 `success`
- Phase 11+ live gate hardening：
  - `deepseek benchmark` 不再在 gate 失败时自动推进 benchmark history baseline
  - 新增 `deepseek benchmark --accept-live-baseline`，只有显式接受后才会把已排查的 live dogfood snapshot 作为新基线
  - 最新普通 benchmark：`42/42`，trend gate pass，live gate pass（accepted baseline 后 runs=33，无新增 dogfood 记录）
  - re-analysis：本地 CLI agent loop / recovery / PR fixture 基线差距已收敛到“小到中”；真实在线模型稳定性、IDE 配套、外部 PR/CI live 样本厚度仍不是“小差距”
- Phase 11+ custom slash commands：
  - REPL 支持 `.dscode/commands/*.md` 与用户级 `~/.config/dscode/commands/*.md`
  - 支持 `/name args`、namespace（如 `/pr/fix`）和 `$ARGUMENTS` / `$0` 参数替换
  - 对齐 Claude Code prompt-backed custom commands 的核心使用方式，降低常用 workflow 复用成本
- Phase 11+ workspace instructions：
  - agent loop 启动时读取用户级 `~/.config/dscode/AGENTS.md`
  - 项目级从 git root 到当前目录逐层读取 `AGENTS.override.md` / `AGENTS.md` / `CLAUDE.md` / `.claude/CLAUDE.md`
  - 每个文件最多注入 32 KiB，并在 system prompt 中标注来源路径
  - 对齐 Codex `AGENTS.md` 与 Claude Code `CLAUDE.md` 的基础项目记忆/团队规则入口
- Phase 11+ local hooks：
  - 默认关闭，通过 `hooks.enabled = true` 显式启用
  - 支持 project/user hook dirs 与 `user_prompt_submit` / `pre_tool_use` / `post_tool_use`
  - hook scripts 通过 stdin JSON payload 获取上下文
  - prompt submit / pre-tool hook 可阻断，post-tool hook 只作为 advisory observation
  - 对齐 Claude Code / Codex hook 扩展面的最小本地策略与审计能力
- Phase 11+ config bootstrap：
  - 新增 `deepseek config init [--force]`
  - 自动创建 `.dscode/config.toml`、sessions、custom command 目录和 hooks 事件目录
  - `config --print-default` 覆盖 workspace user dirs / instruction file / hooks 字段
  - 把首次配置从手动复制模板降低为命令式初始化
- Phase 11+ live coverage gate：
  - 当 live dogfood snapshot 达到 `12` 条 run 后，普通 benchmark 还会要求关键 live slice 保持最小覆盖
  - 当前要求 `pr_workflow` / `recovery` / `write_validate` 各至少 `3` 条 run
  - 这避免“总量看起来健康，但关键 workflow 缺样本”的 snapshot 被误当成产品级 baseline
- Phase 11+ benchmark asset reproducibility / Go baseline：
  - 默认 benchmark manifest、example manifest 和 fixture corpus 已解除 ignore 并进入版本控制
  - 新增 `fixtures/go-write-mini`
  - 新增 `fixture-write-validate-go-mini` 与 `fixture-pr-patch-validate-go-mini`
  - 默认 baseline 从 `42` 条扩到 `44` 条，Rust / JavaScript / Python 后开始覆盖 Go 的 write+validate 与 PR patch+validate
- Phase 11+ PR/CI fixture thickening：
  - 新增 `fixtures/python-cli-failing-mini`
  - 新增 `fixture-pr-reproduce-fix-python-cli-failing-mini`
  - 默认 baseline 从 `44` 条扩到 `45` 条，PR/CI 自然失败修复链路已有 Rust / JavaScript / Python 样本
- Phase 11+ Go PR CI reproduce fixture：
  - 新增 `fixtures/go-cli-failing-mini`
  - 新增 `fixture-pr-reproduce-fix-go-cli-failing-mini`
  - 默认 baseline 从 `46` 条扩到 `47` 条，PR/CI 自然失败复现修复链路已覆盖 Rust / JavaScript / Python / Go
- Phase 11+ ambiguous improvement planning guard：
  - explicit planning heuristic 会把短句 `improve` / `enhance` / `stabilize` / `hardening` / `optimize` / `better` 类模糊改进请求纳入 first-turn todo plan
  - 新增 `plan-ambiguous-improvement` benchmark，覆盖 `improve benchmark reliability` 这类没有路径和明确编辑指令的 open-ended 请求
  - 默认 baseline 从 `45` 条扩到 `46` 条，planning category 对短模糊任务也有回归样本
- Phase 11+ subagent edited-file handoff：
  - `dispatch_subagent` summary 现在会从 child 的 `apply_patch` 输入/输出和 `git_diff` 输出提取 touched files
  - child 修改文件后会生成 `meta.child_files` 与 `meta.child_next_action=read_file:<path>`，让 parent loop 有明确 readback 入口
  - 这把 subagent merge-back 从“只会回传读取文件”推进到“也能回传 child patch/diff 的编辑文件”
- Phase 11+ IDE bootstrap：
  - 新增 `editors/vscode` 最小扩展雏形
  - 支持从 VS Code 命令面板启动 `deepseek` chat / task / benchmark / dogfood report
  - 可把当前文件路径和选中文本作为 `deepseek run` 的上下文
  - 这只把 IDE gap 从“没有入口”推进到“可试用入口”，距离 Claude/Codex 的完整 IDE/app 体验仍是大差距
- Phase 11+ VS Code quick actions：
  - VS Code extension 新增状态栏 `DeepseekCode` 入口
  - 新增 `DeepseekCode: Quick Action` quick-pick，可从一个入口启动 chat / task / selection explain / benchmark / dogfood report
  - 新增 editor title 和 editor context menu 入口，提高 explain selection / run task 的可发现性
  - extension manifest 为常用命令补齐 product icons，并继续保持无外部 npm dependency
- Phase 11+ MCP config surface：
  - 新增 `deepseek mcp init|list|doctor`
  - 支持项目级 `.dscode/mcp.json` 与用户级 `~/.config/dscode/mcp.json`
  - 支持常见 `mcpServers` JSON object 的 server 发现与 schema 校验
- Phase 11+ MCP stdio tool discovery：
  - 新增 `deepseek mcp tools [server]`
  - 按 MCP lifecycle 对 stdio server 执行 `initialize` / `notifications/initialized` / `tools/list`
  - 可展示远端 tool name / description / input schema，并支持 `nextCursor` 分页
- Phase 11+ MCP manual tool call：
  - 新增 `deepseek mcp call <server> <tool> [json-args]`
  - 对 stdio server 执行 MCP `tools/call`，支持 JSON object arguments
  - 可展示 text content、structuredContent 和 tool-level `isError`
- Phase 11+ MCP agent bridge：
  - 当 project/user MCP config 文件存在时，agent registry 会暴露 `mcp_list_tools` 与 `mcp_call`
  - `mcp_list_tools` 让模型枚举 configured MCP server tools 和 schema
  - `mcp_call` 让模型通过 JSON object arguments 调用 stdio / HTTP / SSE MCP tools
  - 后续已补 opt-in 动态 tool 注入初版；完整 schema 注入、permission UX 和 plugin ecosystem 仍不是小差距
- Phase 11+ MCP call approval/allowlist policy：
  - 新增 `approval.require_mcp_confirmation`，默认 `true`
  - 新增 `approval.mcp_call_allowlist`，支持 `server/tool`、`server/*`、`*/tool` 和 `*/*`
  - 新增 `DSCODE_AUTO_APPROVE_MCP=1`
  - agent `mcp_call` 调用远端 MCP tool 前会确认 `server/tool`
  - `mcp_list_tools` 保持只读发现能力；用户直接执行的 `deepseek mcp call ...` 不重复走 agent 审批
- Phase 11+ MCP HTTP JSON-RPC transport：
  - `deepseek mcp tools [server]` 可对 `http` / `streamable-http` server 执行 `tools/list`
  - `deepseek mcp call <server> <tool> [json-args]` 可通过 HTTP JSON-RPC POST 执行 `tools/call`
  - 会续传服务端返回的 `Mcp-Session-Id`
  - HTTP response 如为 `text/event-stream` 形态，会读取 `data:` 中的 JSON-RPC response
  - agent bridge 复用同一路径，因此 HTTP MCP server 也可被 `mcp_list_tools` / `mcp_call` 使用，并继续受 confirmation / allowlist 保护
- Phase 11+ MCP legacy SSE transport：
  - `deepseek mcp tools [server]` 可对旧式 `sse` server 打开 event stream、读取 `endpoint` 事件，再向 endpoint POST JSON-RPC
  - `deepseek mcp call <server> <tool> [json-args]` 可通过同一 SSE session 执行 `tools/call`
  - SSE stream 上的 JSON-RPC response 会按 request id 匹配，忽略 endpoint / heartbeat / 非目标 response
  - agent bridge 复用同一路径，因此 SSE MCP server 也可被 `mcp_list_tools` / `mcp_call` 使用，并继续受 confirmation / allowlist 保护
- Phase 11+ opt-in MCP dynamic tool exposure：
  - 新增 `mcp.expose_remote_tools`，默认 `false`，避免 agent 启动时隐式执行不受信任的 MCP server discovery
  - 开启后，agent registry 会发现 enabled MCP servers 的远端 tools，并以 `mcp__server__tool` 名称注入为独立 agent tool
  - 动态 tool 复用 `deepseek mcp call` 的 stdio / HTTP / SSE 调用路径，参数为 `arguments` JSON object string
  - 动态 tool 继续按真实 `server/tool` 走 `approval.require_mcp_confirmation` 与 `approval.mcp_call_allowlist`
  - 单次最多注入 `24` 个动态 MCP tools，启动发现失败的 server 会被跳过，避免单个坏 server 阻断整个 agent registry

本轮收口顺序：

1. 回填 roadmap/spec 与实测状态，承认 `38/39` 红点
2. 收 `11d`：subagent v2 的 child summary / next-action / merge-back 语义
3. 收 `11e`：benchmark failed expectation 与 dogfood 新增 live failure 都必须能阻断
4. 收 `11f`：release / upgrade story 从“能安装”补到“能发布、能升级、能回滚”

当前结果：Phase 11 主体与后续 baseline hardening / custom slash commands / workspace instructions /
local hooks / config bootstrap / live coverage gate / benchmark asset reproducibility / IDE bootstrap / VS Code quick actions / MCP config surface / MCP stdio tool discovery / MCP manual tool call / MCP agent bridge / MCP call approval/allowlist policy / MCP HTTP JSON-RPC transport / MCP legacy SSE transport / opt-in MCP dynamic tool exposure / Python PR CI fixture thickening / Go PR CI reproduce fixture / ambiguous improvement planning guard / subagent edited-file handoff 已收口，最新 benchmark 为 `47/47`，全量测试为 `540 passed, 0 failed`。本轮 trend gate 因 case 数从 `46` 到 `47` 进入 comparability warmup，live gate 继续通过。

这说明 `DeepseekCode` 已经不是“演示级原型”，但仍明显低于 Claude Code / Codex 的
产品完成度。差距不再是“有没有 planner / tool loop”，而是：

1. 真实/外部 PR / CI / review live 样本不够厚
2. open-ended / ambiguous task 的默认稳定性不够
3. subagent orchestration 仍是单层、保守的 merge-back
4. IDE / 编辑器配套已有 VS Code command palette / status bar / quick action / context menu 的轻量入口，MCP/plugin 生态已有配置发现、stdio/HTTP/SSE `tools/list`、manual `tools/call`、generic agent bridge、bridge 级审批/allowlist 和 opt-in 动态 tool 注入初版，但完整 IDE agent 体验、完整 schema 注入、更完整 permission UX、plugin 生态和云端/外部任务面仍缺失
5. live online-model 稳定性与外部 PR/CI 样本厚度还不足以宣称产品级

## 差距表

| 维度 | `DeepseekCode` 当前状态 | Claude Code / Codex 目标状态 | 差距等级 |
|---|---|---|---|
| 命令行入口 | `deepseek` 已可直接进入 REPL | 默认心智一致、文档和错误提示完全统一 | 小 |
| REPL / 交互体验 | transcript、slash、session 已有 | 更顺滑的 history、恢复、帮助、默认提示 | 中 |
| 单仓库本地 coding flow | benchmark 覆盖面已较强 | 默认成功率更高，少漂移、少无效 hops | 中 |
| 本地扩展 / 策略入口 | custom commands、workspace instructions、local hooks、MCP config + stdio/HTTP/SSE tools/list/call + generic agent bridge + bridge 级 MCP 审批/allowlist + opt-in 动态 MCP tool 注入已有 | 更完整的 MCP/plugin ecosystem 与团队级扩展面 | 中 |
| open-ended 任务 | 已有 recovery / replan / ambiguous improvement first-turn plan guard，但仍依赖 heuristic | 对模糊任务也能稳定收敛 | 中到大 |
| PR / CI 工作流 | `pr review/fix/patch` 已有 + `16` 条 fixture baseline，Rust / JavaScript / Python / Go 均已有 PR 向修复样本 | 更厚的真实/外部 PR/CI 样本与稳定端到端闭环 | 中 |
| subagent | 已能 dispatch / merge-back，并能把 child patch/diff touched files 回传给 parent readback | 更成熟的拆分、归并、去重、收敛 | 中到大 |
| live 回归体系 | benchmark + dogfood 已闭环，且有关键 slice 覆盖下限 | 更厚的外部/在线 live baseline，且可阻断回归 | 小到中 |
| 安装 / 分发 | install guide、version、completion、config init 已有 | 普通用户开箱即装即用 | 小到中 |
| IDE / 编辑器配套 | VS Code terminal launcher + status bar / quick action / context menu | 统一的产品体验 | 中到大 |
| 默认产品完成度 | 强原型 | 可长期主用的产品级工具 | 大 |

## 目标

Phase 11 的目标不是“功能数量翻倍”，而是把现有能力推进到更稳定、更像产品的状态：

- `deepseek` 对新用户来说就是默认入口，不需要再解释命令心智
- PR / CI / review 成为真正高价值主线路，而不只是 demo 路径
- recovery / replan / subagent 更少依赖偶然的 prompt luck
- benchmark 和 dogfood 成为真正的质量门禁
- 为后续 packaging / install / public release 铺平道路

## 非目标 (Phase 11)

- 完整 VS Code / JetBrains 插件开发（本轮只做最小 terminal launcher）
- 云端多租户服务
- skill marketplace / remote plugin ecosystem
- 多模型路由平台化
- 完整 GUI

这些都可以在 Phase 12+ 讨论，但不应阻塞本 phase 的 CLI 产品化。

## 锁定的设计决策

1. **优先级**：先补真实工作流和稳定性，再补纯 UX 装饰
2. **主入口**：`deepseek` 为唯一主品牌；`dscode` 只保留兼容角色
3. **衡量方式**：以 benchmark + live dogfood + PR workflow baseline 为准，而不是凭感觉说“更像了”
4. **subagent 方向**：继续做“更稳的单层 orchestration”，不急着上多层 agent swarm
5. **PR / CI 路线**：先做 fixture-backed + live replay 做厚，再扩大到更真实外部 repo 样本
6. **发布门禁**：quality gate 先服务本仓库与 dogfood，不急于第一时间接 GitHub release pipeline

## 架构总览

Phase 11 拆成 6 条 workstream：

1. `11a` Product Entry And UX
2. `11b` Real PR / CI Workflow Hardening
3. `11c` Recovery And Open-Ended Stability
4. `11d` Subagent v2
5. `11e` Live Quality Gates
6. `11f` Packaging And Distribution

其中：

- `11a` 可立即做
- `11b` 与 `11c` 可并行
- `11d` 依赖 `11c` 的稳定 recovery 语义
- `11e` 可与 `11b/11c` 交错推进
- `11f` 最后收口，但不应阻塞中间能力验证

## 详细 Spec

### 11a. Product Entry And UX

目标：

- 让 `deepseek` 成为真正一致的对外入口
- 明确 one-shot vs interactive 的心智边界
- 把“工程内部工具”的感觉继续压低

范围：

- CLI 帮助、doctor、README、REPL guide 统一主品牌
- `deepseek` / `deepseek chat` / `deepseek repl` / `deepseek interactive` 同心智
- REPL 启动时给出更明确的首屏引导
- 非 TTY / 非交互错误提示统一
- one-shot 指令示例统一为 `deepseek run "..."`

交付：

- 命令行文案统一
- 文档统一
- REPL 首屏 / help 文本统一

验收：

- 新用户只看 README 就能开始用
- 帮助输出不再要求先理解 `dscode`
- REPL / one-shot / benchmark / dogfood 的文案风格统一

### 11b. Real PR / CI Workflow Hardening

目标：

- 让 `pr review / fix / patch` 成为主力路径，而不是“有命令但样本薄”

范围：

- 增加更多 `pr_workflow` fixtures：
  - review readback
  - failed CI lint
  - failed CI test
  - patch follow-up
  - second-round review feedback
- 扩大 live dogfood 的 `pr_workflow` 样本
- 增加 CI log tail -> symbol -> search/readback/patch 的真实链路
- 把 review follow-up / merge-back 语义做稳

交付：

- 更厚的 `pr_workflow` benchmark baseline
- 更厚的 `pr_workflow` live dogfood ledger
- 更稳定的 `pr fix/patch` orchestration

验收：

- `pr_workflow` 不再只有少量 happy path
- 至少覆盖 Rust + JavaScript + Go 三类 workflow，其中 Rust / JavaScript / Python 已覆盖 retry，Go 已覆盖 patch+validate 与 reproduce+fix
- benchmark / dogfood 两端都能看到样本增长

### 11c. Recovery And Open-Ended Stability

目标：

- 让 planner 对模糊任务、失败恢复、调查型任务更稳定

范围：

- failure repro / readback / replan 规则进一步结构化
- 收紧“已读回失败点后继续乱搜”的路径
- 压少多余 tool hops
- 减少 seeded-only recovery case，增加自然 failure fixture
- 对 failing-test / lint-failure / build-failure 三类恢复链路统一语义

交付：

- recovery baseline 更厚
- open-ended task 更少漂移
- 自然 failure fixtures 增长

验收：

- recovery baseline 稳定
- live dogfood 中 `manual` / `stuck` 比例下降
- tool trace 更短、更一致

### 11d. Subagent v2

目标：

- 让 subagent 从“能派出去”进化到“真的帮助父计划收敛”

范围：

- 更稳定的 dispatch 规则
- 多 child file / child summary merge-back
- duplicate dispatch 抑制
- parent replan 对 child blocker 的消费
- PR / repo inspection / recovery 三类 follow-up 专门策略

交付：

- 更厚的 `subagent` baseline
- 更强的 child summary schema
- 更少的 parent drift

验收：

- subagent path 能稳定推进 parent todos
- child output 真正改变 parent next step
- 无明显重复 dispatch

### 11e. Live Quality Gates

目标：

- 让 benchmark 和 dogfood 不只是“观察面板”，而是“质量门禁”

范围：

- dogfood slice-level trend gate
- benchmark + dogfood 对照 gate
- `pr_workflow` 专项 gate
- 更清晰的 warmup / comparable-run 规则
- 更强 explainability：为什么被 gate 挡住

交付：

- slice-level gate
- dogfood trend gate
- 更清晰的 report

验收：

- `recovery / pr_workflow / write_validate` 三类都有可解释 gate
- 可比性不足时明确 `skipped`
- 退化时能快速定位是哪个 slice 出问题

### 11f. Packaging And Distribution

目标：

- 让 `DeepseekCode` 从“本地开发仓库”走向“别人可安装使用”

范围：

- `cargo install` / release artifact 路径清晰
- shell completion
- install guide
- config bootstrap
- version / upgrade story

交付：

- 安装文档
- completion 生成
- release 流程说明

验收：

- 新机器按文档可安装并跑起 `deepseek`
- 不需要先读代码才能理解怎么用

## 依赖与并行矩阵

| Workstream | 依赖 | 可并行对象 |
|---|---|---|
| `11a` UX | 无 | 可与全部并行 |
| `11b` PR/CI | 现有 `pr_workflow` baseline | 可与 `11c`、`11e` 并行 |
| `11c` Recovery | 现有 recovery baseline | 可与 `11b`、`11e` 并行 |
| `11d` Subagent v2 | `11c` 基本稳定 | 后半段可与 `11e` 并行 |
| `11e` Quality Gates | benchmark + dogfood history | 可与 `11b/11c/11d` 并行 |
| `11f` Packaging | 入口基本稳定 | 可与 `11e` 交错，但最好最后收口 |

推荐顺序：

1. `11a`
2. `11b` + `11c` 并行
3. `11d`
4. `11e`
5. `11f`

## 里程碑

### M1 — Product Entry Ready

- `deepseek` 成为文档和 UX 唯一主入口
- REPL / one-shot 文案统一

### M2 — PR/CI Baseline Thickened

- `pr_workflow` fixture 和 live 样本显著增加
- review / failed-ci / patch follow-up 都有稳定覆盖

### M3 — Recovery Stable

- 自然 failure fixtures 明显增加
- open-ended recovery 漂移减少

### M4 — Subagent v2

- child summary / merge-back 更稳
- parent plan 更少漂移

### M5 — Live Gates Enforced

- dogfood slice gate 生效
- benchmark + dogfood 对齐

### M6 — Installable Product

- 新用户可安装
- `deepseek` 作为主入口稳定工作

## 验收标准

Phase 11 完成时，应同时满足：

1. `deepseek` 命令对新用户是直观的
2. benchmark 维持高通过率，且 trend gate 稳定
3. live dogfood 的 `pr_workflow / recovery / write_validate` 样本显著更厚
4. PR / CI 路线不再只是 seeded readback，而是更完整的真实链路
5. subagent 在真实多步任务里明显更少漂移
6. 有明确的安装 / 分发路径

## 风险

| 风险 | 影响 | 缓解 |
|---|---|---|
| baseline 越做越厚，trend gate 经常重置 warmup | 让趋势数据短期噪声变大 | 每次新增 case 时明确记录 comparability reset |
| `pr_workflow` fixture 越多越复杂，维护成本上升 | benchmark 成本增大 | 优先保留高价值 fixture family，压缩重复 case |
| subagent v2 过早复杂化 | 反而增加漂移 | 先做“稳的一层”而不是多层 swarm |
| packaging 提前做会掩盖核心 agent 问题 | 产品表面更好看，实质不稳 | packaging 放在 Phase 11 后段 |

## 实施建议

如果按 2-4 周冲刺推进，建议：

- 第 1 周：`11a` + `11b` 起步
- 第 2 周：`11c`
- 第 3 周：`11d` + `11e`
- 第 4 周：`11f` + 总体回归

更务实的执行方式是：

- 每完成一个 workstream，就：
  - 扩 benchmark
  - 扩 live dogfood
  - review 一轮真实 trace
  - 再推进下一个

这和当前 Phase 10i 后半段形成的工作节奏一致，也最适合继续收敛到产品级。
