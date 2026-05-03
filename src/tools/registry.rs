use std::env;

use crate::config::types::ApprovalConfig;
use crate::tools::apply_patch::ApplyPatchTool;
use crate::tools::git_diff::GitDiffTool;
use crate::tools::list_files::ListFilesTool;
use crate::tools::read_file::ReadFileTool;
use crate::tools::run_shell::{is_safe_shell_command, RunShellTool};
use crate::tools::search_text::SearchTextTool;
use crate::tools::types::{Tool, ToolInput, ToolOutput};
use crate::error::{app_error, policy_denied, tool_failure, AppResult};
use crate::skills::schema::SkillSpec;
use crate::ui::confirm::confirm;

pub struct ToolRegistry {
    tools: Vec<Box<dyn Tool>>,
}

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

        if name == "apply_patch" && policy.require_write_confirmation && !policy.auto_approve_writes {
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
    shell_allowlist: Vec<String>,
    auto_approve_writes: bool,
    auto_approve_shell: bool,
}

impl ExecutionPolicy {
    pub fn new(approval: &ApprovalConfig, skill: Option<&SkillSpec>) -> Self {
        let (allowed_tools, require_write_confirmation, require_shell_confirmation, shell_allowlist) =
            if let Some(skill) = skill {
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
            shell_allowlist,
            auto_approve_writes: env_flag("DSCODE_AUTO_APPROVE_WRITES"),
            auto_approve_shell: env_flag("DSCODE_AUTO_APPROVE_SHELL"),
        }
    }

    pub fn allows_tool(&self, name: &str) -> bool {
        self.allowed_tools.is_empty() || self.allowed_tools.iter().any(|tool| tool == name)
    }
}

fn env_flag(name: &str) -> bool {
    matches!(env::var(name).ok().as_deref(), Some("1") | Some("true") | Some("TRUE"))
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
    default_registry_with_todos(std::rc::Rc::new(std::cell::RefCell::new(
        crate::core::todos::TodoList::default(),
    )))
}

pub fn default_registry_with_todos(
    todos: std::rc::Rc<std::cell::RefCell<crate::core::todos::TodoList>>,
) -> ToolRegistry {
    ToolRegistry {
        tools: vec![
            Box::new(ListFilesTool),
            Box::new(ReadFileTool),
            Box::new(SearchTextTool),
            Box::new(ApplyPatchTool),
            Box::new(RunShellTool),
            Box::new(GitDiffTool),
            Box::new(crate::tools::todo::TodoWriteTool { list: todos }),
        ],
    }
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
            shell_allowlist: Vec::new(),
            auto_approve_writes: false,
            auto_approve_shell: false,
        }
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
}
