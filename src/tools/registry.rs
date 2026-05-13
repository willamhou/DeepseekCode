use std::cell::RefCell;
use std::collections::BTreeSet;
use std::env;
use std::rc::Rc;

use crate::config::types::AppConfig;
use crate::config::types::{ApprovalConfig, NetworkConfig};
use crate::core::todos::TodoList;
use crate::error::{app_error, policy_denied, tool_failure, AppResult};
use crate::skills::schema::SkillSpec;
use crate::tools::apply_patch::ApplyPatchTool;
use crate::tools::diagnostics::DiagnosticsTool;
use crate::tools::dispatch_subagent::{DispatchSubagentTool, DispatchSubagentsTool};
use crate::tools::document::{ImageOcrTool, PandocConvertTool};
use crate::tools::exec_shell::{
    ExecShellCancelTool, ExecShellInteractTool, ExecShellListTool, ExecShellReplayTool,
    ExecShellShowTool, ExecShellTool, ExecShellWaitTool, TaskShellStartTool, TaskShellWaitTool,
};
use crate::tools::file_search::FileSearchTool;
use crate::tools::file_write::{EditFileTool, FimEditTool, WriteFileTool};
use crate::tools::git_diff::{GitDiffTool, GitStatusTool};
use crate::tools::git_history::{GitBlameTool, GitLogTool, GitShowTool};
use crate::tools::github::{
    GithubCloseIssueTool, GithubCommentTool, GithubIssueContextTool, GithubPrContextTool,
    GithubPrReviewCommentTool,
};
use crate::tools::list_files::{ListDirTool, ListFilesTool};
use crate::tools::mcp::{
    remote_tool_registry_name, McpCallTool, McpGetPromptTool, McpListPromptsTool,
    McpListResourceTemplatesTool, McpListResourcesTool, McpListToolsTool, McpReadResourceTool,
    McpRemoteToolTool,
};
use crate::tools::notes::{NoteTool, RememberTool};
use crate::tools::notify::NotifyTool;
use crate::tools::project_map::ProjectMapTool;
use crate::tools::read_file::ReadFileTool;
use crate::tools::recall_archive::RecallArchiveTool;
use crate::tools::revert_turn::RevertTurnTool;
use crate::tools::review::{PrReviewCommentPlanTool, ReviewTool};
use crate::tools::rlm::{
    RlmBatchTool, RlmChunkPlanTool, RlmLiveCancelTool, RlmLiveDrainTool, RlmLiveEventsTool,
    RlmLiveRunNextTool, RlmLiveWaitTool, RlmMapReducePlanTool, RlmModelSessionsTool,
    RlmPythonSessionTool, RlmPythonSessionsTool, RlmPythonTool, RlmRecursivePlanTool, RlmTool,
};
use crate::tools::run_shell::{is_safe_shell_command, RunShellTool};
use crate::tools::run_tests::{render_run_tests_command, RunTestsTool};
use crate::tools::runtime_tasks::{
    AgentCancelTool, AgentCloseTool, AgentListTool, AgentResultTool, AgentResumeTool,
    AgentSendInputTool, AgentSpawnTool, AutomationCreateTool, AutomationDeleteTool,
    AutomationListTool, AutomationPauseTool, AutomationReadTool, AutomationResumeTool,
    AutomationRunTool, AutomationUpdateTool, PrAttemptListTool, PrAttemptPreflightTool,
    PrAttemptReadTool, PrAttemptRecordTool, TaskCancelTool, TaskCreateTool, TaskGateRunTool,
    TaskListTool, TaskReadTool,
};
use crate::tools::search_text::{GrepFilesTool, SearchTextTool};
use crate::tools::skill::LoadSkillTool;
use crate::tools::todo::{
    TodoAddTool, TodoListTool, TodoUpdateTool, TodoWriteAliasTool, TodoWriteTool, UpdatePlanTool,
};
use crate::tools::tool_output::RetrieveToolResultTool;
use crate::tools::tool_search::{ToolSearchMode, ToolSearchTool};
use crate::tools::types::{Tool, ToolInput, ToolOutput};
use crate::tools::user_input::RequestUserInputTool;
use crate::tools::validate_data::ValidateDataTool;
use crate::tools::vision::ImageAnalyzeTool;
use crate::tools::web::{
    network_permission_target_for_tool, FetchUrlTool, FinanceTool, WebRunTool, WebSearchTool,
    NETWORK_APPROVED_ARG,
};
use crate::ui::confirm::confirm;
use crate::util::cancel::CancellationCheck;

pub struct ToolRegistry {
    tools: Vec<Box<dyn Tool>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionRequest {
    pub kind: String,
    pub target: String,
}

pub const MAX_SUBAGENT_DEPTH: usize = 2;
const MAX_DYNAMIC_MCP_TOOLS: usize = 24;

impl ToolRegistry {
    pub fn names_for_policy(&self, policy: &ExecutionPolicy) -> Vec<&str> {
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
        self.execute_with_policy_and_cancel(name, input, policy, None)
    }

    pub fn execute_with_policy_and_cancel(
        &self,
        name: &str,
        mut input: ToolInput,
        policy: &ExecutionPolicy,
        cancel_check: Option<&mut dyn CancellationCheck>,
    ) -> AppResult<ToolOutput> {
        let tool = self
            .tools
            .iter()
            .find(|tool| tool.name() == name)
            .ok_or_else(|| app_error(format!("unknown tool: {name}")))?;
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

        if name == "revert_turn" && policy.require_write_confirmation && !policy.auto_approve_writes
        {
            let target = describe_revert_turn_target(&input);
            let prompt = format!(
                "Restore rollback snapshot {}?",
                sanitize_for_prompt(&target)
            );
            if !confirm(&prompt) {
                return Err(policy_denied(format!(
                    "rollback restore declined for {}; set DSCODE_AUTO_APPROVE_WRITES=1 to skip prompts or relax the active policy",
                    sanitize_for_prompt(&target)
                )));
            }
        }

        if is_runtime_mutation_tool(name)
            && policy.require_write_confirmation
            && !policy.auto_approve_writes
        {
            let target = describe_runtime_task_target(name, &input);
            let prompt = format!(
                "Mutate runtime task state: {}?",
                sanitize_for_prompt(&target)
            );
            if !confirm(&prompt) {
                return Err(policy_denied(format!(
                    "runtime task mutation declined for {}; set DSCODE_AUTO_APPROVE_WRITES=1 to skip prompts or relax the active policy",
                    sanitize_for_prompt(&target)
                )));
            }
        }

        if name == "run_shell"
            || name == "exec_shell"
            || name == "task_shell_start"
            || name == "task_gate_run"
        {
            let command = input
                .get("command")
                .ok_or_else(|| app_error(format!("{name} requires a command")))?;
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

        if name == "run_tests" && policy.require_shell_confirmation && !policy.auto_approve_shell {
            let command = describe_run_tests_target(&input);
            if !policy.shell_allowlist.is_empty()
                && !policy
                    .shell_allowlist
                    .iter()
                    .any(|prefix| command.trim().starts_with(prefix))
            {
                return Err(policy_denied(format!(
                    "test command blocked by policy allowlist: {}",
                    sanitize_for_prompt(&command)
                )));
            }
            let prompt = format!(
                "Run tests in {}: '{}'?",
                sanitize_for_prompt(input.get("cwd").unwrap_or(".")),
                sanitize_for_prompt(&command)
            );
            if !confirm(&prompt) {
                return Err(policy_denied(
                    "test command declined; set DSCODE_AUTO_APPROVE_SHELL=1 to skip prompts or relax the active policy",
                ));
            }
        }

        let mcp_target = if name == "mcp_call" {
            Some(mcp_call_target(&input)?)
        } else {
            tool.mcp_target()
        };
        if let Some((server, remote_tool)) = mcp_target {
            let target = format!("{server}/{remote_tool}");

            if !policy.mcp_call_allowlist.is_empty()
                && !mcp_call_matches_allowlist(&policy.mcp_call_allowlist, server, remote_tool)
            {
                return Err(policy_denied(format!(
                    "mcp tool call blocked by policy allowlist: {}; add an entry to approval.mcp_call_allowlist or use server/*",
                    sanitize_for_prompt(&target)
                )));
            }

            if policy.require_mcp_confirmation && !policy.auto_approve_mcp {
                let args = describe_mcp_arguments(&input);
                let prompt = format!(
                    "Call MCP tool {} with {}?",
                    sanitize_for_prompt(&target),
                    sanitize_for_prompt(&args)
                );
                if !confirm(&prompt) {
                    return Err(policy_denied(format!(
                        "mcp tool call declined for {}; set DSCODE_AUTO_APPROVE_MCP=1 to skip prompts or relax approval.require_mcp_confirmation",
                        sanitize_for_prompt(&target)
                    )));
                }
            }
        }

        if policy.auto_approve_network {
            input
                .args
                .insert(NETWORK_APPROVED_ARG.to_string(), "true".to_string());
        }

        tool.execute_with_cancel(input, cancel_check)
            .map_err(|error| {
                if error.downcast_ref::<crate::error::AppError>().is_some() {
                    error
                } else {
                    tool_failure(error.to_string())
                }
            })
    }

    pub fn permission_request_for(
        &self,
        name: &str,
        input: &ToolInput,
        policy: &ExecutionPolicy,
    ) -> Option<PermissionRequest> {
        let tool = self.tools.iter().find(|tool| tool.name() == name)?;
        if !policy.allows_tool(name) {
            return None;
        }

        if name == "apply_patch" && policy.require_write_confirmation && !policy.auto_approve_writes
        {
            return Some(PermissionRequest {
                kind: "write".to_string(),
                target: describe_apply_patch_target(input),
            });
        }

        if name == "revert_turn" && policy.require_write_confirmation && !policy.auto_approve_writes
        {
            return Some(PermissionRequest {
                kind: "write".to_string(),
                target: describe_revert_turn_target(input),
            });
        }

        if (name == "write_file" || name == "edit_file" || name == "fim_edit")
            && policy.require_write_confirmation
            && !policy.auto_approve_writes
        {
            return Some(PermissionRequest {
                kind: "write".to_string(),
                target: describe_file_write_target(name, input),
            });
        }

        if name == "pandoc_convert"
            && input
                .get("output_path")
                .map(|value| !value.trim().is_empty())
                .unwrap_or(false)
            && policy.require_write_confirmation
            && !policy.auto_approve_writes
        {
            return Some(PermissionRequest {
                kind: "write".to_string(),
                target: describe_file_write_target(name, input),
            });
        }

        if (name == "github_comment"
            || name == "github_pr_review_comment"
            || name == "github_close_issue")
            && policy.require_write_confirmation
            && !policy.auto_approve_writes
        {
            return Some(PermissionRequest {
                kind: "write".to_string(),
                target: describe_github_write_target(name, input),
            });
        }

        if is_runtime_mutation_tool(name)
            && policy.require_write_confirmation
            && !policy.auto_approve_writes
        {
            return Some(PermissionRequest {
                kind: "write".to_string(),
                target: describe_runtime_task_target(name, input),
            });
        }

        if (name == "run_shell"
            || name == "exec_shell"
            || name == "task_shell_start"
            || name == "task_gate_run")
            && policy.require_shell_confirmation
            && !policy.auto_approve_shell
        {
            let command = input.get("command")?;
            if !policy.shell_allowlist.is_empty()
                && !policy
                    .shell_allowlist
                    .iter()
                    .any(|prefix| command.trim().starts_with(prefix))
            {
                return None;
            }
            if !is_safe_shell_command(command) {
                return None;
            }
            return Some(PermissionRequest {
                kind: "shell".to_string(),
                target: command.to_string(),
            });
        }
        if name == "run_tests" && policy.require_shell_confirmation && !policy.auto_approve_shell {
            let target = describe_run_tests_target(input);
            if !policy.shell_allowlist.is_empty()
                && !policy
                    .shell_allowlist
                    .iter()
                    .any(|prefix| target.trim().starts_with(prefix))
            {
                return None;
            }
            return Some(PermissionRequest {
                kind: "shell".to_string(),
                target,
            });
        }

        if !policy.auto_approve_network {
            if let Some(target) = network_permission_target_for_tool(name, input, &policy.network) {
                return Some(PermissionRequest {
                    kind: "network".to_string(),
                    target,
                });
            }
        }

        let mcp_target = if name == "mcp_call" {
            mcp_call_target(input).ok()
        } else {
            tool.mcp_target()
        };
        if let Some((server, remote_tool)) = mcp_target {
            if policy.require_mcp_confirmation && !policy.auto_approve_mcp {
                if !policy.mcp_call_allowlist.is_empty()
                    && !mcp_call_matches_allowlist(&policy.mcp_call_allowlist, server, remote_tool)
                {
                    return None;
                }
                return Some(PermissionRequest {
                    kind: "mcp".to_string(),
                    target: format!("{server}/{remote_tool}"),
                });
            }
        }

        None
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
    network: NetworkConfig,
    auto_approve_writes: bool,
    auto_approve_shell: bool,
    auto_approve_mcp: bool,
    auto_approve_network: bool,
}

impl ExecutionPolicy {
    pub fn new(approval: &ApprovalConfig, skill: Option<&SkillSpec>) -> Self {
        Self::with_network(approval, &NetworkConfig::default(), skill)
    }

    pub fn with_network(
        approval: &ApprovalConfig,
        network: &NetworkConfig,
        skill: Option<&SkillSpec>,
    ) -> Self {
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
            network: network.clone(),
            auto_approve_writes: env_flag("DSCODE_AUTO_APPROVE_WRITES"),
            auto_approve_shell: env_flag("DSCODE_AUTO_APPROVE_SHELL"),
            auto_approve_mcp: env_flag("DSCODE_AUTO_APPROVE_MCP"),
            auto_approve_network: env_flag("DSCODE_AUTO_APPROVE_NETWORK"),
        }
    }

    pub fn allows_tool(&self, name: &str) -> bool {
        self.allowed_tools.is_empty() || self.allowed_tools.iter().any(|tool| tool == name)
    }

    pub fn with_auto_approved_permission(&self, kind: &str) -> Self {
        let mut policy = self.clone();
        match kind {
            "write" => policy.auto_approve_writes = true,
            "shell" => policy.auto_approve_shell = true,
            "mcp" => policy.auto_approve_mcp = true,
            "network" => policy.auto_approve_network = true,
            _ => {}
        }
        policy
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

fn describe_revert_turn_target(input: &ToolInput) -> String {
    if let Some(id) = input
        .get("snapshot_id")
        .or_else(|| input.get("checkpoint_id"))
        .or_else(|| input.get("id"))
    {
        return format!("snapshot {id}");
    }
    if let Some(turn_id) = input.get("turn_id") {
        return format!("turn {turn_id}");
    }
    format!(
        "turn_offset {}",
        input
            .get("turn_offset")
            .or_else(|| input.get("offset"))
            .unwrap_or("1")
    )
}

fn describe_file_write_target(name: &str, input: &ToolInput) -> String {
    let path = input.get("path").unwrap_or("?");
    match name {
        "write_file" => format!("write {path}"),
        "edit_file" => format!("edit {path}"),
        "fim_edit" => format!("fim edit {path}"),
        "pandoc_convert" => {
            let output = input.get("output_path").unwrap_or(path);
            format!("pandoc convert {output}")
        }
        _ => path.to_string(),
    }
}

fn describe_github_write_target(name: &str, input: &ToolInput) -> String {
    let number = input
        .get("number")
        .or_else(|| input.get("issue"))
        .or_else(|| input.get("pr"))
        .or_else(|| input.get("ref"))
        .unwrap_or("?");
    let repo = input
        .get("repo")
        .or_else(|| input.get("repository"))
        .map(|value| format!(" in {value}"))
        .unwrap_or_default();
    match name {
        "github_comment" => {
            let target = input.get("target").unwrap_or("issue/pr");
            format!("github {target} #{number} comment{repo}")
        }
        "github_pr_review_comment" => {
            let path = input.get("path").unwrap_or("inline");
            let line = input.get("line").unwrap_or("?");
            format!("github pr #{number} inline comment {path}:{line}{repo}")
        }
        "github_close_issue" => format!("github issue #{number} close{repo}"),
        _ => format!("github write #{number}{repo}"),
    }
}

fn describe_run_tests_target(input: &ToolInput) -> String {
    render_run_tests_command(input).unwrap_or_else(|_| "run_tests".to_string())
}

fn is_runtime_mutation_tool(name: &str) -> bool {
    matches!(
        name,
        "task_create"
            | "task_cancel"
            | "automation_create"
            | "automation_update"
            | "automation_pause"
            | "automation_resume"
            | "automation_delete"
            | "automation_run"
            | "agent_spawn"
            | "agent_cancel"
            | "close_agent"
            | "resume_agent"
            | "send_input"
    )
}

fn describe_runtime_task_target(name: &str, input: &ToolInput) -> String {
    match name {
        "task_create" => {
            let summary = input
                .get("prompt")
                .or_else(|| input.get("summary"))
                .unwrap_or("?");
            format!("create runtime task: {}", sanitize_for_prompt(summary))
        }
        "task_cancel" => {
            let id = input
                .get("task_id")
                .or_else(|| input.get("id"))
                .unwrap_or("?");
            format!("cancel runtime task {id}")
        }
        "automation_create" => {
            let name = input.get("name").unwrap_or("?");
            format!("create automation: {}", sanitize_for_prompt(name))
        }
        "automation_run" => {
            let id = input
                .get("automation_id")
                .or_else(|| input.get("id"))
                .unwrap_or("?");
            format!("run automation {id}")
        }
        "automation_update" => {
            let id = input
                .get("automation_id")
                .or_else(|| input.get("id"))
                .unwrap_or("?");
            format!("update automation {id}")
        }
        "automation_pause" => {
            let id = input
                .get("automation_id")
                .or_else(|| input.get("id"))
                .unwrap_or("?");
            format!("pause automation {id}")
        }
        "automation_resume" => {
            let id = input
                .get("automation_id")
                .or_else(|| input.get("id"))
                .unwrap_or("?");
            format!("resume automation {id}")
        }
        "automation_delete" => {
            let id = input
                .get("automation_id")
                .or_else(|| input.get("id"))
                .unwrap_or("?");
            format!("delete automation {id}")
        }
        "agent_spawn" => {
            let prompt = input
                .get("prompt")
                .or_else(|| input.get("message"))
                .or_else(|| input.get("objective"))
                .or_else(|| input.get("task"))
                .unwrap_or("?");
            format!("spawn sub-agent: {}", sanitize_for_prompt(prompt))
        }
        "agent_cancel" | "close_agent" | "resume_agent" | "send_input" => {
            let id = input
                .get("agent_id")
                .or_else(|| input.get("id"))
                .unwrap_or("?");
            format!("{} {id}", name.replace('_', " "))
        }
        _ => "runtime task".to_string(),
    }
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

fn describe_mcp_arguments(input: &ToolInput) -> String {
    if let Some(arguments) = input.get("arguments") {
        let trimmed = arguments.trim();
        if trimmed.is_empty() {
            return "{}".to_string();
        }
        return trimmed.to_string();
    }
    let args = input
        .args
        .iter()
        .filter(|(key, _)| !matches!(key.as_str(), "server" | "tool"))
        .map(|(key, value)| format!("{key}={value}"))
        .collect::<Vec<_>>();
    if args.is_empty() {
        "{}".to_string()
    } else {
        args.join(", ")
    }
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
        Box::new(ListDirTool),
        Box::new(ReadFileTool),
        Box::new(RetrieveToolResultTool),
        Box::new(SearchTextTool),
        Box::new(GrepFilesTool),
        Box::new(FileSearchTool),
        Box::new(WebRunTool),
        Box::new(WebSearchTool),
        Box::new(FetchUrlTool),
        Box::new(FinanceTool),
        Box::new(PandocConvertTool),
        Box::new(ImageOcrTool),
        Box::new(ImageAnalyzeTool::new(&config)),
        Box::new(ReviewTool::new(config.clone(), subagent_depth)),
        Box::new(PrReviewCommentPlanTool),
        Box::new(ToolSearchTool {
            tool_name: "tool_search_tool_regex",
            mode: ToolSearchMode::Regex,
        }),
        Box::new(ToolSearchTool {
            tool_name: "tool_search_tool_bm25",
            mode: ToolSearchMode::Bm25,
        }),
        Box::new(RequestUserInputTool),
        Box::new(ApplyPatchTool::new(config.diagnostics.clone())),
        Box::new(WriteFileTool),
        Box::new(EditFileTool),
        Box::new(FimEditTool::new(&config)),
        Box::new(RunShellTool),
        Box::new(ExecShellTool),
        Box::new(TaskShellStartTool),
        Box::new(TaskShellWaitTool),
        Box::new(ExecShellWaitTool {
            tool_name: "exec_shell_wait",
        }),
        Box::new(ExecShellWaitTool {
            tool_name: "exec_wait",
        }),
        Box::new(ExecShellListTool),
        Box::new(ExecShellShowTool),
        Box::new(ExecShellReplayTool),
        Box::new(ExecShellInteractTool {
            tool_name: "exec_shell_interact",
        }),
        Box::new(ExecShellInteractTool {
            tool_name: "exec_interact",
        }),
        Box::new(ExecShellCancelTool),
        Box::new(GitStatusTool),
        Box::new(GitDiffTool),
        Box::new(ProjectMapTool),
        Box::new(DiagnosticsTool),
        Box::new(ValidateDataTool),
        Box::new(RecallArchiveTool::new(&config)),
        Box::new(RunTestsTool),
        Box::new(TaskCreateTool::new(&config)),
        Box::new(TaskListTool::new(&config)),
        Box::new(TaskReadTool::new(&config)),
        Box::new(TaskCancelTool::new(&config)),
        Box::new(TaskGateRunTool),
        Box::new(AgentSpawnTool::new(&config)),
        Box::new(AgentResultTool::new(&config)),
        Box::new(AgentListTool::new(&config)),
        Box::new(AgentCancelTool::new(&config)),
        Box::new(AgentCloseTool::new(&config)),
        Box::new(AgentResumeTool::new(&config)),
        Box::new(AgentSendInputTool::new(&config)),
        Box::new(PrAttemptRecordTool::new(&config)),
        Box::new(PrAttemptListTool::new(&config)),
        Box::new(PrAttemptReadTool::new(&config)),
        Box::new(PrAttemptPreflightTool::new(&config)),
        Box::new(AutomationCreateTool::new(&config)),
        Box::new(AutomationListTool::new(&config)),
        Box::new(AutomationReadTool::new(&config)),
        Box::new(AutomationUpdateTool::new(&config)),
        Box::new(AutomationPauseTool::new(&config)),
        Box::new(AutomationResumeTool::new(&config)),
        Box::new(AutomationDeleteTool::new(&config)),
        Box::new(AutomationRunTool::new(&config)),
        Box::new(RevertTurnTool::new(
            std::path::PathBuf::from(&config.workspace.config_dir).join("rollback"),
        )),
        Box::new(GitLogTool),
        Box::new(GitShowTool),
        Box::new(GitBlameTool),
        Box::new(LoadSkillTool::new(config.clone())),
        Box::new(NoteTool::new(config.memory.notes_path())),
        Box::new(NotifyTool),
        Box::new(GithubIssueContextTool),
        Box::new(GithubPrContextTool),
        Box::new(GithubCommentTool),
        Box::new(GithubPrReviewCommentTool),
        Box::new(GithubCloseIssueTool),
        Box::new(TodoWriteTool {
            list: todos.clone(),
        }),
        Box::new(UpdatePlanTool {
            list: todos.clone(),
        }),
        Box::new(TodoWriteAliasTool {
            list: todos.clone(),
            tool_name: "checklist_write",
        }),
        Box::new(TodoAddTool {
            list: todos.clone(),
            tool_name: "todo_add",
        }),
        Box::new(TodoAddTool {
            list: todos.clone(),
            tool_name: "checklist_add",
        }),
        Box::new(TodoUpdateTool {
            list: todos.clone(),
            tool_name: "todo_update",
        }),
        Box::new(TodoUpdateTool {
            list: todos.clone(),
            tool_name: "checklist_update",
        }),
        Box::new(TodoListTool {
            list: todos.clone(),
            tool_name: "todo_list",
        }),
        Box::new(TodoListTool {
            list: todos.clone(),
            tool_name: "checklist_list",
        }),
    ];
    if config.memory.enabled {
        tools.push(Box::new(RememberTool::new(config.memory.memory_path())));
    }
    if expose_mcp_tools {
        tools.push(Box::new(McpListToolsTool {
            config: config.clone(),
        }));
        tools.push(Box::new(McpCallTool {
            config: config.clone(),
        }));
        tools.push(Box::new(McpListPromptsTool {
            config: config.clone(),
        }));
        tools.push(Box::new(McpGetPromptTool {
            config: config.clone(),
        }));
        tools.push(Box::new(McpListResourcesTool {
            config: config.clone(),
        }));
        tools.push(Box::new(McpReadResourceTool {
            config: config.clone(),
        }));
        tools.push(Box::new(McpListResourceTemplatesTool {
            config: config.clone(),
        }));
        if config.mcp.expose_remote_tools {
            let mut names = tools
                .iter()
                .map(|tool| tool.name().to_string())
                .collect::<BTreeSet<_>>();
            for remote in crate::cli::commands::mcp::discover_remote_tools_for_agent(
                &config,
                MAX_DYNAMIC_MCP_TOOLS,
            ) {
                let name = remote_tool_registry_name(&remote.server, &remote.tool);
                if names.insert(name.clone()) {
                    crate::tools::mcp::cache_dynamic_tool_schema(
                        &name,
                        remote.description.clone(),
                        remote.input_schema.clone(),
                    );
                    tools.push(Box::new(McpRemoteToolTool {
                        name,
                        server: remote.server,
                        tool: remote.tool,
                        config: config.clone(),
                    }));
                }
            }
        }
    }
    if subagent_depth < MAX_SUBAGENT_DEPTH {
        tools.push(Box::new(DispatchSubagentTool {
            config: config.clone(),
            parent_depth: subagent_depth,
        }));
        tools.push(Box::new(DispatchSubagentsTool {
            config: config.clone(),
            parent_depth: subagent_depth,
        }));
        tools.push(Box::new(RlmTool {
            tool_name: "rlm",
            config: config.clone(),
            parent_depth: subagent_depth,
        }));
        tools.push(Box::new(RlmTool {
            tool_name: "rlm_query",
            config: config.clone(),
            parent_depth: subagent_depth,
        }));
        tools.push(Box::new(RlmTool {
            tool_name: "llm_query",
            config: config.clone(),
            parent_depth: subagent_depth,
        }));
        tools.push(Box::new(RlmTool {
            tool_name: "rlm_process",
            config: config.clone(),
            parent_depth: subagent_depth,
        }));
        tools.push(Box::new(RlmChunkPlanTool));
        tools.push(Box::new(RlmMapReducePlanTool));
        tools.push(Box::new(RlmRecursivePlanTool));
        tools.push(Box::new(RlmPythonTool));
        tools.push(Box::new(RlmPythonSessionTool {
            config: config.clone(),
        }));
        tools.push(Box::new(RlmPythonSessionsTool {
            config: config.clone(),
        }));
        tools.push(Box::new(RlmModelSessionsTool {
            config: config.clone(),
        }));
        tools.push(Box::new(RlmLiveEventsTool {
            config: config.clone(),
        }));
        tools.push(Box::new(RlmLiveWaitTool {
            config: config.clone(),
        }));
        tools.push(Box::new(RlmLiveCancelTool {
            config: config.clone(),
        }));
        tools.push(Box::new(RlmLiveRunNextTool {
            config: config.clone(),
            parent_depth: subagent_depth,
        }));
        tools.push(Box::new(RlmLiveDrainTool {
            config: config.clone(),
            parent_depth: subagent_depth,
        }));
        tools.push(Box::new(RlmBatchTool {
            tool_name: "rlm_batch",
            config: config.clone(),
            parent_depth: subagent_depth,
        }));
        tools.push(Box::new(RlmBatchTool {
            tool_name: "rlm_query_batched",
            config: config.clone(),
            parent_depth: subagent_depth,
        }));
        tools.push(Box::new(RlmBatchTool {
            tool_name: "llm_query_batched",
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
            network: NetworkConfig::default(),
            auto_approve_writes: false,
            auto_approve_shell: false,
            auto_approve_mcp: false,
            auto_approve_network: false,
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

    fn fake_mcp_server_config(root: &std::path::Path, expose_remote_tools: bool) -> AppConfig {
        std::fs::create_dir_all(root).unwrap();
        let server = root.join("server.sh");
        std::fs::write(
            &server,
            r#"#!/bin/sh
while IFS= read -r line; do
  case "$line" in
    *'"method":"initialize"'*)
      printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-11-25","capabilities":{"tools":{}},"serverInfo":{"name":"fake","version":"1"}}}'
      ;;
    *'"method":"tools/list"'*)
      printf '%s\n' '{"jsonrpc":"2.0","id":2,"result":{"tools":[{"name":"echo","description":"Echo input","inputSchema":{"type":"object","properties":{"text":{"type":"string"}}}}]}}'
      exit 0
      ;;
    *'"method":"tools/call"'*)
      printf '%s\n' '{"jsonrpc":"2.0","id":2,"result":{"content":[{"type":"text","text":"echo: hello"}],"structuredContent":{"ok":true},"isError":false}}'
      exit 0
      ;;
  esac
done
"#,
        )
        .unwrap();
        let mcp_file = root.join("mcp.json");
        std::fs::write(
            &mcp_file,
            format!(
                r#"{{"mcpServers":{{"fake":{{"transport":"stdio","command":"/bin/sh","args":["{}"]}}}}}}"#,
                server.display()
            ),
        )
        .unwrap();

        let mut config = AppConfig::default();
        config.mcp.project_file = mcp_file.display().to_string();
        config.mcp.user_file = root.join("missing-user.json").display().to_string();
        config.mcp.expose_remote_tools = expose_remote_tools;
        config
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
    fn default_registry_includes_read_only_git_history_tools() {
        let registry = default_registry();
        let approval = ApprovalConfig::default();
        let policy = ExecutionPolicy::new(&approval, None);
        let names = registry.names_for_policy(&policy);

        assert!(names.contains(&"list_dir"));
        assert!(names.contains(&"retrieve_tool_result"));
        assert!(names.contains(&"grep_files"));
        assert!(names.contains(&"file_search"));
        assert!(names.contains(&"web_run"));
        assert!(names.contains(&"web_search"));
        assert!(names.contains(&"fetch_url"));
        assert!(names.contains(&"finance"));
        assert!(names.contains(&"pandoc_convert"));
        assert!(names.contains(&"image_ocr"));
        assert!(names.contains(&"image_analyze"));
        assert!(names.contains(&"review"));
        assert!(names.contains(&"tool_search_tool_regex"));
        assert!(names.contains(&"tool_search_tool_bm25"));
        assert!(names.contains(&"request_user_input"));
        assert!(names.contains(&"write_file"));
        assert!(names.contains(&"edit_file"));
        assert!(names.contains(&"fim_edit"));
        assert!(names.contains(&"task_shell_start"));
        assert!(names.contains(&"task_shell_wait"));
        assert!(names.contains(&"git_status"));
        assert!(names.contains(&"project_map"));
        assert!(names.contains(&"validate_data"));
        assert!(names.contains(&"recall_archive"));
        assert!(names.contains(&"task_create"));
        assert!(names.contains(&"task_list"));
        assert!(names.contains(&"task_read"));
        assert!(names.contains(&"task_cancel"));
        assert!(names.contains(&"task_gate_run"));
        assert!(names.contains(&"agent_spawn"));
        assert!(names.contains(&"agent_result"));
        assert!(names.contains(&"agent_list"));
        assert!(names.contains(&"agent_cancel"));
        assert!(names.contains(&"close_agent"));
        assert!(names.contains(&"resume_agent"));
        assert!(names.contains(&"send_input"));
        assert!(names.contains(&"pr_attempt_record"));
        assert!(names.contains(&"pr_attempt_list"));
        assert!(names.contains(&"pr_attempt_read"));
        assert!(names.contains(&"pr_attempt_preflight"));
        assert!(names.contains(&"automation_create"));
        assert!(names.contains(&"automation_list"));
        assert!(names.contains(&"automation_read"));
        assert!(names.contains(&"automation_update"));
        assert!(names.contains(&"automation_pause"));
        assert!(names.contains(&"automation_resume"));
        assert!(names.contains(&"automation_delete"));
        assert!(names.contains(&"automation_run"));
        assert!(names.contains(&"revert_turn"));
        assert!(names.contains(&"git_log"));
        assert!(names.contains(&"git_show"));
        assert!(names.contains(&"git_blame"));
        assert!(names.contains(&"load_skill"));
        assert!(names.contains(&"note"));
        assert!(names.contains(&"notify"));
        assert!(!names.contains(&"remember"));
        assert!(names.contains(&"github_issue_context"));
        assert!(names.contains(&"github_pr_context"));
        assert!(names.contains(&"github_comment"));
        assert!(names.contains(&"github_pr_review_comment"));
        assert!(names.contains(&"github_close_issue"));
        assert!(names.contains(&"diagnostics"));
    }

    #[test]
    fn default_registry_includes_remember_only_when_memory_enabled() {
        let mut config = AppConfig::default();
        config.memory.enabled = true;
        let registry =
            default_registry_with_context(config, 0, Rc::new(RefCell::new(TodoList::default())));
        let approval = ApprovalConfig::default();
        let names = registry.names_for_policy(&ExecutionPolicy::new(&approval, None));

        assert!(names.contains(&"note"));
        assert!(names.contains(&"remember"));
    }

    #[test]
    fn default_registry_includes_run_tests_with_shell_permission_request() {
        let root = temp_root("run-tests-permission");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("Cargo.toml"), "[package]\nname = \"demo\"\n").unwrap();

        let registry = default_registry();
        let policy = ExecutionPolicy {
            allowed_tools: vec!["run_tests".to_string()],
            ..deny_writes_policy()
        };
        let input = ToolInput::new().with_arg("cwd", root.display().to_string());
        let request = registry
            .permission_request_for("run_tests", &input, &policy)
            .unwrap();

        assert_eq!(request.kind, "shell");
        assert_eq!(request.target, "cargo test");
    }

    #[test]
    fn permission_request_for_reports_network_prompt() {
        let registry = default_registry();
        let approval = ApprovalConfig::default();
        let mut network = NetworkConfig::default();
        network.default = "prompt".to_string();
        let policy = ExecutionPolicy::with_network(&approval, &network, None);
        let input = ToolInput::new().with_arg("url", "https://example.com/docs");

        let permission = registry
            .permission_request_for("fetch_url", &input, &policy)
            .expect("network permission request");

        assert_eq!(permission.kind, "network");
        assert!(permission.target.contains("example.com"));

        let approved = policy.with_auto_approved_permission("network");
        assert!(registry
            .permission_request_for("fetch_url", &input, &approved)
            .is_none());
    }

    #[test]
    fn default_registry_includes_exec_shell_background_tools() {
        let registry = default_registry();
        let approval = ApprovalConfig::default();
        let policy = ExecutionPolicy::new(&approval, None);
        let names = registry.names_for_policy(&policy);

        for name in [
            "exec_shell",
            "task_shell_start",
            "task_shell_wait",
            "exec_shell_wait",
            "exec_wait",
            "exec_shell_list",
            "exec_shell_show",
            "exec_shell_replay",
            "exec_shell_interact",
            "exec_interact",
            "exec_shell_cancel",
        ] {
            assert!(names.contains(&name), "missing tool {name}");
        }

        let policy = ExecutionPolicy {
            allowed_tools: vec!["exec_shell".to_string()],
            ..deny_writes_policy()
        };
        let request = registry
            .permission_request_for(
                "exec_shell",
                &ToolInput::new().with_arg("command", "echo hello"),
                &policy,
            )
            .unwrap();
        assert_eq!(request.kind, "shell");
        assert_eq!(request.target, "echo hello");

        let request = registry
            .permission_request_for(
                "task_shell_start",
                &ToolInput::new().with_arg("command", "echo task"),
                &ExecutionPolicy {
                    allowed_tools: vec!["task_shell_start".to_string()],
                    ..deny_writes_policy()
                },
            )
            .unwrap();
        assert_eq!(request.kind, "shell");
        assert_eq!(request.target, "echo task");
    }

    #[test]
    fn default_registry_includes_todo_checklist_compat_tools() {
        let registry = default_registry();
        let approval = ApprovalConfig::default();
        let policy = ExecutionPolicy::new(&approval, None);
        let names = registry.names_for_policy(&policy);

        for name in [
            "todo_write",
            "update_plan",
            "checklist_write",
            "todo_add",
            "checklist_add",
            "todo_update",
            "checklist_update",
            "todo_list",
            "checklist_list",
        ] {
            assert!(names.contains(&name), "missing {name}");
        }
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
    fn permission_request_for_reports_write_shell_and_mcp_prompts() {
        let root = temp_root("permission-request");
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
        let policy = deny_writes_policy();

        let write = registry
            .permission_request_for(
                "apply_patch",
                &ToolInput::new().with_arg("path", "src/lib.rs"),
                &policy,
            )
            .unwrap();
        assert_eq!(write.kind, "write");
        assert_eq!(write.target, "src/lib.rs");

        let edit = registry
            .permission_request_for(
                "edit_file",
                &ToolInput::new().with_arg("path", "src/main.rs"),
                &ExecutionPolicy {
                    allowed_tools: vec!["edit_file".to_string()],
                    ..deny_writes_policy()
                },
            )
            .unwrap();
        assert_eq!(edit.kind, "write");
        assert_eq!(edit.target, "edit src/main.rs");

        let fim = registry
            .permission_request_for(
                "fim_edit",
                &ToolInput::new().with_arg("path", "src/lib.rs"),
                &ExecutionPolicy {
                    allowed_tools: vec!["fim_edit".to_string()],
                    ..deny_writes_policy()
                },
            )
            .unwrap();
        assert_eq!(fim.kind, "write");
        assert_eq!(fim.target, "fim edit src/lib.rs");

        let pandoc = registry
            .permission_request_for(
                "pandoc_convert",
                &ToolInput::new()
                    .with_arg("source_path", "README.md")
                    .with_arg("target_format", "html")
                    .with_arg("output_path", "target/readme.html"),
                &ExecutionPolicy {
                    allowed_tools: vec!["pandoc_convert".to_string()],
                    ..deny_writes_policy()
                },
            )
            .unwrap();
        assert_eq!(pandoc.kind, "write");
        assert_eq!(pandoc.target, "pandoc convert target/readme.html");

        let shell = registry
            .permission_request_for(
                "run_shell",
                &ToolInput::new()
                    .with_arg("cwd", ".")
                    .with_arg("command", "cargo test"),
                &policy,
            )
            .unwrap();
        assert_eq!(shell.kind, "shell");
        assert_eq!(shell.target, "cargo test");

        let github_policy = ExecutionPolicy {
            allowed_tools: vec!["github_comment".to_string()],
            ..deny_writes_policy()
        };
        let github = registry
            .permission_request_for(
                "github_comment",
                &ToolInput::new()
                    .with_arg("target", "pr")
                    .with_arg("number", "7")
                    .with_arg("repo", "owner/repo"),
                &github_policy,
            )
            .unwrap();
        assert_eq!(github.kind, "write");
        assert_eq!(github.target, "github pr #7 comment in owner/repo");

        let inline_github = registry
            .permission_request_for(
                "github_pr_review_comment",
                &ToolInput::new()
                    .with_arg("number", "7")
                    .with_arg("path", "src/lib.rs")
                    .with_arg("line", "12")
                    .with_arg("repo", "owner/repo"),
                &ExecutionPolicy {
                    allowed_tools: vec!["github_pr_review_comment".to_string()],
                    ..deny_writes_policy()
                },
            )
            .unwrap();
        assert_eq!(inline_github.kind, "write");
        assert_eq!(
            inline_github.target,
            "github pr #7 inline comment src/lib.rs:12 in owner/repo"
        );

        let task = registry
            .permission_request_for(
                "task_create",
                &ToolInput::new().with_arg("prompt", "run a durable check"),
                &ExecutionPolicy {
                    allowed_tools: vec!["task_create".to_string()],
                    ..deny_writes_policy()
                },
            )
            .unwrap();
        assert_eq!(task.kind, "write");
        assert_eq!(task.target, "create runtime task: run a durable check");

        let automation = registry
            .permission_request_for(
                "automation_create",
                &ToolInput::new().with_arg("name", "weekly review"),
                &ExecutionPolicy {
                    allowed_tools: vec!["automation_create".to_string()],
                    ..deny_writes_policy()
                },
            )
            .unwrap();
        assert_eq!(automation.kind, "write");
        assert_eq!(automation.target, "create automation: weekly review");

        let automation_update = registry
            .permission_request_for(
                "automation_update",
                &ToolInput::new().with_arg("automation_id", "automation_1"),
                &ExecutionPolicy {
                    allowed_tools: vec!["automation_update".to_string()],
                    ..deny_writes_policy()
                },
            )
            .unwrap();
        assert_eq!(automation_update.kind, "write");
        assert_eq!(automation_update.target, "update automation automation_1");

        let agent = registry
            .permission_request_for(
                "agent_spawn",
                &ToolInput::new().with_arg("prompt", "inspect the cache"),
                &ExecutionPolicy {
                    allowed_tools: vec!["agent_spawn".to_string()],
                    ..deny_writes_policy()
                },
            )
            .unwrap();
        assert_eq!(agent.kind, "write");
        assert_eq!(agent.target, "spawn sub-agent: inspect the cache");

        let gate_policy = ExecutionPolicy {
            allowed_tools: vec!["task_gate_run".to_string()],
            ..deny_writes_policy()
        };
        let gate = registry
            .permission_request_for(
                "task_gate_run",
                &ToolInput::new()
                    .with_arg("gate", "test")
                    .with_arg("command", "cargo test"),
                &gate_policy,
            )
            .unwrap();
        assert_eq!(gate.kind, "shell");
        assert_eq!(gate.target, "cargo test");

        let mcp_policy = ExecutionPolicy {
            allowed_tools: vec!["mcp_call".to_string(), "mcp_get_prompt".to_string()],
            ..deny_writes_policy()
        };
        let mcp = registry
            .permission_request_for(
                "mcp_call",
                &ToolInput::new()
                    .with_arg("server", "fake")
                    .with_arg("tool", "echo"),
                &mcp_policy,
            )
            .unwrap();
        assert_eq!(mcp.kind, "mcp");
        assert_eq!(mcp.target, "fake/echo");
        assert!(registry
            .permission_request_for(
                "mcp_get_prompt",
                &ToolInput::new()
                    .with_arg("server", "fake")
                    .with_arg("prompt", "review_pr"),
                &mcp_policy,
            )
            .is_none());

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn describe_mcp_arguments_handles_wrapped_and_direct_args() {
        let wrapped = ToolInput::new().with_arg("arguments", r#"{"path":"README.md"}"#);
        assert_eq!(describe_mcp_arguments(&wrapped), r#"{"path":"README.md"}"#);

        let direct = ToolInput::new()
            .with_arg("server", "fake")
            .with_arg("tool", "echo")
            .with_arg("text", "hello");
        assert_eq!(describe_mcp_arguments(&direct), "text=hello");
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
            network: NetworkConfig::default(),
            auto_approve_writes: false,
            auto_approve_shell: false,
            auto_approve_mcp: false,
            auto_approve_network: false,
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
            network: NetworkConfig::default(),
            auto_approve_writes: false,
            auto_approve_shell: false,
            auto_approve_mcp: false,
            auto_approve_network: false,
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
        assert!(root
            .names_for_policy(&ExecutionPolicy::new(&approval, None))
            .contains(&"dispatch_subagents"));
        assert!(root
            .names_for_policy(&ExecutionPolicy::new(&approval, None))
            .contains(&"rlm"));
        assert!(root
            .names_for_policy(&ExecutionPolicy::new(&approval, None))
            .contains(&"rlm_query"));
        assert!(root
            .names_for_policy(&ExecutionPolicy::new(&approval, None))
            .contains(&"llm_query"));
        assert!(root
            .names_for_policy(&ExecutionPolicy::new(&approval, None))
            .contains(&"rlm_process"));
        assert!(root
            .names_for_policy(&ExecutionPolicy::new(&approval, None))
            .contains(&"rlm_chunk_plan"));
        assert!(root
            .names_for_policy(&ExecutionPolicy::new(&approval, None))
            .contains(&"rlm_map_reduce_plan"));
        assert!(root
            .names_for_policy(&ExecutionPolicy::new(&approval, None))
            .contains(&"rlm_recursive_plan"));
        assert!(root
            .names_for_policy(&ExecutionPolicy::new(&approval, None))
            .contains(&"rlm_python"));
        assert!(root
            .names_for_policy(&ExecutionPolicy::new(&approval, None))
            .contains(&"rlm_python_session"));
        assert!(root
            .names_for_policy(&ExecutionPolicy::new(&approval, None))
            .contains(&"rlm_python_sessions"));
        assert!(root
            .names_for_policy(&ExecutionPolicy::new(&approval, None))
            .contains(&"rlm_process_sessions"));
        assert!(root
            .names_for_policy(&ExecutionPolicy::new(&approval, None))
            .contains(&"rlm_process_events"));
        assert!(root
            .names_for_policy(&ExecutionPolicy::new(&approval, None))
            .contains(&"rlm_process_wait"));
        assert!(root
            .names_for_policy(&ExecutionPolicy::new(&approval, None))
            .contains(&"rlm_process_cancel"));
        assert!(root
            .names_for_policy(&ExecutionPolicy::new(&approval, None))
            .contains(&"rlm_process_run_next"));
        assert!(root
            .names_for_policy(&ExecutionPolicy::new(&approval, None))
            .contains(&"rlm_process_drain"));
        assert!(root
            .names_for_policy(&ExecutionPolicy::new(&approval, None))
            .contains(&"rlm_batch"));
        assert!(root
            .names_for_policy(&ExecutionPolicy::new(&approval, None))
            .contains(&"rlm_query_batched"));
        assert!(root
            .names_for_policy(&ExecutionPolicy::new(&approval, None))
            .contains(&"llm_query_batched"));

        let nested = default_registry_with_context(
            AppConfig::default(),
            1,
            Rc::new(RefCell::new(TodoList::default())),
        );
        assert!(nested
            .names_for_policy(&ExecutionPolicy::new(&approval, None))
            .contains(&"dispatch_subagent"));
        assert!(nested
            .names_for_policy(&ExecutionPolicy::new(&approval, None))
            .contains(&"dispatch_subagents"));
        assert!(nested
            .names_for_policy(&ExecutionPolicy::new(&approval, None))
            .contains(&"rlm"));
        assert!(nested
            .names_for_policy(&ExecutionPolicy::new(&approval, None))
            .contains(&"rlm_query"));
        assert!(nested
            .names_for_policy(&ExecutionPolicy::new(&approval, None))
            .contains(&"llm_query"));
        assert!(nested
            .names_for_policy(&ExecutionPolicy::new(&approval, None))
            .contains(&"rlm_process"));
        assert!(nested
            .names_for_policy(&ExecutionPolicy::new(&approval, None))
            .contains(&"rlm_chunk_plan"));
        assert!(nested
            .names_for_policy(&ExecutionPolicy::new(&approval, None))
            .contains(&"rlm_map_reduce_plan"));
        assert!(nested
            .names_for_policy(&ExecutionPolicy::new(&approval, None))
            .contains(&"rlm_recursive_plan"));
        assert!(nested
            .names_for_policy(&ExecutionPolicy::new(&approval, None))
            .contains(&"rlm_python"));
        assert!(nested
            .names_for_policy(&ExecutionPolicy::new(&approval, None))
            .contains(&"rlm_python_session"));
        assert!(nested
            .names_for_policy(&ExecutionPolicy::new(&approval, None))
            .contains(&"rlm_python_sessions"));
        assert!(nested
            .names_for_policy(&ExecutionPolicy::new(&approval, None))
            .contains(&"rlm_process_sessions"));
        assert!(nested
            .names_for_policy(&ExecutionPolicy::new(&approval, None))
            .contains(&"rlm_process_events"));
        assert!(nested
            .names_for_policy(&ExecutionPolicy::new(&approval, None))
            .contains(&"rlm_process_wait"));
        assert!(nested
            .names_for_policy(&ExecutionPolicy::new(&approval, None))
            .contains(&"rlm_process_cancel"));
        assert!(nested
            .names_for_policy(&ExecutionPolicy::new(&approval, None))
            .contains(&"rlm_process_run_next"));
        assert!(nested
            .names_for_policy(&ExecutionPolicy::new(&approval, None))
            .contains(&"rlm_process_drain"));
        assert!(nested
            .names_for_policy(&ExecutionPolicy::new(&approval, None))
            .contains(&"rlm_batch"));
        assert!(nested
            .names_for_policy(&ExecutionPolicy::new(&approval, None))
            .contains(&"rlm_query_batched"));
        assert!(nested
            .names_for_policy(&ExecutionPolicy::new(&approval, None))
            .contains(&"llm_query_batched"));

        let at_limit = default_registry_with_context(
            AppConfig::default(),
            MAX_SUBAGENT_DEPTH,
            Rc::new(RefCell::new(TodoList::default())),
        );
        assert!(!at_limit
            .names_for_policy(&ExecutionPolicy::new(&approval, None))
            .contains(&"dispatch_subagent"));
        assert!(!at_limit
            .names_for_policy(&ExecutionPolicy::new(&approval, None))
            .contains(&"dispatch_subagents"));
        assert!(!at_limit
            .names_for_policy(&ExecutionPolicy::new(&approval, None))
            .contains(&"rlm"));
        assert!(!at_limit
            .names_for_policy(&ExecutionPolicy::new(&approval, None))
            .contains(&"rlm_query"));
        assert!(!at_limit
            .names_for_policy(&ExecutionPolicy::new(&approval, None))
            .contains(&"llm_query"));
        assert!(!at_limit
            .names_for_policy(&ExecutionPolicy::new(&approval, None))
            .contains(&"rlm_process"));
        assert!(!at_limit
            .names_for_policy(&ExecutionPolicy::new(&approval, None))
            .contains(&"rlm_chunk_plan"));
        assert!(!at_limit
            .names_for_policy(&ExecutionPolicy::new(&approval, None))
            .contains(&"rlm_map_reduce_plan"));
        assert!(!at_limit
            .names_for_policy(&ExecutionPolicy::new(&approval, None))
            .contains(&"rlm_recursive_plan"));
        assert!(!at_limit
            .names_for_policy(&ExecutionPolicy::new(&approval, None))
            .contains(&"rlm_python"));
        assert!(!at_limit
            .names_for_policy(&ExecutionPolicy::new(&approval, None))
            .contains(&"rlm_python_session"));
        assert!(!at_limit
            .names_for_policy(&ExecutionPolicy::new(&approval, None))
            .contains(&"rlm_python_sessions"));
        assert!(!at_limit
            .names_for_policy(&ExecutionPolicy::new(&approval, None))
            .contains(&"rlm_process_sessions"));
        assert!(!at_limit
            .names_for_policy(&ExecutionPolicy::new(&approval, None))
            .contains(&"rlm_process_events"));
        assert!(!at_limit
            .names_for_policy(&ExecutionPolicy::new(&approval, None))
            .contains(&"rlm_batch"));
        assert!(!at_limit
            .names_for_policy(&ExecutionPolicy::new(&approval, None))
            .contains(&"rlm_query_batched"));
        assert!(!at_limit
            .names_for_policy(&ExecutionPolicy::new(&approval, None))
            .contains(&"llm_query_batched"));
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
        assert!(names.contains(&"mcp_list_prompts"));
        assert!(names.contains(&"mcp_get_prompt"));
        assert!(names.contains(&"mcp_list_resources"));
        assert!(names.contains(&"mcp_read_resource"));
        assert!(names.contains(&"mcp_list_resource_templates"));

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn default_registry_exposes_dynamic_mcp_tools_when_enabled() {
        let root = temp_root("mcp-dynamic");
        let config = fake_mcp_server_config(&root, true);
        let registry =
            default_registry_with_context(config, 0, Rc::new(RefCell::new(TodoList::default())));
        let approval = ApprovalConfig::default();
        let names = registry.names_for_policy(&ExecutionPolicy::new(&approval, None));

        assert!(names.contains(&"mcp_list_tools"));
        assert!(names.contains(&"mcp_call"));
        assert!(names.contains(&"mcp__fake__echo"));

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn execute_with_policy_blocks_non_allowlisted_dynamic_mcp_tool() {
        let root = temp_root("mcp-dynamic-policy");
        let config = fake_mcp_server_config(&root, true);
        let registry =
            default_registry_with_context(config, 0, Rc::new(RefCell::new(TodoList::default())));
        let policy = ExecutionPolicy {
            allowed_tools: vec!["mcp__fake__echo".to_string()],
            require_write_confirmation: false,
            require_shell_confirmation: false,
            require_mcp_confirmation: false,
            shell_allowlist: Vec::new(),
            mcp_call_allowlist: vec!["other/*".to_string()],
            network: NetworkConfig::default(),
            auto_approve_writes: false,
            auto_approve_shell: false,
            auto_approve_mcp: false,
            auto_approve_network: false,
        };

        let error = registry
            .execute_with_policy(
                "mcp__fake__echo",
                ToolInput::new().with_arg("arguments", r#"{"text":"hello"}"#),
                &policy,
            )
            .unwrap_err();
        assert_eq!(classify(error.as_ref()), AppErrorKind::PolicyDenied);
        assert!(error.to_string().contains("policy allowlist"));

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
        assert!(!names.contains(&"mcp_list_prompts"));
        assert!(!names.contains(&"mcp_get_prompt"));
        assert!(!names.contains(&"mcp_list_resources"));
        assert!(!names.contains(&"mcp_read_resource"));
        assert!(!names.contains(&"mcp_list_resource_templates"));
    }
}
