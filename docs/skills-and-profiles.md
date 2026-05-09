# Skill 与 Language Profile 设计

## 为什么要分成两类

`Profile` 与 `Skill` 解决的是不同问题：

- `Language Profile` 关注仓库和语言环境
- `Skill` 关注当前任务的执行策略

这样能避免：

- 把语言逻辑硬编码进 agent prompt
- 把任务逻辑和运行时绑死
- 为了扩展一个语言或一个任务就修改核心代码

## Language Profile

Profile 主要定义：

- 文件优先级
- 忽略规则
- 常见测试命令
- 常见 lint/build 命令
- 对模型的补充提示

Rust 结构建议：

```rust
#[derive(Debug, Clone, serde::Deserialize)]
pub struct LanguageProfile {
    pub name: String,
    #[serde(default)]
    pub file_priority: Vec<String>,
    #[serde(default)]
    pub ignore_patterns: Vec<String>,
    #[serde(default)]
    pub test_commands: Vec<String>,
    #[serde(default)]
    pub lint_commands: Vec<String>,
    #[serde(default)]
    pub build_commands: Vec<String>,
    #[serde(default)]
    pub hints: Vec<String>,
}
```

示例：

```toml
name = "rust"
file_priority = ["Cargo.toml", "src/main.rs", "src/lib.rs", "tests/"]
ignore_patterns = ["target/", ".git/"]
test_commands = ["cargo test"]
lint_commands = ["cargo clippy --all-targets --all-features"]
build_commands = ["cargo build"]
hints = ["Prefer minimal compile-safe changes."]
```

## Skill

Skill 主要定义：

- 任务描述
- 可用工具
- 追加 system prompt
- 建议步骤
- 写入与 shell 审批策略
- shell allowlist

Rust 结构建议：

```rust
#[derive(Debug, Clone, serde::Deserialize)]
pub struct SkillSpec {
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub allowed_tools: Vec<String>,
    #[serde(default)]
    pub system_append: String,
    #[serde(default)]
    pub suggested_steps: Vec<String>,
    #[serde(default)]
    pub triggers: Vec<String>,
    #[serde(default)]
    pub initial_todos: Vec<TodoSeed>,
    #[serde(default)]
    pub references: Vec<String>,
    pub policy: SkillPolicy,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct TodoSeed {
    pub content: String,
    pub active_form: String,
    #[serde(default = "default_pending")]
    pub status: TodoStatus,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct SkillPolicy {
    #[serde(default = "default_true")]
    pub require_write_confirmation: bool,
    #[serde(default = "default_true")]
    pub require_shell_confirmation: bool,
    #[serde(default)]
    pub shell_allowlist: Vec<String>,
}
```

示例：

```toml
name = "fix-tests"
description = "Focus on reproducing and fixing failing tests with minimal edits"
allowed_tools = ["list_files", "read_file", "search_text", "apply_patch", "run_shell", "git_diff"]
triggers = ["fix tests", "failing tests", "red tests"]
references = ["README.md", "Cargo.toml"]
system_append = """
Reproduce failures first. Prefer the smallest safe code change.
Rerun only relevant tests before broad test suites.
"""
suggested_steps = [
  "Find the test command",
  "Reproduce the failure",
  "Inspect the smallest relevant code path",
  "Apply a minimal patch",
  "Rerun the relevant tests"
]

[[initial_todos]]
content = "Reproduce the failure"
active_form = "Reproducing the failure"
status = "in_progress"

[[initial_todos]]
content = "Apply the minimal patch"
active_form = "Applying the minimal patch"
status = "pending"

[policy]
require_write_confirmation = true
require_shell_confirmation = false
shell_allowlist = ["cargo test", "pytest", "pnpm test", "npm test", "go test", "mvn test", "gradle test"]
```

## 推荐的首批 Skills

- `fix-tests`
- `fix-lint`
- `explain-codebase`
- `small-refactor`

这些已经足够支撑第一版常见任务，不需要一开始做开放式插件生态。
