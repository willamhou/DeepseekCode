use std::cell::RefCell;
use std::env;
use std::rc::Rc;

use crate::config::types::AppConfig;
use crate::config::types::ApprovalConfig;
use crate::core::todos::TodoList;
use crate::error::{app_error, policy_denied, tool_failure, AppResult};
use crate::skills::schema::SkillSpec;
use crate::tools::apply_patch::ApplyPatchTool;
use crate::tools::dispatch_subagent::DispatchSubagentTool;
use crate::tools::git_diff::GitDiffTool;
use crate::tools::list_files::ListFilesTool;
use crate::tools::mcp::{McpCallTool, McpListToolsTool};
use crate::tools::read_file::ReadFileTool;
use crate::tools::run_shell::{is_safe_shell_command, RunShellTool};
use crate::tools::search_text::SearchTextTool;
use crate::tools::types::{Tool, ToolInput, ToolOutput};
use crate::ui::confirm::confirm;

pub struct ToolRegistry {
    tools: Vec<Box<dyn Tool>>,
}

pub const MAX_SUBAGENT_DEPTH: usize = 1;

impl ToolRegistry {
    pub fn names_for_policy(&self, policy: &ExecutionPolicy) -> Vec<&'static str> {
        self.tools
            .iter()
            .map(|tool| tool.name())
            .filter(|name| policy.allows_tool(name))
            .collect()
    }

    pub fn execute(&self, name: &str, input: ToolInput) -> AppResult<ToolOutput> {
        let tool = self
            .tools
            .iter()
            .find(|tool| tool.name() == name)
            .ok_or_else(|| app_error(format!("unknown tool: {name}")))?;
        tool.execute(input)
    }

    pub fn execute_with_policy(
        &self,
        name: &str,
        input: ToolInput,
        policy: &ExecutionPolicy,
    ) -> AppResult<ToolOutput> {
        if !policy.allows_tool(name) {
            return Err(policy_denied(format!("tool blocked by policy: {name}")));
        }

        if name == "apply_patch" && policy.require_write_confirmation && !policy.auto_approve_writes
        {
            let target = describe_apply_patch_target(&input);
            let prompt = format!("Apply patch in {}?", sanitize_for_prompt(&target));
            if !confirm(&prompt) {
                return Err(policy_denied(format!(
                    "write declined for {}; set DSCODE_AUTO_APPROVE_WRITES=1 to skip prompts or relax the active policy",
                    sanitize_for_prompt(&target)
                )));
            }
        }

        if name == "run_shell" {
            let command = input
                .get("command")
                .ok_or_else(|| app_error("run_shell requires a command"))?;
            let cwd = input.get("cwd").unwrap_or(".");

            if !policy.shell_allowlist.is_empty()
                && !policy
                    .shell_allowlist
                    .iter()
                    .any(|prefix| command.trim().starts_with(prefix))
            {
                return Err(policy_denied(format!(
                    "shell command blocked by policy allowlist: {}",
                    sanitize_for_prompt(command)
                )));
            }

            if !is_safe_shell_command(command) {
                return Err(policy_denied(format!(
                    "command not allowed: {}",
                    sanitize_for_prompt(command)
                )));
            }

            if policy.require_shell_confirmation && !policy.auto_approve_shell {
                let prompt = format!(
                    "Run shell command in {}: '{}'?",
                    sanitize_for_prompt(cwd),
                    sanitize_for_prompt(command)
                );
                if !confirm(&prompt) {
                    return Err(policy_denied(
                        "shell command declined; set DSCODE_AUTO_APPROVE_SHELL=1 to skip prompts or relax the active policy",
                    ));
                }
            }
        }

        if name == "mcp_call" {
            let (server, tool) = mcp_call_target(&input)?;
            let target = format!("{server}/{tool}");

            if !policy.mcp_call_allowlist.is_empty()
                && !mcp_call_matches_allowlist(&policy.mcp_call_allowlist, server, tool)
            {
                return Err(policy_denied(format!(
                    "mcp tool call blocked by policy allowlist: {}; add an entry to approval.mcp_call_allowlist or use server/*",
                    sanitize_for_prompt(&target)
                )));
            }

            if policy.require_mcp_confirmation && !policy.auto_approve_mcp {
                let prompt = format!("Call MCP tool {}?", sanitize_for_prompt(&target));
                if !confirm(&prompt) {
                    return Err(policy_denied(format!(
                        "mcp tool call declined for {}; set DSCODE_AUTO_APPROVE_MCP=1 to skip prompts or relax approval.require_mcp_confirmation",
                        sanitize_for_prompt(&target)
                    )));
                }
            }
        }

        self.execute(name, input).map_err(|error| {
            if error.downcast_ref::<crate::error::AppError>().is_some() {
                error
            } else {
                tool_failure(error.to_string())
            }
        })
    }
}

#[derive(Debug, Clone)]
pub struct ExecutionPolicy {
    allowed_tools: Vec<String>,
    require_write_confirmation: bool,
    require_shell_confirmation: bool,
    require_mcp_confirmation: bool,
    shell_allowlist: Vec<String>,
    mcp_call_allowlist: Vec<String>,
    auto_approve_writes: bool,
    auto_approve_shell: bool,
    auto_approve_mcp: bool,
}

impl ExecutionPolicy {
    pub fn new(approval: &ApprovalConfig, skill: Option<&SkillSpec>) -> Self {
        let (
            allowed_tools,
            require_write_confirmation,
            require_shell_confirmation,
            shell_allowlist,
        ) = if let Some(skill) = skill {
            (
                skill.allowed_tools.clone(),
                skill.policy.require_write_confirmation,
                skill.policy.require_shell_confirmation,
                skill.policy.shell_allowlist.clone(),
            )
        } else {
            (
                Vec::new(),
                approval.require_write_confirmation,
                approval.require_shell_confirmation,
                Vec::new(),
            )
        };

        Self {
            allowed_tools,
            require_write_confirmation,
            require_shell_confirmation,
            require_mcp_confirmation: approval.require_mcp_confirmation,
            shell_allowlist,
            mcp_call_allowlist: approval.mcp_call_allowlist.clone(),
            auto_approve_writes: env_flag("DSCODE_AUTO_APPROVE_WRITES"),
            auto_approve_shell: env_flag("DSCODE_AUTO_APPROVE_SHELL"),
            auto_approve_mcp: env_flag("DSCODE_AUTO_APPROVE_MCP"),
        }
    }

    pub fn allows_tool(&self, name: &str) -> bool {
        self.allowed_tools.is_empty() || self.allowed_tools.iter().any(|tool| tool == name)
    }
}

fn env_flag(name: &str) -> bool {
    matches!(
        env::var(name).ok().as_deref(),
        Some("1") | Some("true") | Some("TRUE")
    )
}

fn describe_apply_patch_target(input: &ToolInput) -> String {
    if let Some(path) = input.get("path") {
        return path.to_string();
    }
    if let Some(cwd) = input.get("cwd") {
        return format!("{cwd} (unified diff)");
    }
    "current workspace".to_string()
}

fn mcp_call_target(input: &ToolInput) -> AppResult<(&str, &str)> {
    let server = input
        .get("server")
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| app_error("mcp_call requires `server`"))?;
    let tool = input
        .get("tool")
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| app_error("mcp_call requires `tool`"))?;
    Ok((server, tool))
}

fn mcp_call_matches_allowlist(patterns: &[String], server: &str, tool: &str) -> bool {
    patterns
        .iter()
        .any(|pattern| mcp_call_pattern_matches(pattern, server, tool))
}

fn mcp_call_pattern_matches(pattern: &str, server: &str, tool: &str) -> bool {
    let Some((server_pattern, tool_pattern)) = pattern.trim().split_once('/') else {
        return false;
    };
    segment_matches(server_pattern.trim(), server) && segment_matches(tool_pattern.trim(), tool)
}

fn segment_matches(pattern: &str, value: &str) -> bool {
    pattern == "*" || pattern == value
}

const PROMPT_LIMIT: usize = 200;

fn sanitize_for_prompt(value: &str) -> String {
    let mut out = String::with_capacity(value.len().min(PROMPT_LIMIT) + 1);
    let mut count = 0usize;
    for ch in value.chars() {
        if count >= PROMPT_LIMIT {
            out.push('…');
            break;
        }
        if ch.is_control() && ch != '\t' {
            out.push('?');
        } else {
            out.push(ch);
        }
        count += 1;
    }
    out
}

#[cfg(test)]
pub fn default_registry() -> ToolRegistry {
    default_registry_with_context(
        AppConfig::default(),
        0,
        Rc::new(RefCell::new(TodoList::default())),
    )
}

pub fn default_registry_with_context(
    config: AppConfig,
    subagent_depth: usize,
    todos: Rc<RefCell<TodoList>>,
) -> ToolRegistry {
    let expose_mcp_tools = config.mcp.enabled
        && (config.mcp.project_file_path().exists() || config.mcp.user_file_path().exists());
    let mut tools: Vec<Box<dyn Tool>> = vec![
        Box::new(ListFilesTool),
        Box::new(ReadFileTool),
        Box::new(SearchTextTool),
        Box::new(ApplyPatchTool),
        Box::new(RunShellTool),
        Box::new(GitDiffTool),
        Box::new(crate::tools::todo::TodoWriteTool {
            list: todos.clone(),
        }),
    ];
    if expose_mcp_tools {
        tools.push(Box::new(McpListToolsTool {
            config: config.clone(),
        }));
        tools.push(Box::new(McpCallTool {
            config: config.clone(),
        }));
    }
    if subagent_depth < MAX_SUBAGENT_DEPTH {
        tools.push(Box::new(DispatchSubagentTool {
            config,
            parent_depth: subagent_depth,
        }));
    }
    ToolRegistry { tools }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::types::ApprovalConfig;
    use crate::error::{classify, AppErrorKind};

    fn deny_writes_policy() -> ExecutionPolicy {
        ExecutionPolicy {
            allowed_tools: vec!["apply_patch".to_string(), "run_shell".to_string()],
            require_write_confirmation: true,
            require_shell_confirmation: true,
            require_mcp_confirmation: true,
            shell_allowlist: Vec::new(),
            mcp_call_allowlist: Vec::new(),
            auto_approve_writes: false,
            auto_approve_shell: false,
            auto_approve_mcp: false,
        }
    }

    fn temp_root(name: &str) -> std::path::PathBuf {
        let suffix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "deepseek-registry-{name}-{}-{suffix}",
            std::process::id()
        ))
    }

    #[test]
    fn execute_with_policy_returns_policy_denied_for_blocked_tool() {
        let registry = default_registry();
        let approval = ApprovalConfig::default();
        let policy = ExecutionPolicy::new(&approval, None);
        let blocked_policy = ExecutionPolicy {
            allowed_tools: vec!["read_file".to_string()],
            ..policy
        };

        let error = registry
            .execute_with_policy("apply_patch", ToolInput::new(), &blocked_policy)
            .unwrap_err();
        assert_eq!(classify(error.as_ref()), AppErrorKind::PolicyDenied);
        assert!(error.to_string().contains("blocked by policy"));
    }

    #[test]
    fn execute_with_policy_blocks_non_allowlisted_shell_command() {
        let registry = default_registry();
        let policy = ExecutionPolicy {
            shell_allowlist: vec!["echo".to_string()],
            ..deny_writes_policy()
        };

        let input = ToolInput::new()
            .with_arg("cwd", ".")
            .with_arg("command", "rm -rf /");
        let error = registry
            .execute_with_policy("run_shell", input, &policy)
            .unwrap_err();
        assert_eq!(classify(error.as_ref()), AppErrorKind::PolicyDenied);
        assert!(error.to_string().contains("allowlist"));
    }

    #[test]
    fn execute_with_policy_denies_apply_patch_under_non_tty() {
        let registry = default_registry();
        let policy = deny_writes_policy();

        let input = ToolInput::new()
            .with_arg("path", "/tmp/does_not_matter.txt")
            .with_arg("find", "x")
            .with_arg("replace", "y");
        let error = registry
            .execute_with_policy("apply_patch", input, &policy)
            .unwrap_err();
        assert_eq!(classify(error.as_ref()), AppErrorKind::PolicyDenied);
        assert!(error.to_string().contains("write declined"));
    }

    #[test]
    fn execute_with_policy_denies_mcp_call_under_non_tty() {
        let root = temp_root("mcp-policy");
        std::fs::create_dir_all(&root).unwrap();
        let mcp_file = root.join("mcp.json");
        std::fs::write(
            &mcp_file,
            r#"{"mcpServers":{"fake":{"disabled":true,"transport":"stdio"}}}"#,
        )
        .unwrap();

        let mut config = AppConfig::default();
        config.mcp.project_file = mcp_file.display().to_string();
        config.mcp.user_file = root.join("missing-user.json").display().to_string();
        let registry =
            default_registry_with_context(config, 0, Rc::new(RefCell::new(TodoList::default())));
        let policy = ExecutionPolicy {
            allowed_tools: vec!["mcp_call".to_string()],
            require_write_confirmation: false,
            require_shell_confirmation: false,
            require_mcp_confirmation: true,
            shell_allowlist: Vec::new(),
            mcp_call_allowlist: Vec::new(),
            auto_approve_writes: false,
            auto_approve_shell: false,
            auto_approve_mcp: false,
        };

        let input = ToolInput::new()
            .with_arg("server", "fake")
            .with_arg("tool", "echo");
        let error = registry
            .execute_with_policy("mcp_call", input, &policy)
            .unwrap_err();
        assert_eq!(classify(error.as_ref()), AppErrorKind::PolicyDenied);
        assert!(error.to_string().contains("mcp tool call declined"));

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn execute_with_policy_blocks_non_allowlisted_mcp_call() {
        let root = temp_root("mcp-allowlist");
        std::fs::create_dir_all(&root).unwrap();
        let mcp_file = root.join("mcp.json");
        std::fs::write(
            &mcp_file,
            r#"{"mcpServers":{"fake":{"disabled":true,"transport":"stdio"}}}"#,
        )
        .unwrap();

        let mut config = AppConfig::default();
        config.mcp.project_file = mcp_file.display().to_string();
        config.mcp.user_file = root.join("missing-user.json").display().to_string();
        let registry =
            default_registry_with_context(config, 0, Rc::new(RefCell::new(TodoList::default())));
        let policy = ExecutionPolicy {
            allowed_tools: vec!["mcp_call".to_string()],
            require_write_confirmation: false,
            require_shell_confirmation: false,
            require_mcp_confirmation: false,
            shell_allowlist: Vec::new(),
            mcp_call_allowlist: vec!["github/*".to_string()],
            auto_approve_writes: false,
            auto_approve_shell: false,
            auto_approve_mcp: false,
        };

        let input = ToolInput::new()
            .with_arg("server", "fake")
            .with_arg("tool", "echo");
        let error = registry
            .execute_with_policy("mcp_call", input, &policy)
            .unwrap_err();
        assert_eq!(classify(error.as_ref()), AppErrorKind::PolicyDenied);
        assert!(error.to_string().contains("policy allowlist"));

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn mcp_call_pattern_supports_server_and_tool_wildcards() {
        assert!(mcp_call_pattern_matches(
            "filesystem/read_file",
            "filesystem",
            "read_file"
        ));
        assert!(mcp_call_pattern_matches(
            "filesystem/*",
            "filesystem",
            "write_file"
        ));
        assert!(mcp_call_pattern_matches("*/search", "github", "search"));
        assert!(!mcp_call_pattern_matches(
            "github/*",
            "filesystem",
            "read_file"
        ));
        assert!(!mcp_call_pattern_matches(
            "missing_separator",
            "github",
            "search"
        ));
    }

    #[test]
    fn sanitize_for_prompt_replaces_ansi_escape_sequences() {
        let raw = "evil\x1b[2J\x1b[Happroved";
        let sanitized = sanitize_for_prompt(raw);
        assert!(!sanitized.contains('\x1b'));
        assert!(sanitized.contains("approved"));
    }

    #[test]
    fn sanitize_for_prompt_caps_length() {
        let raw = "a".repeat(500);
        let sanitized = sanitize_for_prompt(&raw);
        assert!(sanitized.chars().count() <= 201);
        assert!(sanitized.ends_with('…'));
    }

    #[test]
    fn sanitize_for_prompt_keeps_tabs_and_letters() {
        let raw = "name\twith\ttabs";
        let sanitized = sanitize_for_prompt(raw);
        assert_eq!(sanitized, "name\twith\ttabs");
    }

    #[test]
    fn default_registry_includes_dispatch_subagent_only_below_max_depth() {
        let approval = ApprovalConfig::default();
        let root = default_registry_with_context(
            AppConfig::default(),
            0,
            Rc::new(RefCell::new(TodoList::default())),
        );
        assert!(root
            .names_for_policy(&ExecutionPolicy::new(&approval, None))
            .contains(&"dispatch_subagent"));

        let nested = default_registry_with_context(
            AppConfig::default(),
            MAX_SUBAGENT_DEPTH,
            Rc::new(RefCell::new(TodoList::default())),
        );
        assert!(!nested
            .names_for_policy(&ExecutionPolicy::new(&approval, None))
            .contains(&"dispatch_subagent"));
    }

    #[test]
    fn default_registry_exposes_mcp_bridge_tools_when_config_exists() {
        let root = temp_root("mcp");
        std::fs::create_dir_all(&root).unwrap();
        let mcp_file = root.join("mcp.json");
        std::fs::write(
            &mcp_file,
            r#"{"mcpServers":{"fake":{"disabled":true,"transport":"stdio"}}}"#,
        )
        .unwrap();

        let mut config = AppConfig::default();
        config.mcp.project_file = mcp_file.display().to_string();
        config.mcp.user_file = root.join("missing-user.json").display().to_string();
        let registry =
            default_registry_with_context(config, 0, Rc::new(RefCell::new(TodoList::default())));
        let approval = ApprovalConfig::default();
        let names = registry.names_for_policy(&ExecutionPolicy::new(&approval, None));

        assert!(names.contains(&"mcp_list_tools"));
        assert!(names.contains(&"mcp_call"));

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn default_registry_hides_mcp_bridge_tools_without_config_files() {
        let root = temp_root("mcp-missing");
        let mut config = AppConfig::default();
        config.mcp.project_file = root.join("missing-project.json").display().to_string();
        config.mcp.user_file = root.join("missing-user.json").display().to_string();
        let registry =
            default_registry_with_context(config, 0, Rc::new(RefCell::new(TodoList::default())));
        let approval = ApprovalConfig::default();
        let names = registry.names_for_policy(&ExecutionPolicy::new(&approval, None));

        assert!(!names.contains(&"mcp_list_tools"));
        assert!(!names.contains(&"mcp_call"));
    }
}
