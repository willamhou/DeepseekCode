use std::cell::RefCell;
use std::rc::Rc;

use crate::config::types::AppConfig;
use crate::core::context::TaskContext;
use crate::core::loop_runtime::{AgentLoop, AgentLoopOptions, RunResult, ToolEvent};
use crate::core::todos::TodoList;
use crate::error::{tool_failure, AppResult};
use crate::tools::types::{Tool, ToolInput, ToolOutput};

const DEFAULT_SUBAGENT_STEPS: usize = 4;
const MAX_SUBAGENT_STEPS: usize = 12;

pub struct DispatchSubagentTool {
    pub config: AppConfig,
    pub parent_depth: usize,
}

impl Tool for DispatchSubagentTool {
    fn name(&self) -> &str {
        "dispatch_subagent"
    }

    fn execute(&self, input: ToolInput) -> AppResult<ToolOutput> {
        let task = input
            .get("task")
            .map(str::trim)
            .filter(|task| !task.is_empty())
            .ok_or_else(|| tool_failure("dispatch_subagent requires a non-empty `task`"))?
            .to_string();
        let skill = input
            .get("skill")
            .map(str::trim)
            .filter(|skill| !skill.is_empty())
            .map(str::to_string);
        let steps = parse_steps(input.get("steps"))?;

        let result = AgentLoop::new(self.config.clone())
            .run_with(
                TaskContext::new(task.clone(), skill.clone()),
                AgentLoopOptions {
                    steps,
                    initial_observations: Vec::new(),
                    todos: Rc::new(RefCell::new(TodoList::default())),
                    subagent_depth: self.parent_depth + 1,
                    emit_progress: false,
                    persist_session: false,
                },
            )
            .map_err(|error| tool_failure(format!("subagent failed: {error}")))?;

        Ok(ToolOutput {
            summary: render_summary(&task, skill.as_deref(), steps, &result),
        })
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

fn render_summary(task: &str, skill: Option<&str>, steps: usize, result: &RunResult) -> String {
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
        let summary = render_summary("inspect file", None, 2, &result);
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
        let summary = render_summary("inspect entrypoint", None, 2, &result);
        assert!(summary.contains("meta.child_next_action=read_file:src/main.rs"));
    }

    #[test]
    fn render_summary_emits_search_next_action_from_quoted_message() {
        let result = RunResult {
            final_message: "search for `route_benchmark_subcommand`".to_string(),
            tool_events: Vec::new(),
            usage: TokenUsage::default(),
        };
        let summary = render_summary("inspect symbol", None, 2, &result);
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
        let summary = render_summary("fix route", None, 4, &result);
        assert!(summary.contains("meta.child_files=src/lib.rs"));
        assert!(summary.contains("meta.child_next_action=read_file:src/lib.rs"));
    }
}
