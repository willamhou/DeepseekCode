use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use crate::config::types::AppConfig;
use crate::core::context::TaskContext;
use crate::core::loop_runtime::{
    AgentLoop, AgentLoopOptions, RunResult, SharedAgentCancelCheck, ToolEvent,
};
use crate::core::todos::TodoList;
use crate::error::{tool_failure, AppResult};
use crate::tools::types::{Tool, ToolInput, ToolOutput};

const DEFAULT_SUBAGENT_STEPS: usize = 4;
const MAX_SUBAGENT_STEPS: usize = 12;
const MAX_PARALLEL_SUBAGENTS: usize = 4;

pub struct DispatchSubagentTool {
    pub config: AppConfig,
    pub parent_depth: usize,
}

impl Tool for DispatchSubagentTool {
    fn name(&self) -> &str {
        "dispatch_subagent"
    }

    fn execute(&self, input: ToolInput) -> AppResult<ToolOutput> {
        let request = subagent_request_from_input(&input, "dispatch_subagent")?;
        let summary = run_subagent_request(&self.config, self.parent_depth, &request, None)?;

        Ok(ToolOutput { summary })
    }
}

impl DispatchSubagentTool {
    pub fn execute_with_agent_cancel(
        &self,
        input: ToolInput,
        cancel_check: Option<SharedAgentCancelCheck>,
    ) -> AppResult<ToolOutput> {
        let request = subagent_request_from_input(&input, "dispatch_subagent")?;
        let summary =
            run_subagent_request(&self.config, self.parent_depth, &request, cancel_check)?;
        Ok(ToolOutput { summary })
    }
}

pub struct DispatchSubagentsTool {
    pub config: AppConfig,
    pub parent_depth: usize,
}

impl Tool for DispatchSubagentsTool {
    fn name(&self) -> &str {
        "dispatch_subagents"
    }

    fn execute(&self, input: ToolInput) -> AppResult<ToolOutput> {
        let requests = parse_parallel_requests(input.get("tasks"))?;
        let mut handles = Vec::with_capacity(requests.len());
        for (index, request) in requests.into_iter().enumerate() {
            let config = self.config.clone();
            let parent_depth = self.parent_depth;
            handles.push(std::thread::spawn(move || {
                let thread_id = new_thread_id(index + 1);
                let summary = run_subagent_request(&config, parent_depth, &request, None)
                    .unwrap_or_else(|error| {
                        render_blocked_parallel_child(&request, &error.to_string())
                    });
                let artifact = persist_agent_thread(
                    &config.workspace.config_dir,
                    &thread_id,
                    &request,
                    &summary,
                )
                .ok();
                ParallelChildSummary {
                    thread_id,
                    request,
                    summary,
                    artifact,
                }
            }));
        }

        let mut children = Vec::with_capacity(handles.len());
        for handle in handles {
            children.push(
                handle
                    .join()
                    .map_err(|_| tool_failure("dispatch_subagents child thread panicked"))?,
            );
        }

        Ok(ToolOutput {
            summary: render_parallel_summary(&children),
        })
    }
}

#[derive(Debug, Clone)]
struct SubagentRequest {
    task: String,
    skill: Option<String>,
    agent_name: Option<String>,
    steps: usize,
}

#[derive(Debug, Clone)]
struct ParallelChildSummary {
    thread_id: String,
    request: SubagentRequest,
    summary: String,
    artifact: Option<PathBuf>,
}

fn subagent_request_from_input(input: &ToolInput, tool_name: &str) -> AppResult<SubagentRequest> {
    let task = input
        .get("task")
        .map(str::trim)
        .filter(|task| !task.is_empty())
        .ok_or_else(|| tool_failure(format!("{tool_name} requires a non-empty `task`")))?
        .to_string();
    let skill = input
        .get("skill")
        .map(str::trim)
        .filter(|skill| !skill.is_empty())
        .map(str::to_string);
    let agent_name = input
        .get("agent")
        .map(str::trim)
        .filter(|agent| !agent.is_empty())
        .map(str::to_string);
    let steps = parse_steps(input.get("steps"))?;
    Ok(SubagentRequest {
        task,
        skill,
        agent_name,
        steps,
    })
}

fn run_subagent_request(
    config: &AppConfig,
    parent_depth: usize,
    request: &SubagentRequest,
    cancel_check: Option<SharedAgentCancelCheck>,
) -> AppResult<String> {
    let agent = request
        .agent_name
        .as_deref()
        .map(|name| crate::core::agents::find_agent(&config.workspace.config_dir, name))
        .transpose()
        .map_err(|error| {
            tool_failure(format!(
                "subagent agent `{}` could not be loaded: {}",
                request.agent_name.as_deref().unwrap_or("unknown"),
                error.message
            ))
        })?;
    let child_task = match agent.as_ref() {
        Some(agent) => render_agent_task(agent, &request.task),
        None => request.task.clone(),
    };
    let hooks = crate::core::hooks::HookRunner::new(&config.hooks);
    let hook_context = hooks
        .subagent_start(&request.task, &child_task, request.agent_name.as_deref())
        .map_err(|error| tool_failure(format!("subagent_start hook failed: {error}")))?;
    let child_task = match hook_context {
        Some(context) => format!("{child_task}\n\nSubagent start hook context:\n{context}"),
        None => child_task,
    };

    let result = AgentLoop::new(config.clone())
        .run_with(
            TaskContext::new(child_task, request.skill.clone()),
            AgentLoopOptions {
                steps: request.steps,
                initial_observations: Vec::new(),
                initial_recent_steps: Vec::new(),
                todos: Rc::new(RefCell::new(TodoList::default())),
                subagent_depth: parent_depth + 1,
                emit_progress: false,
                persist_session: false,
                stream_events: None,
                run_events: None,
                approval_resolver: None,
                user_input_resolver: None,
                cancel_check,
            },
        )
        .map_err(|error| tool_failure(format!("subagent failed: {error}")))?;

    let summary = render_summary(
        &request.task,
        request.skill.as_deref(),
        agent.as_ref(),
        request.steps,
        &result,
    );
    let _ = hooks
        .subagent_stop(
            &request.task,
            &request.task,
            request.agent_name.as_deref(),
            &summary,
        )
        .map_err(|error| tool_failure(format!("subagent_stop hook failed: {error}")))?;
    Ok(summary)
}

fn parse_parallel_requests(raw: Option<&str>) -> AppResult<Vec<SubagentRequest>> {
    let raw = raw
        .map(str::trim)
        .filter(|raw| !raw.is_empty())
        .ok_or_else(|| tool_failure("dispatch_subagents requires `tasks` JSON array"))?;
    let value = crate::util::json::parse_json_value(raw)
        .map_err(|error| tool_failure(format!("dispatch_subagents tasks must be JSON: {error}")))?;
    let array = match value {
        crate::util::json::JsonValue::Array(array) => array,
        _ => {
            return Err(tool_failure(
                "dispatch_subagents `tasks` must be a JSON array",
            ))
        }
    };
    if array.is_empty() {
        return Err(tool_failure(
            "dispatch_subagents requires at least one task",
        ));
    }
    if array.len() > MAX_PARALLEL_SUBAGENTS {
        return Err(tool_failure(format!(
            "dispatch_subagents accepts at most {MAX_PARALLEL_SUBAGENTS} tasks"
        )));
    }

    let mut requests = Vec::with_capacity(array.len());
    for (index, item) in array.iter().enumerate() {
        let object = match item {
            crate::util::json::JsonValue::Object(object) => object,
            _ => {
                return Err(tool_failure(format!(
                    "dispatch_subagents task {} must be an object",
                    index + 1
                )));
            }
        };
        let task = json_string_field(object, "task")
            .map(str::trim)
            .filter(|task| !task.is_empty())
            .ok_or_else(|| {
                tool_failure(format!(
                    "dispatch_subagents task {} requires non-empty `task`",
                    index + 1
                ))
            })?
            .to_string();
        let skill = json_string_field(object, "skill")
            .map(str::trim)
            .filter(|skill| !skill.is_empty())
            .map(str::to_string);
        let agent_name = json_string_field(object, "agent")
            .map(str::trim)
            .filter(|agent| !agent.is_empty())
            .map(str::to_string);
        let steps = match json_string_field(object, "steps") {
            Some(value) => parse_steps(Some(value))?,
            None => DEFAULT_SUBAGENT_STEPS,
        };
        requests.push(SubagentRequest {
            task,
            skill,
            agent_name,
            steps,
        });
    }
    Ok(requests)
}

fn json_string_field<'a>(
    object: &'a std::collections::BTreeMap<String, crate::util::json::JsonValue>,
    key: &str,
) -> Option<&'a str> {
    match object.get(key) {
        Some(crate::util::json::JsonValue::String(value)) => Some(value),
        _ => None,
    }
}

fn parse_steps(raw: Option<&str>) -> AppResult<usize> {
    let Some(raw) = raw else {
        return Ok(DEFAULT_SUBAGENT_STEPS);
    };
    let steps = raw
        .trim()
        .parse::<usize>()
        .map_err(|_| tool_failure("dispatch_subagent `steps` must be a positive integer"))?;
    if steps == 0 {
        return Err(tool_failure("dispatch_subagent `steps` must be at least 1"));
    }
    if steps > MAX_SUBAGENT_STEPS {
        return Err(tool_failure(format!(
            "dispatch_subagent `steps` exceeds max {MAX_SUBAGENT_STEPS}"
        )));
    }
    Ok(steps)
}

fn render_parallel_summary(children: &[ParallelChildSummary]) -> String {
    let mut out = String::new();
    out.push_str(&format!("meta.parallel_children={}\n", children.len()));
    for (index, child) in children.iter().enumerate() {
        let ordinal = index + 1;
        out.push_str(&format!(
            "meta.parallel_child_{ordinal}_thread={}\n",
            child.thread_id
        ));
        out.push_str(&format!(
            "meta.parallel_child_{ordinal}_task={}\n",
            sanitize_meta_value(&child.request.task)
        ));
        if let Some(outcome) = meta_value(&child.summary, "meta.child_outcome") {
            out.push_str(&format!(
                "meta.parallel_child_{ordinal}_outcome={outcome}\n"
            ));
        }
        if let Some(next_action) = meta_value(&child.summary, "meta.child_next_action") {
            out.push_str(&format!(
                "meta.parallel_child_{ordinal}_next_action={next_action}\n"
            ));
        }
        if let Some(path) = child.artifact.as_ref() {
            out.push_str(&format!(
                "meta.parallel_child_{ordinal}_artifact={}\n",
                sanitize_meta_value(&path.display().to_string())
            ));
        }
    }

    out.push_str(&format!(
        "parallel subagents completed: {} child thread(s)\n",
        children.len()
    ));
    for child in children {
        out.push_str(&format!(
            "\n[{}] task: {}\n{}\n",
            child.thread_id, child.request.task, child.summary
        ));
    }
    out
}

fn render_blocked_parallel_child(request: &SubagentRequest, error: &str) -> String {
    format!(
        "meta.child_task={}\nmeta.child_budget={}\nmeta.child_outcome=blocked\nmeta.child_next_action=replan_parent\nmeta.child_final_message={}\nsubagent failed task `{}`: {}",
        sanitize_meta_value(&request.task),
        request.steps,
        sanitize_meta_value(error),
        request.task,
        error
    )
}

fn meta_value(summary: &str, key: &str) -> Option<String> {
    let prefix = format!("{key}=");
    summary.lines().find_map(|line| {
        line.strip_prefix(&prefix)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    })
}

fn new_thread_id(index: usize) -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    format!("thread-{nanos}-{index}")
}

pub fn agent_threads_dir(config_dir: &str) -> PathBuf {
    PathBuf::from(config_dir).join("agent-threads")
}

fn persist_agent_thread(
    config_dir: &str,
    thread_id: &str,
    request: &SubagentRequest,
    summary: &str,
) -> std::io::Result<PathBuf> {
    let dir = agent_threads_dir(config_dir);
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{thread_id}.md"));
    std::fs::write(&path, render_agent_thread_file(thread_id, request, summary))?;
    Ok(path)
}

fn render_agent_thread_file(thread_id: &str, request: &SubagentRequest, summary: &str) -> String {
    format!(
        "# Agent Thread {thread_id}\n\nTask: {}\nAgent: {}\nSkill: {}\nSteps: {}\n\n## Summary\n\n{}\n",
        request.task,
        request.agent_name.as_deref().unwrap_or("-"),
        request.skill.as_deref().unwrap_or("-"),
        request.steps,
        summary.trim()
    )
}

pub fn active_agent_thread_path(config_dir: &str) -> PathBuf {
    agent_threads_dir(config_dir).join("active")
}

pub fn validate_thread_id(id: &str) -> bool {
    !id.is_empty()
        && !id.starts_with('.')
        && !id.contains("..")
        && id
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_'))
}

pub fn thread_file_path(config_dir: &str, id: &str) -> Option<PathBuf> {
    validate_thread_id(id).then(|| agent_threads_dir(config_dir).join(format!("{id}.md")))
}

fn render_agent_task(agent: &crate::core::agents::AgentSpec, task: &str) -> String {
    let tools = if agent.tools.is_empty() {
        "all available child tools".to_string()
    } else {
        agent.tools.join(", ")
    };
    format!(
        "Act as custom subagent `{}`.\nDescription: {}\nTool access hint: {}\n\nSubagent instructions:\n{}\n\nDelegated task:\n{}",
        agent.name, agent.description, tools, agent.prompt, task
    )
}

fn render_summary(
    task: &str,
    skill: Option<&str>,
    agent: Option<&crate::core::agents::AgentSpec>,
    steps: usize,
    result: &RunResult,
) -> String {
    let tool_calls = if result.tool_events.is_empty() {
        "none".to_string()
    } else {
        result
            .tool_events
            .iter()
            .map(|event| event.tool_name.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    };
    let failed_tool_calls = result
        .tool_events
        .iter()
        .filter(|event| {
            matches!(
                event.status,
                crate::model::protocol::ObservationStatus::Failed
            )
        })
        .count();
    let final_message = first_non_empty_line(&result.final_message)
        .unwrap_or("subagent finished without a final message");
    let outcome = if failed_tool_calls > 0 || message_looks_blocked(final_message) {
        "blocked"
    } else {
        "ok"
    };
    let child_files = extract_child_files(result);
    let child_next_action = child_next_action(outcome, &child_files, final_message);
    let mut summary = String::new();
    summary.push_str(&format!("meta.child_task={}\n", sanitize_meta_value(task)));
    if let Some(skill) = skill {
        summary.push_str(&format!(
            "meta.child_skill={}\n",
            sanitize_meta_value(skill)
        ));
    }
    if let Some(agent) = agent {
        summary.push_str(&format!(
            "meta.child_agent={}\n",
            sanitize_meta_value(&agent.name)
        ));
    }
    summary.push_str(&format!("meta.child_budget={steps}\n"));
    summary.push_str(&format!(
        "meta.child_tool_calls={}\n",
        sanitize_meta_value(&tool_calls)
    ));
    summary.push_str(&format!(
        "meta.child_failed_tool_calls={failed_tool_calls}\n"
    ));
    summary.push_str(&format!("meta.child_outcome={outcome}\n"));
    summary.push_str(&format!(
        "meta.child_next_action={}\n",
        sanitize_meta_value(&child_next_action)
    ));
    if !child_files.is_empty() {
        summary.push_str(&format!(
            "meta.child_files={}\n",
            sanitize_meta_value(&child_files.join(","))
        ));
    }
    summary.push_str(&format!(
        "meta.child_final_message={}\n",
        sanitize_meta_value(final_message)
    ));

    match skill {
        Some(skill) => summary.push_str(&format!(
            "subagent finished task `{task}` with skill `{skill}` (budget {steps} steps)\nchild tool calls: {tool_calls}\nchild failed tool calls: {failed_tool_calls}\nchild outcome: {outcome}\nchild next action: {child_next_action}\nchild files: {}\nchild final message: {final_message}",
            if child_files.is_empty() {
                "none".to_string()
            } else {
                child_files.join(", ")
            }
        )),
        None => summary.push_str(&format!(
            "subagent finished task `{task}` (budget {steps} steps)\nchild tool calls: {tool_calls}\nchild failed tool calls: {failed_tool_calls}\nchild outcome: {outcome}\nchild next action: {child_next_action}\nchild files: {}\nchild final message: {final_message}",
            if child_files.is_empty() {
                "none".to_string()
            } else {
                child_files.join(", ")
            }
        )),
    }

    summary
}

fn first_non_empty_line(text: &str) -> Option<&str> {
    text.lines().map(str::trim).find(|line| !line.is_empty())
}

fn message_looks_blocked(message: &str) -> bool {
    let lower = message.to_lowercase();
    ["blocked", "unable", "could not", "stuck", "no matches"]
        .iter()
        .any(|marker| lower.contains(marker))
}

fn sanitize_meta_value(value: &str) -> String {
    value.replace('\n', " ").trim().to_string()
}

fn extract_child_files(result: &RunResult) -> Vec<String> {
    let mut files = Vec::new();
    for event in &result.tool_events {
        match event.tool_name.as_str() {
            "read_file" => {
                if let Some(path) = event.input.get("path") {
                    push_unique(&mut files, path);
                }
            }
            "apply_patch" => {
                collect_apply_patch_paths(event, &mut files);
            }
            "git_diff" => {
                collect_diff_paths(&event.output, &mut files);
            }
            "search_text" => {
                for line in event.output.lines() {
                    if let Some(path) = line.splitn(3, ':').next().map(str::trim) {
                        if !path.is_empty() && !path.starts_with("No matches for `") {
                            push_unique(&mut files, path);
                        }
                    }
                    if files.len() >= 4 {
                        break;
                    }
                }
            }
            "list_files" => {
                for line in event.output.lines().map(str::trim) {
                    if !line.is_empty() && !line.ends_with('/') {
                        push_unique(&mut files, line);
                    }
                    if files.len() >= 4 {
                        break;
                    }
                }
            }
            _ => {}
        }
        if files.len() >= 4 {
            break;
        }
    }
    files
}

fn collect_apply_patch_paths(event: &ToolEvent, files: &mut Vec<String>) {
    if let Some(path) = event.input.get("path") {
        push_unique(files, path);
    }

    for line in event.output.lines().map(str::trim) {
        if let Some(rest) = line.strip_prefix("Updated ") {
            if let Some(path) = rest.split(" using ").next().map(str::trim) {
                if !path.is_empty() {
                    push_unique(files, path);
                }
            }
            continue;
        }

        if let Some(path) = line
            .strip_prefix("- ")
            .or_else(|| line.strip_prefix("+ "))
            .map(str::trim)
            .filter(|path| !path.is_empty())
        {
            push_unique(files, path);
            continue;
        }

        if let Some((old, new)) = line.split_once(" -> ") {
            push_unique(files, old.trim());
            push_unique(files, new.trim());
        }
    }
}

fn collect_diff_paths(output: &str, files: &mut Vec<String>) {
    for line in output.lines().map(str::trim) {
        if let Some(rest) = line.strip_prefix("diff --git ") {
            let mut parts = rest.split_whitespace();
            let _old = parts.next();
            if let Some(new) = parts.next().and_then(normalize_diff_path) {
                push_unique(files, &new);
            }
            continue;
        }

        if let Some(path) = line
            .strip_prefix("+++ ")
            .and_then(|value| value.split_whitespace().next())
            .and_then(normalize_diff_path)
        {
            push_unique(files, &path);
        }
    }
}

fn normalize_diff_path(raw: &str) -> Option<String> {
    if raw == "/dev/null" {
        return None;
    }
    Some(
        raw.strip_prefix("a/")
            .or_else(|| raw.strip_prefix("b/"))
            .unwrap_or(raw)
            .to_string(),
    )
}

fn child_next_action(outcome: &str, child_files: &[String], final_message: &str) -> String {
    if outcome == "blocked" {
        return "replan_parent".to_string();
    }
    if let Some(path) = child_files.first() {
        return format!("read_file:{path}");
    }
    if let Some(query) = first_backtick_segment(final_message) {
        return format!("search_text:{query}");
    }
    "continue_parent".to_string()
}

fn first_backtick_segment(text: &str) -> Option<String> {
    let start = text.find('`')?;
    let rest = &text[start + 1..];
    let end = rest.find('`')?;
    let value = rest[..end].trim();
    if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}

fn push_unique(files: &mut Vec<String>, candidate: &str) {
    if files.len() >= 4 {
        return;
    }
    if !files.iter().any(|existing| existing == candidate) {
        files.push(candidate.to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::protocol::{ObservationStatus, TokenUsage};

    #[test]
    fn parse_steps_uses_default_when_missing() {
        assert_eq!(parse_steps(None).unwrap(), DEFAULT_SUBAGENT_STEPS);
    }

    #[test]
    fn parse_steps_rejects_zero_and_large_values() {
        assert!(parse_steps(Some("0")).is_err());
        assert!(parse_steps(Some("99")).is_err());
    }

    #[test]
    fn render_summary_marks_blocked_when_child_failed() {
        let result = RunResult {
            final_message: "blocked on missing file".to_string(),
            tool_events: vec![ToolEvent {
                tool_name: "read_file".to_string(),
                input: std::collections::BTreeMap::from([(
                    "path".to_string(),
                    "src/lib.rs".to_string(),
                )]),
                output: "No such file".to_string(),
                status: ObservationStatus::Failed,
            }],
            usage: TokenUsage::default(),
        };
        let summary = render_summary("inspect file", None, None, 2, &result);
        assert!(summary.contains("meta.child_outcome=blocked"));
        assert!(summary.contains("meta.child_next_action=replan_parent"));
        assert!(summary.contains("meta.child_files=src/lib.rs"));
        assert!(summary.contains("child failed tool calls: 1"));
        assert!(summary.contains("child outcome: blocked"));
    }

    #[test]
    fn render_summary_emits_read_file_next_action_for_child_files() {
        let result = RunResult {
            final_message: "read src/main.rs next".to_string(),
            tool_events: vec![ToolEvent {
                tool_name: "read_file".to_string(),
                input: std::collections::BTreeMap::from([(
                    "path".to_string(),
                    "src/main.rs".to_string(),
                )]),
                output: "fn main() {}".to_string(),
                status: ObservationStatus::Ok,
            }],
            usage: TokenUsage::default(),
        };
        let summary = render_summary("inspect entrypoint", None, None, 2, &result);
        assert!(summary.contains("meta.child_next_action=read_file:src/main.rs"));
    }

    #[test]
    fn render_summary_emits_search_next_action_from_quoted_message() {
        let result = RunResult {
            final_message: "search for `route_benchmark_subcommand`".to_string(),
            tool_events: Vec::new(),
            usage: TokenUsage::default(),
        };
        let summary = render_summary("inspect symbol", None, None, 2, &result);
        assert!(summary.contains("meta.child_next_action=search_text:route_benchmark_subcommand"));
    }

    #[test]
    fn render_summary_includes_patched_files_for_parent_readback() {
        let result = RunResult {
            final_message: "patched the failing route".to_string(),
            tool_events: vec![
                ToolEvent {
                    tool_name: "apply_patch".to_string(),
                    input: std::collections::BTreeMap::from([(
                        "path".to_string(),
                        "src/lib.rs".to_string(),
                    )]),
                    output: "Updated src/lib.rs using single replacement mode.".to_string(),
                    status: ObservationStatus::Ok,
                },
                ToolEvent {
                    tool_name: "git_diff".to_string(),
                    input: std::collections::BTreeMap::new(),
                    output: "diff --git a/src/lib.rs b/src/lib.rs\n+++ b/src/lib.rs".to_string(),
                    status: ObservationStatus::Ok,
                },
            ],
            usage: TokenUsage::default(),
        };
        let summary = render_summary("fix route", None, None, 4, &result);
        assert!(summary.contains("meta.child_files=src/lib.rs"));
        assert!(summary.contains("meta.child_next_action=read_file:src/lib.rs"));
    }

    #[test]
    fn render_summary_includes_custom_agent_metadata() {
        let agent = crate::core::agents::AgentSpec {
            name: "reviewer".to_string(),
            description: "Reviews code".to_string(),
            tools: vec!["read_file".to_string()],
            model: None,
            prompt: "Review carefully.".to_string(),
            path: ".dscode/agents/reviewer.md".into(),
            source: crate::core::agents::AgentSource::Project,
        };
        let result = RunResult {
            final_message: "done".to_string(),
            tool_events: Vec::new(),
            usage: TokenUsage::default(),
        };

        let summary = render_summary("review code", None, Some(&agent), 2, &result);

        assert!(summary.contains("meta.child_agent=reviewer"));
    }

    #[test]
    fn render_agent_task_includes_custom_agent_prompt() {
        let agent = crate::core::agents::AgentSpec {
            name: "reviewer".to_string(),
            description: "Reviews code".to_string(),
            tools: vec!["read_file".to_string(), "search_text".to_string()],
            model: None,
            prompt: "Review carefully.".to_string(),
            path: ".dscode/agents/reviewer.md".into(),
            source: crate::core::agents::AgentSource::Project,
        };

        let task = render_agent_task(&agent, "Inspect src/lib.rs");

        assert!(task.contains("custom subagent `reviewer`"));
        assert!(task.contains("read_file, search_text"));
        assert!(task.contains("Review carefully."));
        assert!(task.contains("Inspect src/lib.rs"));
    }

    #[test]
    fn parse_parallel_requests_reads_json_array() {
        let requests = parse_parallel_requests(Some(
            r#"[{"task":"review src/a.rs","agent":"reviewer","steps":"3"},{"task":"inspect docs","skill":"doc"}]"#,
        ))
        .unwrap();

        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].task, "review src/a.rs");
        assert_eq!(requests[0].agent_name.as_deref(), Some("reviewer"));
        assert_eq!(requests[0].steps, 3);
        assert_eq!(requests[1].skill.as_deref(), Some("doc"));
        assert_eq!(requests[1].steps, DEFAULT_SUBAGENT_STEPS);
    }

    #[test]
    fn parse_parallel_requests_rejects_too_many_tasks() {
        let raw = r#"[
            {"task":"one"},
            {"task":"two"},
            {"task":"three"},
            {"task":"four"},
            {"task":"five"}
        ]"#;

        assert!(parse_parallel_requests(Some(raw)).is_err());
    }

    #[test]
    fn render_parallel_summary_includes_thread_metadata() {
        let child = ParallelChildSummary {
            thread_id: "thread-1".to_string(),
            request: SubagentRequest {
                task: "inspect src/lib.rs".to_string(),
                skill: None,
                agent_name: None,
                steps: 2,
            },
            summary: "meta.child_outcome=ok\nmeta.child_next_action=read_file:src/lib.rs\nchild final message".to_string(),
            artifact: Some(".dscode/agent-threads/thread-1.md".into()),
        };

        let summary = render_parallel_summary(&[child]);

        assert!(summary.contains("meta.parallel_children=1"));
        assert!(summary.contains("meta.parallel_child_1_thread=thread-1"));
        assert!(summary.contains("meta.parallel_child_1_outcome=ok"));
        assert!(summary.contains("meta.parallel_child_1_next_action=read_file:src/lib.rs"));
        assert!(summary.contains("[thread-1] task: inspect src/lib.rs"));
    }

    #[test]
    fn thread_id_validation_rejects_path_escape() {
        assert!(validate_thread_id("thread-123"));
        assert!(!validate_thread_id("../thread-123"));
        assert!(!validate_thread_id(".hidden"));
    }

    #[test]
    fn render_agent_thread_file_records_summary() {
        let request = SubagentRequest {
            task: "review".to_string(),
            skill: Some("security".to_string()),
            agent_name: Some("reviewer".to_string()),
            steps: 4,
        };

        let body = render_agent_thread_file("thread-1", &request, "summary");

        assert!(body.contains("# Agent Thread thread-1"));
        assert!(body.contains("Task: review"));
        assert!(body.contains("Agent: reviewer"));
        assert!(body.contains("Skill: security"));
        assert!(body.contains("summary"));
    }
}
