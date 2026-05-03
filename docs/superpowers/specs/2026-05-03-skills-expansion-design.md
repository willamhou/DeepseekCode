# Skills Expansion — Phase 10d-1 设计

最后更新：`2026-05-03`
状态：`spec` (未实现)
关联 Phase：10d-1（Skills 拓展第一步：A + B 合并 — 多 ship skill + 用户级目录）

## 背景

dscode 当前只 ship 3 个 skill（`pr-review` / `fix-tests` / `fix-lint`），且只从仓库 `skills/`
加载。对比 Claude Code 的 superpowers 套有几百个 skill 加用户级 / 系统级路径，
dscode 的 skill 池**严重单薄** — agent 拿不到对应任务模板就只能裸跑。

dogfood 实测发现：DeepSeek v4-pro 在没 skill 时全靠任务文本 + 内置 nudge 工作，开放
任务（research / refactor）容易跑偏。给定一个匹配的 skill（system_append + suggested_steps）
模型行为立即收敛。

Phase 10d-1 干两件事：
1. **多 ship 12 个混合 skill**（覆盖工程 / 语言 / PR / Claude Code 心智）
2. **加用户级 skills 目录加载**，支持 `~/.config/dscode/skills/` 覆盖仓库 skill

## 目标

- skills 池从 3 个扩到 15+（13 仓库 + 用户级数量）
- 用户能写自己的 skill 到 `~/.config/dscode/skills/`，不必 fork dscode
- 撞名时 user wins，让用户能 override 仓库默认（行业惯例：git/zsh/cargo 同向）
- toml schema 不变，老 skill 100% 兼容
- 零新依赖（继续手写 tilde 展开）

## 非目标 (10d-1)

- 修改 SkillSpec schema（加 `triggers` / `initial_todos` / `references` — Phase 10d-2）
- Auto-select skill from task（Phase 10d-3）
- Skill marketplace / git-clone 支持（YAGNI）
- Skill 版本管理（YAGNI）
- 系统级 `/etc/dscode/skills/` 路径（YAGNI，只双层够了）
- Windows-specific 路径展开（用户改 config.toml 写绝对路径即可）

## 锁定的设计决策

brainstorm 五轮 Q&A 收敛：

1. **Skill 类别**：5 全部混合（工程 / 语言 / PR / Claude Code 心智）— vs 单一类别太薄
2. **目录路径**：`~/.config/dscode/skills/` (XDG-compliant)，可经 `workspace.user_skills_dir` 配置
3. **撞名**：user wins（用户级覆盖仓库级）— 与 git/shell/cargo 工业惯例一致
4. **toml depth**：中等 B（~25 行/skill），含 system_append 5-8 句 + suggested_steps 4-6 步
5. **加载顺序**：仓库 → 用户，HashMap insert 让 user override 自动发生
6. **PR 切片**：M1 (code) + M2 (12 toml content)，2 PR 1 天

## 12 个新 skill 清单

| Skill | 类别 | 用途 |
|---|---|---|
| `research` | 工程任务 | GitHub/web 调研写 RESEARCH.md（与 10c-3 research-bootstrap 协同）|
| `refactor` | 工程任务 | minimal-diff rename / extract / move 重构 |
| `debug` | 工程任务 | reproduce → trace → root cause → fix 流水 |
| `write-tests` | 工程任务 | TDD：先写失败测试再实现 |
| `dependency-update` | 工程任务 | cargo update / pnpm up + 跑测试 |
| `rust-clippy` | 语言特化 | clippy --all-targets -D warnings 修 |
| `python-mypy` | 语言特化 | mypy strict 修类型 |
| `pr-fix-feedback` | PR 工作流 | 按 review comment 改 PR |
| `brainstorm` | Claude Code 心智 | 产 design doc 不写代码 |
| `verify-changes` | Claude Code 心智 | commit 前必跑 lint+test+build |
| `commit-message` | 通用 | conventional commit 格式产消息 |
| `readme-update` | 通用 | 重大变更后更新 README |

每个 skill ~25 行 toml（与现有 fix-tests/fix-lint/pr-review 同水位）。

## 架构

### 模块边界

```
skills/                                  (repo 级)
├── pr-review.toml                       (existing)
├── fix-tests.toml                       (existing)
├── fix-lint.toml                        (existing)
├── research.toml                        ← M2
├── refactor.toml                        ← M2
├── debug.toml                           ← M2
├── write-tests.toml                     ← M2
├── dependency-update.toml               ← M2
├── rust-clippy.toml                     ← M2
├── python-mypy.toml                     ← M2
├── pr-fix-feedback.toml                 ← M2
├── brainstorm.toml                      ← M2
├── verify-changes.toml                  ← M2
├── commit-message.toml                  ← M2
└── readme-update.toml                   ← M2

~/.config/dscode/skills/                 (用户级，dscode 启动时尝试加载，不存在则 skip)

src/
├── config/
│   ├── types.rs                         (改：WorkspaceConfig.user_skills_dir)
│   └── load.rs                          (改：toml 解析 + default `~/.config/dscode/skills`)
├── skills/
│   ├── tilde.rs                         ← NEW (~ 展开 helper，零依赖)
│   ├── mod.rs                           (改：pub mod tilde;)
│   └── registry.rs                      (改：load_dir → load_dirs(&[...]); LoadStats)
├── core/loop_runtime.rs                 (改：调 load_dirs，传 [repo, user])
└── cli/commands/doctor.rs               (改：新 [skills] 段)
```

15 文件，13 新（12 skill toml + tilde.rs）+ 5 改。

### 数据契约

#### `config/types.rs::WorkspaceConfig`（改）

```rust
pub struct WorkspaceConfig {
    pub config_dir: String,
    pub session_dir: String,
    pub user_skills_dir: String,    // 新增；默认 "~/.config/dscode/skills"
}

impl Default for WorkspaceConfig {
    fn default() -> Self {
        Self {
            config_dir: ".dscode".to_string(),
            session_dir: ".dscode/sessions".to_string(),
            user_skills_dir: "~/.config/dscode/skills".to_string(),
        }
    }
}
```

默认值**保留 `~`**字面量，由 `expand_tilde` 在使用时展开。配置文件写起来短，跨机器移植直观。

#### `config/load.rs`（改）

`load_or_default` 解析 `workspace.user_skills_dir`（可选；缺失 → default）：

```rust
let user_skills_dir = workspace_table
    .as_ref()
    .and_then(|w| w.get("user_skills_dir"))
    .and_then(|v| v.as_str())
    .map(str::to_string)
    .unwrap_or_else(|| WorkspaceConfig::default().user_skills_dir);
```

#### `skills/tilde.rs`（新）

```rust
/// Expand a leading `~` or `~/` to the user's home directory.
///
/// - `"~/x/y"` → `<HOME>/x/y` if `HOME` env is set
/// - `"~"` alone → `<HOME>` if set
/// - `"/abs/path"` → unchanged
/// - `"relative"` → unchanged
/// - `"~user/x"` → unchanged (we do not support `~username` syntax)
/// - `HOME` unset → input unchanged (caller will treat as missing path)
///
/// Zero-deps: queries `std::env::var("HOME")` directly.
pub fn expand_tilde(path: &str) -> std::path::PathBuf;
```

行为表（用于测试）：

| 输入 | HOME=`/h/u` | 输出 |
|------|-------------|------|
| `~/.config/dscode/skills` | set | `/h/u/.config/dscode/skills` |
| `~/` | set | `/h/u/` |
| `~` | set | `/h/u` |
| `/abs/path` | * | `/abs/path` |
| `relative/path` | * | `relative/path` |
| `~user/x` | * | `~user/x`（不展开） |
| `~/x` | unset | `~/x`（原样返回） |

#### `skills/registry.rs`（改）

```rust
pub struct SkillRegistry {
    skills: BTreeMap<String, SkillSpec>,
}

#[derive(Debug, Clone, Default)]
pub struct LoadStats {
    /// Total skills in final registry after merging.
    pub total: usize,
    /// Per-path: (path, count loaded from this path).
    pub by_path: Vec<(PathBuf, usize)>,
    /// Skill names where a later path overrode an earlier one (user vs repo).
    pub overridden: Vec<String>,
}

impl SkillRegistry {
    /// Load skills from one or more directories. Later directories override
    /// earlier ones on name collision (last-wins). Missing dirs silently skip.
    /// Returns the merged registry plus stats describing the load.
    pub fn load_dirs(paths: &[&Path]) -> AppResult<(Self, LoadStats)> {
        let mut skills = BTreeMap::new();
        let mut stats = LoadStats::default();
        for path in paths {
            if !path.exists() {
                stats.by_path.push((path.to_path_buf(), 0));
                continue;
            }
            let mut count = 0usize;
            for entry in std::fs::read_dir(path)? {
                let entry = entry?;
                let entry_path = entry.path();
                if entry_path.extension().and_then(|s| s.to_str()) != Some("toml") {
                    continue;
                }
                let spec = crate::skills::loader::load_toml(&entry_path)?;
                if skills.contains_key(&spec.name) {
                    stats.overridden.push(spec.name.clone());
                }
                skills.insert(spec.name.clone(), spec);
                count += 1;
            }
            stats.by_path.push((path.to_path_buf(), count));
        }
        stats.total = skills.len();
        Ok((Self { skills }, stats))
    }

    /// Back-compat: load from a single directory. Used by existing tests.
    pub fn load_dir(path: &str) -> AppResult<Self> {
        let p = std::path::PathBuf::from(path);
        Ok(Self::load_dirs(&[p.as_path()])?.0)
    }

    pub fn get(&self, name: &str) -> Option<&SkillSpec> {
        self.skills.get(name)
    }
}
```

#### `core/loop_runtime.rs::run_with_client`（改）

替换：

```rust
let skills = SkillRegistry::load_dir("skills")?;
```

为：

```rust
let user_skills_dir = crate::skills::tilde::expand_tilde(
    &self.config.workspace.user_skills_dir,
);
let (skills, _stats) = SkillRegistry::load_dirs(&[
    std::path::Path::new("skills"),
    user_skills_dir.as_path(),
])?;
```

`_stats` 暂不用（可选：banner 输出加一行 "loaded N skills"）。

#### `cli/commands/doctor.rs`（改）

新加 `[skills]` 段：

```
[skills]
  loaded: 15 skills
    repo (skills/): 13 — pr-review, fix-tests, fix-lint, research, refactor, debug, write-tests, dependency-update, rust-clippy, python-mypy, pr-fix-feedback, brainstorm, verify-changes, commit-message, readme-update
    user (~/.config/dscode/skills/): 0 — not found (skip)
```

或当用户 dir 存在 + override 时：

```
[skills]
  loaded: 16 skills
    repo (skills/): 13
    user (/home/foo/.config/dscode/skills/): 3 — my-cleanup, jira-ticket, pr-review
  user overrides: pr-review (user replaces repo)
```

调用 `SkillRegistry::load_dirs` 时拿 `LoadStats`，按 by_path 列举。

### 12 个 skill 内容模板（B-depth ~25 行）

每 skill 形如：

```toml
name = "research"
description = "Research a topic on GitHub or via curl, write findings to a markdown file"

allowed_tools = ["list_files", "read_file", "search_text", "apply_patch", "run_shell", "todo_write"]

system_append = """
You are doing read-only research, not editing project source code.
- Step 1: todo_write to plan 4-8 search steps.
- Each subsequent step: ONE gh search / curl call.
- Cite real GitHub repos with star counts; never fabricate stats.
- If a search returns nothing, note 'no results' and move on.
- Output: apply_patch to create RESEARCH.md with sections per topic.
"""

suggested_steps = [
  "Plan 4-8 research steps with todo_write",
  "Issue gh search repos / gh search code calls one at a time",
  "Track progress in todo_write between calls",
  "Synthesize findings into RESEARCH.md via apply_patch",
  "Mark all todos completed and Finish",
]

[policy]
require_write_confirmation = false
require_shell_confirmation = false
shell_allowlist = ["gh search", "gh repo view", "gh api", "curl -sSL", "curl -sS"]
```

12 文件按这模板写。具体内容（system_append + suggested_steps + shell_allowlist）按各 skill 用途定。

### 加载顺序与失败处理

| 情景 | 行为 |
|------|------|
| repo `skills/` 存在 + user dir 存在 + 无撞名 | 合并；total = sum of both |
| repo + user 撞名（同 toml `name`） | user 覆盖；overridden 列表记录 |
| user dir 不存在 | silently skip；by_path 记 0 count |
| user dir 存在但空 | silently skip；by_path 记 0 |
| user dir 存在但有 invalid toml | fatal `app_error`（写错的 toml 应该立即知道） |
| HOME unset 且 user_skills_dir 是 `~/...` | tilde 不展开 → 路径不存在 → silently skip |
| 某 toml 缺必填字段（如 `name`） | fatal（与现 loader 行为一致） |

### 向后兼容

- `SkillRegistry::load_dir(path: &str)` 保留，内部 wrap `load_dirs`
- 老 `.dscode/config.toml` 缺 `workspace.user_skills_dir` → default `~/.config/dscode/skills`，几乎所有用户无该目录 → silently skip → 等价老行为
- 老 toml schema（缺 10d-2 的 `triggers` / `references` / `initial_todos`）原样工作 — 这些 fields 是 Phase 10d-2 加的

## 切片：2 PR、~1 天

| PR | 工作 | 估时 | 测试增量 | Land 条件 |
|----|------|------|----------|-----------|
| **M1** | code only：`skills/tilde.rs` 新 + `WorkspaceConfig.user_skills_dir` + `load.rs` parse + `registry::load_dirs` + `LoadStats` + `loop_runtime` switch + `doctor` `[skills]` 段 | 0.5d | +12 | 273 → 285；零 warnings；现 3 个 toml 仍正确加载（向后兼容验证） |
| **M2** | content only：12 个新 skill toml + roadmap 标 10d-1 完成 | 0.5d | +1 | 285 → 286；smoke "all 15 skills parse" + dogfood 跑 2-3 个 skill 验证 |

总：2 PR、+13 测试（273 → 286）、~1 天。

阶段化 land：
- **M1**：纯重构，行为零变化（默认 user dir 不存在 → 等价老行为）
- **M2**：纯内容，不动代码

## 测试策略

### 单测（+13）

`skills/tilde.rs` × 4（M1）：
- `expand_tilde("~/.config/dscode/skills")` → `<HOME>/.config/dscode/skills`
- `expand_tilde("~/")` → `<HOME>/`
- `expand_tilde("/abs/path")` → 原样
- `expand_tilde("~user/x")` → 原样（不支持 `~user`）

`config/load.rs` × 2（M1）：
- `load_or_default` 缺 `user_skills_dir` 时用 default `"~/.config/dscode/skills"`
- toml 含 custom `user_skills_dir = "/custom/x"` → 解析正确

`skills/registry.rs::load_dirs` × 5（M1）：
- 单 dir：等价老 `load_dir`，count 与 `total` 正确
- 两 dir 无撞名：合并，每个 by_path entry count 正确
- 两 dir 有撞名：user wins，`overridden` 含被覆盖的名字
- 用户 dir 不存在：silently skip，`by_path` entry count = 0
- 用户 dir 含 invalid toml：fatal `app_error` 抛出

`cli/commands/doctor.rs` × 1（M1）：
- doctor 输出含 `[skills]` 段、loaded 计数与 by_path 一致

**Integration smoke** × 1（M2）：
- 解析仓库 `skills/*.toml` 全部 15 个文件成功

### 集成 / 手测（M2）

```bash
# 验证 1：默认配置（用户目录不存在）
dscode doctor    # [skills] 段显示 15 加载、user dir not found

# 验证 2：建用户级 skill
mkdir -p ~/.config/dscode/skills
cat > ~/.config/dscode/skills/my-test.toml <<'EOF'
name = "my-test"
description = "Local test skill"
allowed_tools = ["list_files", "todo_write"]
system_append = "Test skill loaded from user dir."
suggested_steps = ["one"]
[policy]
require_write_confirmation = false
require_shell_confirmation = false
shell_allowlist = []
EOF
dscode doctor    # 应显示 16 加载、user 1 个、my-test 列出

# 验证 3：override 仓库 skill
cat > ~/.config/dscode/skills/pr-review.toml <<'EOF'
... 完全自定义内容 ...
EOF
dscode doctor    # 应显示 [skills] overrides: pr-review

# 验证 4：dogfood 真用 skill
DSCODE_AUTO_APPROVE_WRITES=1 dscode run --skill research --budget 25 \
  "research how rust handles memory leaks on github"
# 期望：激活 research skill 的 system_append；agent 在 step 1 todo_write，2-N gh search
```

## 错误分类

| 场景 | 分类 |
|------|------|
| 用户 dir 不存在 | silently skip（不是错误） |
| 用户 dir 存在但 invalid toml | `app_error("failed to parse skill toml at <path>: <detail>")` |
| toml 缺 `name` 字段 | `app_error("skill toml at <path> missing `name`")`（与现 loader 一致） |
| HOME unset + tilde path | tilde 原样返回，路径不存在 → silently skip 走"用户 dir 不存在"分支 |

## 风险

| 风险 | 缓解 |
|------|------|
| `HOME` env 在 CI / minimal docker 中未设置 → tilde 展开失败 | tilde fallback：`HOME` 缺失时**原样返回**，路径不存在 → silently skip |
| Windows 用户配 `%APPDATA%` 路径 | 文档示例改 config.toml；tilde 不展开 `%APPDATA%` 但 Windows 用户写绝对路径即可 |
| 用户 dir 含非 toml 文件（README.md / .git/） | `load_dirs` 只处理 `.toml` 后缀，其他文件 silently skip |
| 12 新 skill 内容写得不准 → LLM 用了反而表现变差 | M2 dogfood 实测 2-3 个；toml 总量 ~300 行，可快速迭代（不动代码） |
| 撞名 silently 让用户困惑 | `LoadStats.overridden` 在 doctor 里明示；启动 banner 也加一行（可选） |
| toml schema 加新字段（10d-2）时旧 toml 兼容 | 10d-1 不动 schema；10d-2 加字段时用 `Option<>` + serde default |
| 用户 dir path 解析为危险路径（如 `/`） | tilde 不解析 `..`；用户写绝对路径风险归用户；不影响 dscode 本身（只 read 不 write） |

## 待解项

无。所有交互式问题在 brainstorming 中收敛：
- Q1: Skill 类别 — 全部混合 12 个
- Q2: 用户级目录 — `~/.config/dscode/skills/`，可经 config.toml 配置
- Q3: 撞名 — user wins
- Q4: toml depth — 中等 B（~25 行/skill）

## 后续 (Phase 10d-2 / 10d-3 候选)

明确为 10d-1 之外，下个子项目：
- **Phase 10d-2**：SkillSpec schema v2 — 加 `triggers: Vec<String>`、`initial_todos: Vec<TodoSeed>`、`references: Vec<String>`。schema bump 但兼容老 toml。
- **Phase 10d-3**：用 `triggers` 做 auto-select skill from task — 在 `dscode run` 没 `--skill` 时尝试自动匹配。
- **Phase 10b**：Sub-agent 派发（独立 phase，与 skills 系统配合）
