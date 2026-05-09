# Claude/Codex Gap Closure — Phase 11 设计

最后更新：`2026-05-09`
状态：`complete`（Phase 11 已收口，后续进入 baseline hardening）
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

当前基线（2026-05-09 收口后 baseline hardening 复测）：

- benchmark：`42/42`
- 全量测试：`467 passed, 0 failed`
- benchmark trend gate：`pass against 3 comparable runs`
- dogfood live gate：`pass (no new dogfood records since previous snapshot, runs=22)`
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

- dogfood：`22 runs`
- `pr_workflow`: `8 runs`, `7 success`, `1 failed`
- `recovery`: `1` 条 diagnostic expected-failure 已真实记账
- `write_validate`: `4 runs`, `3 success`, `1 failed`

当前已完成的 Phase 11 进展：

- `11a`：`deepseek` 已成为主入口，主文档和关键运行时提示已统一到 `deepseek`，`dscode` 退回兼容别名
- `11b`：`pr_workflow` baseline 已新增真实 second-round review feedback fixture，现为 `10` 条 case
- `11b`：`pr_workflow` baseline 进一步新增真实 JavaScript PR fix+validate case，现为 `11` 条 case
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

本轮收口顺序：

1. 回填 roadmap/spec 与实测状态，承认 `38/39` 红点
2. 收 `11d`：subagent v2 的 child summary / next-action / merge-back 语义
3. 收 `11e`：benchmark failed expectation 与 dogfood 新增 live failure 都必须能阻断
4. 收 `11f`：release / upgrade story 从“能安装”补到“能发布、能升级、能回滚”

当前结果：上述 4 项已收口，最新 benchmark 为 `42/42`，全量测试为 `467 passed, 0 failed`。

这说明 `DeepseekCode` 已经不是“演示级原型”，但仍明显低于 Claude Code / Codex 的
产品完成度。差距不再是“有没有 planner / tool loop”，而是：

1. 真实 PR / CI / review 场景样本不够厚
2. open-ended / ambiguous task 的默认稳定性不够
3. subagent orchestration 仍是 v1
4. product UX / install / release 仍偏开发者内部工具
5. live quality gate 还没真正成为“发布阻断器”

## 差距表

| 维度 | `DeepseekCode` 当前状态 | Claude Code / Codex 目标状态 | 差距等级 |
|---|---|---|---|
| 命令行入口 | `deepseek` 已可直接进入 REPL | 默认心智一致、文档和错误提示完全统一 | 小 |
| REPL / 交互体验 | transcript、slash、session 已有 | 更顺滑的 history、恢复、帮助、默认提示 | 中 |
| 单仓库本地 coding flow | benchmark 覆盖面已较强 | 默认成功率更高，少漂移、少无效 hops | 中 |
| open-ended 任务 | 已有 recovery / replan，但依赖 heuristic | 对模糊任务也能稳定收敛 | 大 |
| PR / CI 工作流 | `pr review/fix/patch` 已有 + baseline | 更厚的真实 PR/CI 样本与稳定端到端闭环 | 大 |
| subagent | 已能 dispatch / merge-back | 更成熟的拆分、归并、去重、收敛 | 大 |
| live 回归体系 | benchmark + dogfood 已闭环 | 更厚的 live baseline，且可阻断回归 | 中 |
| 安装 / 分发 | 面向开发者较友好 | 普通用户开箱即装即用 | 中 |
| IDE / 编辑器配套 | 几乎没有 | 统一的产品体验 | 大 |
| 默认产品完成度 | 强原型 | 可长期主用的产品级工具 | 大 |

## 目标

Phase 11 的目标不是“功能数量翻倍”，而是把现有能力推进到更稳定、更像产品的状态：

- `deepseek` 对新用户来说就是默认入口，不需要再解释命令心智
- PR / CI / review 成为真正高价值主线路，而不只是 demo 路径
- recovery / replan / subagent 更少依赖偶然的 prompt luck
- benchmark 和 dogfood 成为真正的质量门禁
- 为后续 packaging / install / public release 铺平道路

## 非目标 (Phase 11)

- VS Code / JetBrains 插件开发
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
- 至少覆盖 Rust + JavaScript 两类完整 workflow
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
