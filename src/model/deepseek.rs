use std::collections::BTreeMap;
use std::env;
use std::io::BufRead;

use crate::config::types::ModelConfig;
use crate::error::app_error;
use crate::error::tool_failure;
use crate::error::AppResult;
use crate::model::client::ModelClient;
use crate::model::protocol::{ModelAction, ModelRequest, ModelResponse, TokenUsage};
use crate::tools::types::ToolInput;
use crate::ui::stream::StreamEvents;
use crate::util::json::{
    json_as_array, json_as_object, json_as_string, json_as_u64, json_escape, parse_root_object,
    JsonValue,
};
use crate::util::sse::read_frame;

pub struct DeepSeekClient {
    pub config: ModelConfig,
}

impl ModelClient for DeepSeekClient {
    fn respond(
        &self,
        input: ModelRequest,
        events: &mut dyn crate::ui::stream::StreamEvents,
    ) -> AppResult<(ModelResponse, Option<TokenUsage>)> {
        let api_key = env::var(&self.config.api_key_env)
            .ok()
            .filter(|key| !key.trim().is_empty());

        if let Some(api_key) = api_key {
            // Remote stream attempted: surface success or error directly.
            // Stream errors propagate so partial text isn't double-rendered
            // by the offline fallback (per StreamEvents "exactly once" contract).
            return self.respond_remote(&input, &api_key, events);
        }

        // No API key configured → run offline planner and drive events.
        let response = self.respond_offline(input);
        events.on_text_delta(&response.message);
        events.on_assistant_done(&response.message);
        if let ModelAction::CallTool { tool_name, input } = &response.action {
            events.on_tool_call(tool_name, &input.args);
        }
        Ok((response, None))
    }
}

impl DeepSeekClient {
    fn respond_remote(
        &self,
        input: &ModelRequest,
        api_key: &str,
        events: &mut dyn StreamEvents,
    ) -> AppResult<(ModelResponse, Option<TokenUsage>)> {
        match api_flavor(&self.config.base_url) {
            ApiFlavor::OpenAi => self.respond_remote_openai(input, api_key, events),
            ApiFlavor::Anthropic => self.respond_remote_anthropic(input, api_key, events),
        }
    }

    fn respond_remote_openai(
        &self,
        input: &ModelRequest,
        api_key: &str,
        events: &mut dyn StreamEvents,
    ) -> AppResult<(ModelResponse, Option<TokenUsage>)> {
        let endpoint = format!(
            "{}/chat/completions",
            self.config.base_url.trim_end_matches('/')
        );
        let system_prompt = build_openai_tool_system_prompt(&input.system_prompt);
        let user_prompt = build_user_prompt(input);
        let tools = build_openai_tools(&input.available_tools);
        let body = format!(
            concat!(
                "{{",
                "\"model\":\"{}\",",
                "\"temperature\":0,",
                "\"max_tokens\":1024,",
                "\"stream\":true,",
                "\"stream_options\":{{\"include_usage\":true}},",
                "\"tool_choice\":\"auto\",",
                "\"parallel_tool_calls\":false,",
                "\"tools\":{},",
                "\"messages\":[",
                "{{\"role\":\"system\",\"content\":\"{}\"}},",
                "{{\"role\":\"user\",\"content\":\"{}\"}}",
                "]",
                "}}"
            ),
            json_escape(&self.config.model),
            tools,
            json_escape(&system_prompt),
            json_escape(&user_prompt)
        );

        let auth = format!("Authorization: Bearer {api_key}");
        let args = [
            "-sS",
            "-N",
            "--max-time",
            "60",
            "-X",
            "POST",
            endpoint.as_str(),
            "-H",
            auth.as_str(),
            "-H",
            "Content-Type: application/json",
            "-H",
            "Accept: text/event-stream",
            "--data-binary",
            body.as_str(),
        ];

        let mut process = match crate::util::process::spawn_streaming("curl", &args) {
            Ok(p) => p,
            Err(error) => {
                events.on_assistant_done("");
                return Err(error);
            }
        };
        let parsed = parse_openai_stream(&mut process.stdout, events);
        let (status, stderr_tail) = process.finish()?;
        if !status.success() {
            return Err(tool_failure(format!(
                "deepseek openai stream failed (exit {:?}): {}",
                status.code(),
                stderr_tail.trim()
            )));
        }
        parsed
    }

    fn respond_remote_anthropic(
        &self,
        input: &ModelRequest,
        api_key: &str,
        events: &mut dyn StreamEvents,
    ) -> AppResult<(ModelResponse, Option<TokenUsage>)> {
        let endpoint = format!("{}/messages", self.config.base_url.trim_end_matches('/'));
        let system_prompt = build_anthropic_tool_system_prompt(&input.system_prompt);
        let user_prompt = build_user_prompt(input);
        let tools = build_anthropic_tools(&input.available_tools);
        let body = format!(
            concat!(
                "{{",
                "\"model\":\"{}\",",
                "\"max_tokens\":1024,",
                "\"stream\":true,",
                "\"tool_choice\":{{\"type\":\"auto\"}},",
                "\"tools\":{},",
                "\"system\":\"{}\",",
                "\"messages\":[",
                "{{\"role\":\"user\",\"content\":[{{\"type\":\"text\",\"text\":\"{}\"}}]}}",
                "]",
                "}}"
            ),
            json_escape(&self.config.model),
            tools,
            json_escape(&system_prompt),
            json_escape(&user_prompt)
        );

        let api_header = format!("x-api-key: {api_key}");
        let args = [
            "-sS",
            "-N",
            "--max-time",
            "60",
            "-X",
            "POST",
            endpoint.as_str(),
            "-H",
            api_header.as_str(),
            "-H",
            "anthropic-version: 2023-06-01",
            "-H",
            "Content-Type: application/json",
            "-H",
            "Accept: text/event-stream",
            "--data-binary",
            body.as_str(),
        ];

        let mut process = match crate::util::process::spawn_streaming("curl", &args) {
            Ok(p) => p,
            Err(error) => {
                events.on_assistant_done("");
                return Err(error);
            }
        };
        let parsed = parse_anthropic_stream(&mut process.stdout, events);
        let (status, stderr_tail) = process.finish()?;
        if !status.success() {
            return Err(tool_failure(format!(
                "deepseek anthropic stream failed (exit {:?}): {}",
                status.code(),
                stderr_tail.trim()
            )));
        }
        parsed
    }

    fn respond_offline(&self, input: ModelRequest) -> ModelResponse {
        let task = input.task.clone();
        let task_lower = task.to_lowercase();
        let mut used_tools: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
        let mut succeeded_tools: std::collections::BTreeSet<&str> =
            std::collections::BTreeSet::new();
        for observation in &input.observations {
            used_tools.insert(observation.tool_name.as_str());
            if !observation.is_failure() {
                succeeded_tools.insert(observation.tool_name.as_str());
            }
        }
        let available_tools: std::collections::BTreeSet<&str> =
            input.available_tools.iter().map(String::as_str).collect();
        let tool_available = |name: &str| available_tools.contains(name);
        let last_apply_patch = input
            .observations
            .iter()
            .rev()
            .find(|observation| observation.tool_name == "apply_patch");
        let last_apply_patch_was_patch_mode_failure = last_apply_patch
            .map(|observation| {
                observation.is_failure()
                    && (observation.summary.starts_with("patch dry-run failed")
                        || observation.summary.starts_with("patch apply failed"))
            })
            .unwrap_or(false);
        let search_query = derive_search_query(&task);
        let child_file_path = next_child_file_path(&input.observations);
        let child_search_query = child_followup_query(&input.observations);
        let edit_request = derive_edit_request(&task);
        let successful_apply_patch_count =
            successful_tool_call_count(&input.observations, "apply_patch");
        let git_diff_call_count = tool_call_count(&input.observations, "git_diff");
        let run_shell_call_count = tool_call_count(&input.observations, "run_shell");

        if input.planning_mode
            && input.todos.is_empty()
            && tool_available("todo_write")
            && !(task_looks_like_pr_workflow(&task_lower)
                && observations_include_pr_review_signal(&input.observations))
        {
            let plan_items = build_initial_todo_plan_json(&input);
            return ModelResponse {
                message: format!(
                    "{} planner is creating a concrete execution plan before acting.",
                    self.config.model
                ),
                action: ModelAction::CallTool {
                    tool_name: "todo_write".to_string(),
                    input: ToolInput::new().with_arg("items", plan_items),
                },
            };
        }

        if input.planning_mode
            && !input.todos.is_empty()
            && used_tools.contains("todo_write")
            && task_requests_plan_only(&task_lower)
        {
            return ModelResponse {
                message: format!(
                    "{} planner prepared the execution plan and is stopping before acting, as requested.",
                    self.config.model
                ),
                action: ModelAction::Finish,
            };
        }

        if let Some(replan_response) = build_replan_response(&self.config.model, &input) {
            return replan_response;
        }

        if let Some(recovery_response) = build_recovery_response(
            &self.config.model,
            &input,
            &used_tools,
            &succeeded_tools,
            search_query.as_deref(),
        ) {
            return recovery_response;
        }

        if task_requests_failure_repro(&task_lower)
            && edit_request.is_none()
            && used_tools.contains("run_shell")
            && used_tools.contains("read_file")
            && !used_tools.contains("apply_patch")
        {
            return ModelResponse {
                message: format!(
                    "{} offline planner reproduced the failure and inspected the relevant file, so it is stopping before speculative follow-up.",
                    self.config.model
                ),
                action: ModelAction::Finish,
            };
        }

        if let Some(test_command) = input.suggested_test_command.as_deref() {
            if tool_available("run_shell")
                && wants_validation(&task_lower)
                && task_requests_failure_repro(&task_lower)
                && !used_tools.contains("run_shell")
            {
                return ModelResponse {
                    message: format!(
                        "{} planner is reproducing the failing validation first with `{}`.",
                        self.config.model, test_command
                    ),
                    action: ModelAction::CallTool {
                        tool_name: "run_shell".to_string(),
                        input: ToolInput::new()
                            .with_arg("cwd", ".")
                            .with_arg("command", test_command),
                    },
                };
            }
        }

        if let Some(subagent_task) = derive_subagent_task(&input, &task_lower, &used_tools) {
            return ModelResponse {
                message: format!(
                    "{} planner is delegating an independent exploration step to a subagent before continuing the parent plan.",
                    self.config.model
                ),
                action: ModelAction::CallTool {
                    tool_name: "dispatch_subagent".to_string(),
                    input: ToolInput::new()
                        .with_arg("task", subagent_task)
                        .with_arg("steps", "2"),
                },
            };
        }

        if edit_request.is_none() {
            if task_looks_like_pr_workflow(&task_lower)
                && !used_tools.contains("read_file")
                && tool_available("read_file")
                && observations_include_pr_review_signal(&input.observations)
            {
                if let Some(path) =
                    preferred_read_path(&input.observations, input.primary_file.as_deref())
                {
                    return ModelResponse {
                        message: format!(
                            "{} planner is reading the most relevant changed file from the PR context `{path}`.",
                            self.config.model
                        ),
                        action: ModelAction::CallTool {
                            tool_name: "read_file".to_string(),
                            input: ToolInput::new()
                                .with_arg("path", path)
                                .with_arg("max_lines", "60"),
                        },
                    };
                }
            }
            if let Some(query) = search_query.as_deref() {
                if !used_tools.contains("search_text")
                    && tool_available("search_text")
                    && !(task_looks_like_pr_workflow(&task_lower)
                        && used_tools.contains("read_file"))
                    && (!task_looks_like_pr_workflow(&task_lower)
                        || !observations_include_pr_review_signal(&input.observations)
                        || query_looks_code_like(query))
                {
                    return ModelResponse {
                        message: format!(
                            "{} planner is searching for `{query}`.",
                            self.config.model
                        ),
                        action: ModelAction::CallTool {
                            tool_name: "search_text".to_string(),
                            input: ToolInput::new()
                                .with_arg("root", ".")
                                .with_arg("query", query)
                                .with_arg("limit", "20"),
                        },
                    };
                }
            }
            if let Some(path) =
                preferred_read_path(&input.observations, input.primary_file.as_deref())
            {
                if (!used_tools.contains("read_file")
                    || (child_file_path.is_some()
                        && task_allows_child_file_followup(&task_lower, &input.observations)))
                    && tool_available("read_file")
                    && observations_include_repo_signal(&input.observations)
                {
                    return ModelResponse {
                        message: format!(
                            "{} planner is reading the most relevant matched file `{path}`.",
                            self.config.model
                        ),
                        action: ModelAction::CallTool {
                            tool_name: "read_file".to_string(),
                            input: ToolInput::new()
                                .with_arg("path", path)
                                .with_arg("max_lines", "60"),
                        },
                    };
                }
            }
        }

        if let Some(edit_request) = edit_request.as_ref() {
            if !succeeded_tools.contains("apply_patch")
                && !used_tools.contains("apply_patch")
                && tool_available("apply_patch")
            {
                if let Some(plan) = crate::tools::apply_patch::build_single_line_diff(
                    &edit_request.path,
                    &edit_request.find,
                    &edit_request.replace,
                ) {
                    return ModelResponse {
                        message: format!(
                            "{} planner skipped inspection; applying a unified diff patch directly in {}.",
                            self.config.model, edit_request.path
                        ),
                        action: ModelAction::CallTool {
                            tool_name: "apply_patch".to_string(),
                            input: ToolInput::new()
                                .with_arg("cwd", plan.cwd)
                                .with_arg("patch", plan.patch),
                        },
                    };
                }
            }
        }

        if task_looks_like_pr_workflow(&task_lower)
            && edit_request.is_none()
            && used_tools.contains("read_file")
            && (observations_include_pr_or_ci_signal(&input.observations)
                || pr_workflow_child_followup_complete(&task_lower, &input.observations))
        {
            return ModelResponse {
                message: format!(
                    "{} offline planner reviewed the most relevant PR/CI file and is stopping to avoid speculative follow-up without stronger context.",
                    self.config.model
                ),
                action: ModelAction::Finish,
            };
        }

        if !used_tools.contains("list_files")
            && tool_available("list_files")
            && !succeeded_tools.contains("apply_patch")
        {
            return ModelResponse {
                message: format!(
                    "{} planner is exploring the repository layout first.",
                    self.config.model
                ),
                action: ModelAction::CallTool {
                    tool_name: "list_files".to_string(),
                    input: ToolInput::new()
                        .with_arg("root", ".")
                        .with_arg("max_depth", "2")
                        .with_arg("limit", "20"),
                },
            };
        }

        if edit_request.is_none()
            && search_query.is_none()
            && child_file_path.is_none()
            && !used_tools.contains("read_file")
        {
            if let Some(query) = child_search_query.as_deref() {
                if !used_tools.contains("search_text") && tool_available("search_text") {
                    return ModelResponse {
                        message: format!(
                            "{} planner is following the subagent hint and searching for `{query}`.",
                            self.config.model
                        ),
                        action: ModelAction::CallTool {
                            tool_name: "search_text".to_string(),
                            input: ToolInput::new()
                                .with_arg("root", ".")
                                .with_arg("query", query)
                                .with_arg("limit", "20"),
                        },
                    };
                }
            }
        }

        if let Some(edit_request) = edit_request.as_ref() {
            if !used_tools.contains("read_file")
                && tool_available("read_file")
                && !succeeded_tools.contains("apply_patch")
            {
                return ModelResponse {
                    message: format!(
                        "{} planner is reading the edit target before applying changes.",
                        self.config.model
                    ),
                    action: ModelAction::CallTool {
                        tool_name: "read_file".to_string(),
                        input: ToolInput::new()
                            .with_arg("path", edit_request.path.clone())
                            .with_arg("max_lines", "40"),
                    },
                };
            }
        }

        if let Some(edit_request) = edit_request.as_ref() {
            if tool_available("apply_patch") && successful_apply_patch_count == 1 {
                if let Some(retry_request) = derive_failed_validation_retry_edit_request(
                    edit_request,
                    &input.observations,
                    &task_lower,
                ) {
                    return ModelResponse {
                        message: format!(
                            "{} planner is applying a targeted retry in {} after the failed validation and readback.",
                            self.config.model, retry_request.path
                        ),
                        action: ModelAction::CallTool {
                            tool_name: "apply_patch".to_string(),
                            input: ToolInput::new()
                                .with_arg("path", retry_request.path)
                                .with_arg("find", retry_request.find)
                                .with_arg("replace", retry_request.replace),
                        },
                    };
                }
            }

            let apply_patch_available = tool_available("apply_patch");
            let already_succeeded = succeeded_tools.contains("apply_patch");
            let already_attempted = used_tools.contains("apply_patch");

            if apply_patch_available && !already_succeeded {
                if !already_attempted {
                    if let Some(plan) = crate::tools::apply_patch::build_single_line_diff(
                        &edit_request.path,
                        &edit_request.find,
                        &edit_request.replace,
                    ) {
                        return ModelResponse {
                            message: format!(
                                "{} planner is applying a unified diff patch in {}.",
                                self.config.model, edit_request.path
                            ),
                            action: ModelAction::CallTool {
                                tool_name: "apply_patch".to_string(),
                                input: ToolInput::new()
                                    .with_arg("cwd", plan.cwd)
                                    .with_arg("patch", plan.patch),
                            },
                        };
                    }

                    return ModelResponse {
                        message: format!(
                            "{} planner is applying a direct text replacement in {} (patch mode unavailable for this edit).",
                            self.config.model, edit_request.path
                        ),
                        action: ModelAction::CallTool {
                            tool_name: "apply_patch".to_string(),
                            input: ToolInput::new()
                                .with_arg("path", edit_request.path.clone())
                                .with_arg("find", edit_request.find.clone())
                                .with_arg("replace", edit_request.replace.clone()),
                        },
                    };
                }

                if last_apply_patch_was_patch_mode_failure {
                    return ModelResponse {
                        message: format!(
                            "{} planner retrying with text replacement after patch-mode failure in {}.",
                            self.config.model, edit_request.path
                        ),
                        action: ModelAction::CallTool {
                            tool_name: "apply_patch".to_string(),
                            input: ToolInput::new()
                                .with_arg("path", edit_request.path.clone())
                                .with_arg("find", edit_request.find.clone())
                                .with_arg("replace", edit_request.replace.clone()),
                        },
                    };
                }
            }
        }

        if edit_request.is_none() {
            if let Some(primary_file) = input.primary_file.as_deref() {
                if !used_tools.contains("read_file") && tool_available("read_file") {
                    return ModelResponse {
                        message: format!(
                            "{} planner is reading the primary file.",
                            self.config.model
                        ),
                        action: ModelAction::CallTool {
                            tool_name: "read_file".to_string(),
                            input: ToolInput::new()
                                .with_arg("path", primary_file)
                                .with_arg("max_lines", "40"),
                        },
                    };
                }
            }
        }

        if successful_apply_patch_count > git_diff_call_count && tool_available("git_diff") {
            return ModelResponse {
                message: format!(
                    "{} planner is reviewing the resulting diff.",
                    self.config.model
                ),
                action: ModelAction::CallTool {
                    tool_name: "git_diff".to_string(),
                    input: ToolInput::new(),
                },
            };
        }

        if let Some(test_command) = input.suggested_test_command.as_deref() {
            if wants_validation(&task_lower)
                && !task_is_lookup_heavy(&task_lower)
                && tool_available("run_shell")
                && should_run_validation_now(
                    &input.observations,
                    successful_apply_patch_count,
                    run_shell_call_count,
                )
            {
                return ModelResponse {
                    message: format!(
                        "{} planner is validating with `{}`.",
                        self.config.model, test_command
                    ),
                    action: ModelAction::CallTool {
                        tool_name: "run_shell".to_string(),
                        input: ToolInput::new()
                            .with_arg("cwd", ".")
                            .with_arg("command", test_command),
                    },
                };
            }
        }

        let mut message = format!(
            "{} offline planner finished after {} observation(s) for {}.",
            self.config.model,
            input.observations.len(),
            input.profile_name
        );

        if !input.system_prompt.is_empty() {
            let prompt_preview = input.system_prompt.lines().next().unwrap_or("").trim();
            if !prompt_preview.is_empty() {
                message.push_str(&format!(" Prompt frame: {prompt_preview}"));
            }
        }

        if let Some(last) = input.observations.last() {
            message.push_str(&format!(" Last observation came from {}.", last.tool_name));
        }

        ModelResponse {
            message,
            action: ModelAction::Finish,
        }
    }
}

#[derive(Clone, Copy)]
enum ApiFlavor {
    OpenAi,
    Anthropic,
}

fn api_flavor(base_url: &str) -> ApiFlavor {
    if base_url.trim_end_matches('/').ends_with("/anthropic") {
        ApiFlavor::Anthropic
    } else {
        ApiFlavor::OpenAi
    }
}

fn build_openai_tool_system_prompt(base: &str) -> String {
    format!(
        "{}\nUse the provided tools when a tool is needed. If no tool is needed, reply with a short plain-text summary.",
        base
    )
}

fn build_anthropic_tool_system_prompt(base: &str) -> String {
    format!(
        "{}\nUse the provided tools when a tool is needed. If no tool is needed, reply with a short plain-text summary.",
        base
    )
}

fn build_user_prompt(input: &ModelRequest) -> String {
    let mut prompt = String::new();
    prompt.push_str(&format!("Task: {}\n", input.task));
    prompt.push_str(&format!("Profile: {}\n", input.profile_name));
    if !input.profile_hints.is_empty() {
        prompt.push_str("Profile hints:\n");
        for hint in &input.profile_hints {
            prompt.push_str(&format!("- {hint}\n"));
        }
    }
    if let Some(primary_file) = input.primary_file.as_deref() {
        prompt.push_str(&format!("Primary file: {primary_file}\n"));
    }
    if let Some(command) = input.suggested_test_command.as_deref() {
        prompt.push_str(&format!("Suggested test command: {command}\n"));
    }
    prompt.push_str(&format!(
        "Available tools: {}\n",
        input.available_tools.join(", ")
    ));
    if let Some(next_action) = next_required_action_for_prompt(input) {
        prompt.push_str(&format!("Next required action: {next_action}\n"));
    }
    if !input.todos.is_empty() {
        prompt.push_str("Todos:\n");
        prompt.push_str(&render_todos_for_prompt(&input.todos));
        prompt.push_str("Execution plan:\n");
        prompt.push_str(&render_execution_plan(&input.todos));
        if let Some(current_step) = current_plan_step_line(&input.todos) {
            prompt.push_str(&format!("Current plan step: {current_step}\n"));
        }
    } else if input.planning_mode {
        prompt.push_str(
            "Current plan step: no plan yet; create one with todo_write before acting.\n",
        );
    }
    if !input.recent_steps.is_empty() {
        prompt.push_str("Recent agent steps (your prior assistant messages, oldest first — do NOT repeat work already done):\n");
        let base = input.recent_steps.len();
        for (offset, msg) in input.recent_steps.iter().enumerate() {
            // Trim each line; one-line summary per step keeps prompt compact.
            let first_line = msg.lines().next().unwrap_or("").trim();
            let preview = if first_line.chars().count() > 200 {
                let head: String = first_line.chars().take(200).collect();
                format!("{head}…")
            } else {
                first_line.to_string()
            };
            prompt.push_str(&format!(
                "- step {}: {}\n",
                offset + 1 + (base - input.recent_steps.len()),
                preview
            ));
        }
    }
    prompt.push_str("Observations:\n");
    if input.observations.is_empty() {
        prompt.push_str("- none\n");
    } else {
        for observation in &input.observations {
            let summary = observation
                .summary
                .lines()
                .take(6)
                .collect::<Vec<_>>()
                .join(" | ");
            prompt.push_str(&format!(
                "- tool={} summary={}\n",
                observation.tool_name, summary
            ));
        }
    }
    prompt
}

fn next_required_action_for_prompt(input: &ModelRequest) -> Option<String> {
    if let Some(edit_request) = derive_edit_request(&input.task) {
        let has_patch_tool = input
            .available_tools
            .iter()
            .any(|tool| tool == "apply_patch");
        let patch_already_succeeded = input
            .observations
            .iter()
            .any(|observation| observation.tool_name == "apply_patch" && !observation.is_failure());
        let read_confirmed_target = input.observations.iter().any(|observation| {
            observation.tool_name == "read_file"
                && !observation.is_failure()
                && observation.summary.contains(&edit_request.find)
        });
        if has_patch_tool && !patch_already_succeeded && read_confirmed_target {
            return Some(format!(
                "call apply_patch with path `{}`, find `{}`, and replace `{}` now; do not call read_file again before patching",
                edit_request.path, edit_request.find, edit_request.replace
            ));
        }
    }

    let command = input.suggested_test_command.as_deref()?;
    if !input.available_tools.iter().any(|tool| tool == "run_shell") {
        return None;
    }

    let last_patch_index = input.observations.iter().rposition(|observation| {
        observation.tool_name == "apply_patch" && !observation.is_failure()
    })?;
    let shell_after_patch = input.observations[last_patch_index + 1..]
        .iter()
        .find(|observation| observation.tool_name == "run_shell");

    match shell_after_patch {
        Some(observation)
            if observation.summary.contains("meta.command_kind=test")
                && observation.summary.contains("meta.result=ok") =>
        {
            Some("validation already passed; finish with a concise summary and do not call more tools".to_string())
        }
        Some(_) => None,
        None => Some(format!(
            "call run_shell with command `{command}` now; do not call read_file or git_diff before validation"
        )),
    }
}

fn render_todos_for_prompt(todos: &[crate::core::todos::Todo]) -> String {
    let mut out = String::new();
    for todo in todos {
        out.push_str(&format!("- [{}] {}\n", todo.status.label(), todo.content));
    }
    out
}

fn render_execution_plan(todos: &[crate::core::todos::Todo]) -> String {
    let mut out = String::new();
    for (index, todo) in todos.iter().enumerate() {
        out.push_str(&format!(
            "{}. [{}] {}\n",
            index + 1,
            todo.status.label(),
            todo.content
        ));
    }
    out
}

fn current_plan_step_line(todos: &[crate::core::todos::Todo]) -> Option<String> {
    if todos.is_empty() {
        return None;
    }

    if let Some((index, todo)) = todos
        .iter()
        .enumerate()
        .find(|(_, todo)| matches!(todo.status, crate::core::todos::TodoStatus::InProgress))
    {
        return Some(format!(
            "{}/{} [{}] {}",
            index + 1,
            todos.len(),
            todo.status.label(),
            todo.content
        ));
    }

    Some(format!(
        "none marked in_progress ({} total steps; update todo_write before continuing)",
        todos.len()
    ))
}

fn build_initial_todo_plan_json(input: &ModelRequest) -> String {
    let steps = collect_plan_steps(input, None);
    render_todo_plan_json(steps)
}

fn build_replan_todo_plan_json(input: &ModelRequest, reason: &str) -> String {
    let steps = collect_plan_steps(input, Some(reason));
    render_todo_plan_json(steps)
}

fn collect_plan_steps(input: &ModelRequest, replan_reason: Option<&str>) -> Vec<(String, String)> {
    let mut steps: Vec<(String, String)> = Vec::new();
    let tool_available = |name: &str| input.available_tools.iter().any(|tool| tool == name);
    let task_lower = input.task.to_lowercase();
    let search_query = derive_search_query(&input.task);
    let failure_repro_first =
        input.suggested_test_command.is_some() && task_requests_failure_repro(&task_lower);

    if let Some(reason) = replan_reason {
        let reason = clip_reason_for_todo(reason);
        steps.push((
            format!("Reassess the plan using the latest blocker or recovery signal ({reason})"),
            "Reassessing the plan using the latest blocker or recovery signal".to_string(),
        ));
    }

    if failure_repro_first && tool_available("run_shell") {
        steps.push((
            "Reproduce the failing validation command".to_string(),
            "Reproducing the failing validation command".to_string(),
        ));
    } else if search_query.is_some() && tool_available("search_text") {
        steps.push((
            "Search for the relevant code paths".to_string(),
            "Searching for the relevant code paths".to_string(),
        ));
    } else if let Some(primary_file) = input.primary_file.as_deref() {
        steps.push((
            format!("Inspect {}", primary_file),
            format!("Inspecting {}", primary_file),
        ));
    } else if tool_available("search_text") {
        steps.push((
            "Search for the relevant code paths".to_string(),
            "Searching for the relevant code paths".to_string(),
        ));
    } else if tool_available("list_files") {
        steps.push((
            "Inspect the repository layout".to_string(),
            "Inspecting the repository layout".to_string(),
        ));
    }

    if tool_available("read_file") {
        steps.push((
            "Read the most relevant files".to_string(),
            "Reading the most relevant files".to_string(),
        ));
    }

    if task_requests_plan_only(&task_lower) {
        steps.push((
            "Report the planned execution steps".to_string(),
            "Reporting the planned execution steps".to_string(),
        ));
    } else if tool_available("apply_patch") {
        steps.push((
            "Implement the requested changes".to_string(),
            "Implementing the requested changes".to_string(),
        ));
    } else if tool_available("run_shell") {
        steps.push((
            "Run the most relevant command for the task".to_string(),
            "Running the most relevant command for the task".to_string(),
        ));
    }

    if let Some(command) = input.suggested_test_command.as_deref() {
        if tool_available("run_shell")
            && wants_validation(&task_lower)
            && !task_is_lookup_heavy(&task_lower)
        {
            steps.push((
                format!("Validate with `{command}`"),
                format!("Validating with `{command}`"),
            ));
        }
    }

    if tool_available("git_diff") {
        steps.push((
            "Review the resulting diff".to_string(),
            "Reviewing the resulting diff".to_string(),
        ));
    }

    steps.push((
        "Summarize results and remaining risks".to_string(),
        "Summarizing results and remaining risks".to_string(),
    ));

    steps
}

fn render_todo_plan_json(steps: Vec<(String, String)>) -> String {
    let mut deduped: Vec<(String, String)> = Vec::new();
    for (content, active_form) in steps {
        if deduped.iter().any(|(existing, _)| existing == &content) {
            continue;
        }
        deduped.push((content, active_form));
    }

    let capped = deduped.into_iter().take(6).collect::<Vec<_>>();
    let mut json = String::from("[");
    for (index, (content, active_form)) in capped.iter().enumerate() {
        if index > 0 {
            json.push(',');
        }
        let status = if index == 0 { "in_progress" } else { "pending" };
        json.push_str(&format!(
            "{{\"content\":\"{}\",\"activeForm\":\"{}\",\"status\":\"{}\"}}",
            json_escape(content),
            json_escape(active_form),
            status
        ));
    }
    json.push(']');
    json
}

fn clip_reason_for_todo(reason: &str) -> String {
    const LIMIT: usize = 72;
    if reason.chars().count() <= LIMIT {
        return reason.to_string();
    }
    let head: String = reason.chars().take(LIMIT).collect();
    format!("{head}…")
}

fn derive_subagent_task(
    input: &ModelRequest,
    task_lower: &str,
    used_tools: &std::collections::BTreeSet<&str>,
) -> Option<String> {
    if !input.planning_mode
        || input.todos.is_empty()
        || !input
            .available_tools
            .iter()
            .any(|tool| tool == "dispatch_subagent")
        || used_tools.contains("dispatch_subagent")
    {
        return None;
    }
    if task_requests_plan_only(task_lower) || task_is_lookup_heavy(task_lower) {
        return None;
    }

    let unfinished = input
        .todos
        .iter()
        .filter(|todo| !matches!(todo.status, crate::core::todos::TodoStatus::Completed))
        .count();
    if unfinished < 3 {
        return None;
    }

    let task_has_multiple_workstreams = [
        " and ",
        " across ",
        " plus ",
        "meanwhile",
        "alongside",
        "multiple",
        "end-to-end",
    ]
    .iter()
    .any(|marker| task_lower.contains(marker));
    if !task_has_multiple_workstreams && input.todos.len() < 4 {
        return None;
    }

    let candidate = input
        .todos
        .iter()
        .find(|todo| {
            matches!(todo.status, crate::core::todos::TodoStatus::InProgress)
                && todo_looks_delegatable(&todo.content)
        })
        .or_else(|| {
            input.todos.iter().find(|todo| {
                matches!(todo.status, crate::core::todos::TodoStatus::Pending)
                    && todo_looks_delegatable(&todo.content)
            })
        })?;

    Some(format!(
        "Delegated todo step: {}. Parent task: {}. Summarize concrete findings, relevant file paths, and suggested next steps.",
        candidate.content,
        input.task
    ))
}

struct ParsedRecoveryHint<'a> {
    after: &'a str,
    next: &'a str,
    query: Option<&'a str>,
    path: Option<&'a str>,
    reason: &'a str,
}

struct ParsedReplanHint<'a> {
    reason: &'a str,
}

fn build_replan_response(model_name: &str, input: &ModelRequest) -> Option<ModelResponse> {
    let hint = latest_replan_hint(&input.observations)?;
    if !input
        .available_tools
        .iter()
        .any(|tool| tool == "todo_write")
    {
        return None;
    }
    if !input.planning_mode && input.todos.is_empty() {
        return None;
    }

    Some(ModelResponse {
        message: format!(
            "{model_name} planner is replanning before continuing. {}",
            hint.reason
        ),
        action: ModelAction::CallTool {
            tool_name: "todo_write".to_string(),
            input: ToolInput::new()
                .with_arg("items", build_replan_todo_plan_json(input, hint.reason)),
        },
    })
}

fn build_recovery_response(
    model_name: &str,
    input: &ModelRequest,
    used_tools: &std::collections::BTreeSet<&str>,
    succeeded_tools: &std::collections::BTreeSet<&str>,
    search_query: Option<&str>,
) -> Option<ModelResponse> {
    let hint = latest_recovery_hint(&input.observations)?;
    let tool_available = |name: &str| input.available_tools.iter().any(|tool| tool == name);
    let recovery_read_path =
        preferred_read_path(&input.observations, input.primary_file.as_deref());

    match hint.next {
        "git_diff" if tool_available("git_diff") && !used_tools.contains("git_diff") => {
            Some(ModelResponse {
                message: format!(
                    "{model_name} planner is recovering after {}: reviewing the diff before retrying.",
                    hint.after
                ),
                action: ModelAction::CallTool {
                    tool_name: "git_diff".to_string(),
                    input: ToolInput::new(),
                },
            })
        }
        "read_file" if tool_available("read_file") && !used_tools.contains("read_file") => {
            let path = hint
                .path
                .map(str::to_string)
                .or_else(|| preferred_read_path(&input.observations, input.primary_file.as_deref()))?;
            Some(ModelResponse {
                message: format!(
                    "{model_name} planner is recovering after {}: reading `{path}`. {}",
                    hint.after, hint.reason
                ),
                action: ModelAction::CallTool {
                    tool_name: "read_file".to_string(),
                    input: ToolInput::new()
                        .with_arg("path", path)
                        .with_arg("max_lines", "60"),
                },
            })
        }
        "git_diff"
            if tool_available("read_file")
                && used_tools.contains("git_diff")
                && !used_tools.contains("read_file") =>
        {
            let path = recovery_read_path?;
            Some(ModelResponse {
                message: format!(
                    "{model_name} planner already reviewed the diff after {} and is now rereading `{path}` before the next fix attempt. {}",
                    hint.after, hint.reason
                ),
                action: ModelAction::CallTool {
                    tool_name: "read_file".to_string(),
                    input: ToolInput::new()
                        .with_arg("path", path)
                        .with_arg("max_lines", "60"),
                },
            })
        }
        "search_text"
            if tool_available("search_text")
                && !used_tools.contains("search_text")
                && (hint.query.is_some() || search_query.is_some()) =>
        {
            let query = hint.query.or(search_query).unwrap_or_default();
            Some(ModelResponse {
                message: format!(
                    "{model_name} planner is recovering after {}: retrying with repository search `{query}`. {}",
                    hint.after, hint.reason
                ),
                action: ModelAction::CallTool {
                    tool_name: "search_text".to_string(),
                    input: ToolInput::new()
                        .with_arg("root", ".")
                        .with_arg("query", query)
                        .with_arg("limit", "20"),
                },
            })
        }
        "list_files" if tool_available("list_files") => Some(ModelResponse {
            message: format!(
                "{model_name} planner is recovering after {}: checking the repository layout first. {}",
                hint.after, hint.reason
            ),
            action: ModelAction::CallTool {
                tool_name: "list_files".to_string(),
                input: ToolInput::new()
                    .with_arg("root", ".")
                    .with_arg("max_depth", "3")
                    .with_arg("limit", "30"),
            },
        }),
        _ => {
            if hint.next == "git_diff"
                && !tool_available("git_diff")
                && succeeded_tools.contains("apply_patch")
            {
                return Some(ModelResponse {
                    message: format!(
                        "{model_name} planner noted a failed validation after code changes but no diff tool is available. {}",
                        hint.reason
                    ),
                    action: ModelAction::Finish,
                });
            }
            None
        }
    }
}

fn latest_recovery_hint(
    observations: &[crate::model::protocol::Observation],
) -> Option<ParsedRecoveryHint<'_>> {
    let observation = observations.last()?;
    if observation.tool_name != "recovery_hint" {
        return None;
    }

    let mut after = None;
    let mut next = None;
    let mut query = None;
    let mut path = None;
    let mut reason = None;
    for part in observation.summary.split(';') {
        let (key, value) = part.trim().split_once('=')?;
        match key.trim() {
            "after" => after = Some(value.trim()),
            "next" => next = Some(value.trim()),
            "query" => query = Some(value.trim()),
            "path" => path = Some(value.trim()),
            "reason" => reason = Some(value.trim()),
            _ => {}
        }
    }

    Some(ParsedRecoveryHint {
        after: after?,
        next: next?,
        query,
        path,
        reason: reason?,
    })
}

fn latest_replan_hint(
    observations: &[crate::model::protocol::Observation],
) -> Option<ParsedReplanHint<'_>> {
    let observation = observations.last()?;
    if observation.tool_name != "replan_hint" {
        return None;
    }

    let mut reason = None;
    for part in observation.summary.split(';') {
        let (key, value) = part.trim().split_once('=')?;
        if key.trim() == "reason" {
            reason = Some(value.trim());
        }
    }

    Some(ParsedReplanHint { reason: reason? })
}

fn preferred_read_path(
    observations: &[crate::model::protocol::Observation],
    primary_file: Option<&str>,
) -> Option<String> {
    if let Some(path) = last_search_result_path(observations) {
        return Some(path);
    }
    if let Some(path) = next_child_file_path(observations) {
        return Some(path);
    }
    if let Some(path) = last_patched_file_path(observations) {
        return Some(path);
    }
    if let Some(path) = last_listed_file_path(observations) {
        return Some(path);
    }
    primary_file.map(str::to_string)
}

fn observations_include_repo_signal(observations: &[crate::model::protocol::Observation]) -> bool {
    observations
        .iter()
        .any(|observation| match observation.tool_name.as_str() {
            "search_text" | "list_files" => !observation.is_failure(),
            "dispatch_subagent" => {
                !observation.is_failure()
                    && next_child_file_path(std::slice::from_ref(observation)).is_some()
            }
            _ => false,
        })
}

fn observations_include_pr_review_signal(
    observations: &[crate::model::protocol::Observation],
) -> bool {
    observations.iter().any(|observation| {
        matches!(observation.tool_name.as_str(), "git_diff" | "list_files")
            && !observation.is_failure()
    })
}

fn observations_include_pr_or_ci_signal(
    observations: &[crate::model::protocol::Observation],
) -> bool {
    observations.iter().any(|observation| {
        matches!(
            observation.tool_name.as_str(),
            "git_diff" | "list_files" | "run_shell"
        )
    })
}

fn last_search_result_path(observations: &[crate::model::protocol::Observation]) -> Option<String> {
    observations
        .iter()
        .rev()
        .find(|observation| observation.tool_name == "search_text" && !observation.is_failure())
        .and_then(|observation| {
            observation
                .summary
                .lines()
                .find(|line| !line.trim().is_empty() && !line.starts_with("No matches for `"))
        })
        .and_then(|line| line.splitn(3, ':').next())
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .map(str::to_string)
}

fn last_listed_file_path(observations: &[crate::model::protocol::Observation]) -> Option<String> {
    observations
        .iter()
        .rev()
        .find(|observation| observation.tool_name == "list_files" && !observation.is_failure())
        .and_then(|observation| {
            observation
                .summary
                .lines()
                .map(str::trim)
                .find(|line| !line.is_empty() && !line.ends_with('/'))
        })
        .map(str::to_string)
}

fn last_patched_file_path(observations: &[crate::model::protocol::Observation]) -> Option<String> {
    observations
        .iter()
        .rev()
        .find(|observation| observation.tool_name == "apply_patch" && !observation.is_failure())
        .and_then(|observation| {
            if let Some(path) = observation
                .summary
                .lines()
                .find_map(|line| line.strip_prefix("patched ").map(str::trim))
            {
                return Some(path.to_string());
            }

            let cwd = observation.summary.lines().find_map(|line| {
                line.strip_prefix("Applied unified patch in ")
                    .and_then(|value| value.split_once(" (touched "))
                    .map(|(value, _)| value.trim().to_string())
            });
            let modified = observation.summary.lines().find_map(|line| {
                let trimmed = line.trim_start();
                trimmed
                    .strip_prefix("- ")
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(str::to_string)
            })?;
            Some(match cwd.as_deref() {
                Some(".") | None => modified,
                Some(cwd) => format!("{cwd}/{modified}"),
            })
        })
        .filter(|path| !path.is_empty())
}

fn tool_call_count(observations: &[crate::model::protocol::Observation], tool_name: &str) -> usize {
    observations
        .iter()
        .filter(|observation| observation.tool_name == tool_name)
        .count()
}

fn successful_tool_call_count(
    observations: &[crate::model::protocol::Observation],
    tool_name: &str,
) -> usize {
    observations
        .iter()
        .filter(|observation| {
            observation.tool_name == tool_name
                && matches!(
                    observation.status,
                    crate::model::protocol::ObservationStatus::Ok
                )
        })
        .count()
}

fn should_run_validation_now(
    observations: &[crate::model::protocol::Observation],
    successful_apply_patch_count: usize,
    run_shell_call_count: usize,
) -> bool {
    if successful_apply_patch_count > 0 {
        let Some(last_patch_index) = observations.iter().rposition(|observation| {
            observation.tool_name == "apply_patch" && !observation.is_failure()
        }) else {
            return false;
        };
        return observations
            .iter()
            .rposition(|observation| observation.tool_name == "run_shell")
            .is_none_or(|last_shell_index| last_shell_index < last_patch_index);
    } else {
        run_shell_call_count == 0
    }
}

fn derive_failed_validation_retry_edit_request(
    edit_request: &EditRequest,
    observations: &[crate::model::protocol::Observation],
    task_lower: &str,
) -> Option<EditRequest> {
    if !task_requests_retry_until_passing(task_lower) {
        return None;
    }

    let last_read = observations.last()?;
    if last_read.tool_name != "read_file" {
        return None;
    }

    let failed_tests = latest_failed_tests(observations);
    let readback_text = last_read.summary.to_ascii_lowercase();
    let readback_matches_edit = last_read.summary.contains(&edit_request.replace);
    let readback_looks_like_test = readback_text.contains("test(")
        || readback_text.contains("node:test")
        || readback_text.contains("assert.")
        || readback_text.contains("describe(")
        || readback_text.contains("it(");
    if !readback_matches_edit && !readback_looks_like_test {
        return None;
    }

    let desired_operator = infer_operator_from_failed_tests(&failed_tests)
        .or_else(|| infer_operator_from_text(&last_read.summary))?;
    let (lhs, current_operator, rhs) = split_arithmetic_expression(&edit_request.replace)?;
    if current_operator == desired_operator {
        return None;
    }

    Some(EditRequest {
        path: edit_request.path.clone(),
        find: edit_request.replace.clone(),
        replace: format!("{} {} {}", lhs.trim(), desired_operator, rhs.trim()),
    })
}

fn task_requests_retry_until_passing(task_lower: &str) -> bool {
    [
        "until the tests pass",
        "until tests pass",
        "until it passes",
        "keep fixing",
        "retry until",
    ]
    .iter()
    .any(|needle| task_lower.contains(needle))
}

fn latest_failed_tests(observations: &[crate::model::protocol::Observation]) -> Vec<String> {
    let Some(run_shell) = observations
        .iter()
        .rev()
        .find(|observation| observation.tool_name == "run_shell")
    else {
        return Vec::new();
    };

    for line in run_shell.summary.lines() {
        if let Some(raw) = line.trim().strip_prefix("meta.failed_tests=") {
            return raw
                .split(',')
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
                .collect();
        }
    }

    Vec::new()
}

fn infer_operator_from_failed_tests(failed_tests: &[String]) -> Option<char> {
    let combined = failed_tests
        .iter()
        .map(|value| value.to_ascii_lowercase())
        .collect::<Vec<_>>()
        .join(" ");
    infer_operator_from_text(&combined)
}

fn infer_operator_from_text(text: &str) -> Option<char> {
    let combined = text.to_ascii_lowercase();
    if combined.contains("add") || combined.contains("sum") || combined.contains("plus") {
        Some('+')
    } else if combined.contains("sub") || combined.contains("minus") || combined.contains("diff") {
        Some('-')
    } else if combined.contains("mul") || combined.contains("times") || combined.contains("product")
    {
        Some('*')
    } else if combined.contains("div") || combined.contains("quot") {
        Some('/')
    } else {
        None
    }
}

fn split_arithmetic_expression(expression: &str) -> Option<(&str, char, &str)> {
    for (index, ch) in expression.char_indices() {
        if !matches!(ch, '+' | '-' | '*' | '/') {
            continue;
        }
        let lhs = expression[..index].trim();
        let rhs = expression[index + ch.len_utf8()..].trim();
        if !lhs.is_empty() && !rhs.is_empty() {
            return Some((lhs, ch, rhs));
        }
    }
    None
}

fn next_child_file_path(observations: &[crate::model::protocol::Observation]) -> Option<String> {
    let (dispatch_index, dispatch_observation) =
        observations
            .iter()
            .enumerate()
            .rev()
            .find(|(_, observation)| {
                observation.tool_name == "dispatch_subagent" && !observation.is_failure()
            })?;
    let child_files = child_files_from_summary(&dispatch_observation.summary);
    if child_files.is_empty() {
        return None;
    }
    let reads_since_dispatch = observations
        .iter()
        .skip(dispatch_index + 1)
        .filter(|observation| observation.tool_name == "read_file" && !observation.is_failure())
        .count();
    child_files.into_iter().nth(reads_since_dispatch)
}

fn child_files_from_summary(summary: &str) -> Vec<String> {
    let mut explicit = Vec::new();
    if let Some(path) = child_next_action_read_file(summary) {
        explicit.push(path);
    }
    explicit.extend(
        summary
            .lines()
            .find_map(|line| line.strip_prefix("meta.child_files="))
            .map(|value| {
                value
                    .split(',')
                    .map(str::trim)
                    .filter(|path| !path.is_empty() && *path != "none")
                    .map(str::to_string)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default(),
    );
    explicit.dedup();
    if !explicit.is_empty() {
        return explicit;
    }

    child_final_message_from_summary(summary)
        .map(|message| path_like_tokens(&message))
        .unwrap_or_default()
}

fn child_followup_query(observations: &[crate::model::protocol::Observation]) -> Option<String> {
    observations
        .iter()
        .rev()
        .find(|observation| {
            observation.tool_name == "dispatch_subagent" && !observation.is_failure()
        })
        .and_then(|observation| child_next_action_search_query(&observation.summary))
        .or_else(|| {
            observations
                .iter()
                .rev()
                .find(|observation| {
                    observation.tool_name == "dispatch_subagent" && !observation.is_failure()
                })
                .and_then(|observation| child_final_message_from_summary(&observation.summary))
                .as_deref()
                .and_then(first_quoted_segment)
        })
        .or_else(|| {
            observations
                .iter()
                .rev()
                .find(|observation| {
                    observation.tool_name == "dispatch_subagent" && !observation.is_failure()
                })
                .and_then(|observation| child_final_message_from_summary(&observation.summary))
                .as_deref()
                .and_then(identifier_like_token)
        })
}

fn child_next_action_read_file(summary: &str) -> Option<String> {
    summary
        .lines()
        .find_map(|line| line.strip_prefix("meta.child_next_action=read_file:"))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn child_next_action_search_query(summary: &str) -> Option<String> {
    summary
        .lines()
        .find_map(|line| line.strip_prefix("meta.child_next_action=search_text:"))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn child_final_message_from_summary(summary: &str) -> Option<String> {
    summary
        .lines()
        .find_map(|line| line.strip_prefix("meta.child_final_message="))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn path_like_tokens(text: &str) -> Vec<String> {
    let mut paths = Vec::new();
    for token in text.split_whitespace() {
        let trimmed = token.trim_matches(|ch: char| {
            ch.is_ascii_punctuation() && ch != '/' && ch != '.' && ch != '_' && ch != '-'
        });
        if trimmed.len() < 4 || !trimmed.contains('/') || !trimmed.contains('.') {
            continue;
        }
        if trimmed.starts_with('/') || trimmed.ends_with('/') {
            continue;
        }
        if !paths.iter().any(|existing| existing == trimmed) {
            paths.push(trimmed.to_string());
        }
    }
    paths
}

fn todo_looks_delegatable(content: &str) -> bool {
    let lower = content.to_lowercase();
    [
        "inspect",
        "read",
        "search",
        "research",
        "investigate",
        "explore",
        "map",
        "summarize findings",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
}

fn task_requests_plan_only(task_lower: &str) -> bool {
    (task_lower.contains("before acting")
        || task_lower.contains("before making changes")
        || task_lower.contains("report the execution steps")
        || task_lower.contains("only plan")
        || task_lower.contains("just plan"))
        && !task_lower.contains("implement")
        && !task_lower.contains("apply the fix")
}

fn task_is_lookup_heavy(task_lower: &str) -> bool {
    [
        "find where",
        "locate ",
        "where is",
        "where are",
        "search for",
        "look up",
    ]
    .iter()
    .any(|marker| task_lower.contains(marker))
}

fn task_requests_failure_repro(task_lower: &str) -> bool {
    [
        "investigate why",
        "investigate the failing test",
        "failing test",
        "test fails",
        "test failure",
        "lint failure",
        "build failure",
        "reproduce locally",
        "before retrying",
    ]
    .iter()
    .any(|needle| task_lower.contains(needle))
}

fn task_looks_like_pr_workflow(task_lower: &str) -> bool {
    task_lower.contains("pull request")
        || task_lower.contains("pr #")
        || task_lower.contains("review feedback")
        || task_lower.contains("failed ci")
        || task_lower.contains("ci job")
}

fn query_looks_code_like(query: &str) -> bool {
    let trimmed = query.trim();
    if trimmed.is_empty() {
        return false;
    }
    trimmed.contains('_')
        || trimmed.contains("::")
        || trimmed.contains('/')
        || trimmed.contains('.')
        || trimmed
            .chars()
            .zip(trimmed.chars().skip(1))
            .any(|(left, right)| left.is_ascii_lowercase() && right.is_ascii_uppercase())
}

fn task_requests_subagent_followup(task_lower: &str) -> bool {
    task_lower.contains("subagent")
        || task_lower.contains("child loop")
        || task_lower.contains("child loops")
        || task_lower.contains("parent and child")
}

fn task_allows_child_file_followup(
    task_lower: &str,
    observations: &[crate::model::protocol::Observation],
) -> bool {
    task_requests_subagent_followup(task_lower)
        || (task_looks_like_pr_workflow(task_lower)
            && observations.iter().any(|observation| {
                observation.tool_name == "dispatch_subagent" && !observation.is_failure()
            }))
}

fn pr_workflow_child_followup_complete(
    task_lower: &str,
    observations: &[crate::model::protocol::Observation],
) -> bool {
    task_looks_like_pr_workflow(task_lower)
        && task_allows_child_file_followup(task_lower, observations)
        && next_child_file_path(observations).is_none()
        && observations
            .iter()
            .rev()
            .any(|observation| observation.tool_name == "read_file" && !observation.is_failure())
}

fn build_openai_tools(names: &[String]) -> String {
    render_tools(names, openai_envelope)
}

fn build_anthropic_tools(names: &[String]) -> String {
    render_tools(names, anthropic_envelope)
}

fn render_tools(names: &[String], envelope: fn(&ToolSpec) -> String) -> String {
    let tools = names
        .iter()
        .filter_map(|name| tool_spec(name).map(envelope))
        .collect::<Vec<_>>();
    format!("[{}]", tools.join(","))
}

fn openai_envelope(spec: &ToolSpec) -> String {
    format!(
        r#"{{"type":"function","function":{{"name":"{}","description":"{}","parameters":{{"type":"object","properties":{},"required":{},"additionalProperties":false}}}}}}"#,
        json_escape(spec.name),
        json_escape(spec.description),
        spec.properties_json,
        spec.required_json,
    )
}

fn anthropic_envelope(spec: &ToolSpec) -> String {
    format!(
        r#"{{"name":"{}","description":"{}","input_schema":{{"type":"object","properties":{},"required":{}}}}}"#,
        json_escape(spec.name),
        json_escape(spec.description),
        spec.properties_json,
        spec.required_json,
    )
}

struct ToolSpec {
    name: &'static str,
    description: &'static str,
    properties_json: &'static str,
    required_json: &'static str,
}

fn tool_spec(name: &str) -> Option<&'static ToolSpec> {
    TOOL_SPECS.iter().find(|spec| spec.name == name)
}

const TOOL_SPECS: &[ToolSpec] = &[
    ToolSpec {
        name: "list_files",
        description: "List repository files and directories under a root path.",
        properties_json: r#"{"root":{"type":"string","description":"Root directory to list from, usually `.`."},"max_depth":{"type":"string","description":"Maximum directory depth to traverse, encoded as a string integer."},"limit":{"type":"string","description":"Maximum number of entries to return, encoded as a string integer."}}"#,
        required_json: r#"["root","max_depth","limit"]"#,
    },
    ToolSpec {
        name: "read_file",
        description: "Read a text file and return a numbered excerpt.",
        properties_json: r#"{"path":{"type":"string","description":"Path to the file."},"max_lines":{"type":"string","description":"Maximum number of lines to return, encoded as a string integer."}}"#,
        required_json: r#"["path","max_lines"]"#,
    },
    ToolSpec {
        name: "search_text",
        description: "Search for plain text occurrences in repository files.",
        properties_json: r#"{"root":{"type":"string","description":"Root directory to search from."},"query":{"type":"string","description":"Plain text query to find."},"limit":{"type":"string","description":"Maximum number of matches to return, encoded as a string integer."}}"#,
        required_json: r#"["root","query","limit"]"#,
    },
    ToolSpec {
        name: "apply_patch",
        description: "Apply a text replacement or a unified diff patch to files.",
        properties_json: r#"{"cwd":{"type":"string","description":"Working directory used when applying a unified diff patch."},"path":{"type":"string","description":"Target file path for direct replacement mode."},"find":{"type":"string","description":"Exact text to find for direct replacement mode."},"replace":{"type":"string","description":"Replacement text for direct replacement mode."},"replace_all":{"type":"string","description":"`true` to replace all occurrences in direct replacement mode, otherwise `false`."},"patch":{"type":"string","description":"Unified diff patch content. When provided, patch mode is used and path/find/replace are optional."}}"#,
        required_json: r#"[]"#,
    },
    ToolSpec {
        name: "run_shell",
        description: "Run a safe allowlisted shell command in the repository.",
        properties_json: r#"{"cwd":{"type":"string","description":"Working directory for the command."},"command":{"type":"string","description":"Safe shell command to execute."}}"#,
        required_json: r#"["cwd","command"]"#,
    },
    ToolSpec {
        name: "git_diff",
        description: "Show the current git diff for the workspace.",
        properties_json: r#"{}"#,
        required_json: r#"[]"#,
    },
    ToolSpec {
        name: "todo_write",
        description: "Replace the entire todo list with a new set of items. Use proactively for tasks with 3+ steps; mark exactly one item as in_progress at a time.",
        properties_json: r#"{"items":{"type":"string","description":"JSON array of objects with fields {content: string, activeForm: string, status: \"pending\"|\"in_progress\"|\"completed\"}. content is imperative form (e.g. \"Run tests\"); activeForm is present continuous (e.g. \"Running tests\")."}}"#,
        required_json: r#"["items"]"#,
    },
    ToolSpec {
        name: "dispatch_subagent",
        description: "Delegate an independent subtask to a child agent with its own budget and todo list.",
        properties_json: r#"{"task":{"type":"string","description":"Concrete self-contained subtask for the child agent."},"skill":{"type":"string","description":"Optional skill name for the child agent."},"steps":{"type":"string","description":"Optional step budget for the child agent, as a positive integer up to 12."}}"#,
        required_json: r#"["task"]"#,
    },
];

#[derive(Default, Debug)]
struct OpenAiToolAssembly {
    index: u64,
    #[allow(dead_code)]
    id: Option<String>,
    name: Option<String>,
    arguments: String,
}

pub(crate) fn parse_openai_stream<R: BufRead>(
    reader: &mut R,
    events: &mut dyn StreamEvents,
) -> AppResult<(ModelResponse, Option<TokenUsage>)> {
    let mut full_text = String::new();
    let result = parse_openai_stream_inner(reader, events, &mut full_text);
    events.on_assistant_done(&full_text);
    result
}

fn parse_openai_stream_inner<R: BufRead>(
    reader: &mut R,
    events: &mut dyn StreamEvents,
    full_text: &mut String,
) -> AppResult<(ModelResponse, Option<TokenUsage>)> {
    let mut usage: Option<TokenUsage> = None;
    let mut tool_assembly: Option<OpenAiToolAssembly> = None;
    let mut done_seen = false;

    while let Some(frame) =
        read_frame(reader).map_err(|e| app_error(format!("sse read failed: {e}")))?
    {
        let data = frame.data.trim();
        if data.is_empty() {
            continue;
        }
        if data == "[DONE]" {
            // Continue draining frames in case the server emits a trailing usage frame.
            done_seen = true;
            continue;
        }
        let root = parse_root_object(data)
            .map_err(|e| tool_failure(format!("malformed openai sse frame: {e}")))?;

        if let Some(error) = root.get("error").and_then(json_as_object) {
            let message = error
                .get("message")
                .and_then(json_as_string)
                .unwrap_or("openai stream error");
            return Err(tool_failure(format!("openai api error: {message}")));
        }

        if let Some(usage_obj) = root.get("usage").and_then(json_as_object) {
            if let (Some(p), Some(c)) = (
                usage_obj.get("prompt_tokens").and_then(json_as_u64),
                usage_obj.get("completion_tokens").and_then(json_as_u64),
            ) {
                usage = Some(TokenUsage {
                    prompt: p,
                    completion: c,
                });
            }
        }

        let Some(choices) = root.get("choices").and_then(json_as_array) else {
            continue;
        };
        let Some(choice) = choices.first().and_then(json_as_object) else {
            continue;
        };
        if let Some(delta) = choice.get("delta").and_then(json_as_object) {
            if !done_seen {
                if let Some(content) = delta.get("content").and_then(json_as_string) {
                    if !content.is_empty() {
                        events.on_text_delta(content);
                        full_text.push_str(content);
                    }
                }
                if let Some(tool_calls) = delta.get("tool_calls").and_then(json_as_array) {
                    for call in tool_calls {
                        let Some(call_obj) = json_as_object(call) else {
                            continue;
                        };
                        // OpenAI streams tool calls indexed by `index`. The loop executes one
                        // tool call per turn; if a gateway ignores `parallel_tool_calls:false`,
                        // keep the first call and let the next turn continue from its result.
                        let observed_index = call_obj.get("index").and_then(json_as_u64);
                        match (tool_assembly.as_mut(), observed_index) {
                            (Some(existing), Some(idx)) if existing.index != idx => {
                                continue;
                            }
                            (Some(_), _) => {}
                            (None, _) => {
                                tool_assembly = Some(OpenAiToolAssembly {
                                    index: observed_index.unwrap_or(0),
                                    ..OpenAiToolAssembly::default()
                                });
                            }
                        }
                        let assembly = tool_assembly.as_mut().expect("assembly seeded above");
                        if assembly.id.is_none() {
                            if let Some(id) = call_obj.get("id").and_then(json_as_string) {
                                assembly.id = Some(id.to_string());
                            }
                        }
                        if let Some(function) = call_obj.get("function").and_then(json_as_object) {
                            if assembly.name.is_none() {
                                if let Some(name) = function.get("name").and_then(json_as_string) {
                                    assembly.name = Some(name.to_string());
                                }
                            }
                            if let Some(args) = function.get("arguments").and_then(json_as_string) {
                                assembly.arguments.push_str(args);
                            }
                        }
                    }
                }
            }
        }
    }

    let action = if let Some(assembly) = tool_assembly {
        let name = assembly
            .name
            .ok_or_else(|| tool_failure("openai tool call missing function.name"))?;
        let arguments = if assembly.arguments.trim().is_empty() {
            std::collections::BTreeMap::new()
        } else {
            parse_tool_arguments(&assembly.arguments)?
        };
        events.on_tool_call(&name, &arguments);
        ModelAction::CallTool {
            tool_name: name,
            input: ToolInput { args: arguments },
        }
    } else {
        ModelAction::Finish
    };

    let message = if full_text.is_empty() && matches!(action, ModelAction::CallTool { .. }) {
        "DeepSeek selected a tool.".to_string()
    } else if full_text.is_empty() {
        "DeepSeek returned no content.".to_string()
    } else {
        std::mem::take(full_text)
    };

    Ok((ModelResponse { message, action }, usage))
}

#[derive(Default, Debug)]
struct AnthropicToolAssembly {
    index: u64,
    #[allow(dead_code)]
    id: Option<String>,
    name: Option<String>,
    partial_json: String,
}

pub(crate) fn parse_anthropic_stream<R: BufRead>(
    reader: &mut R,
    events: &mut dyn StreamEvents,
) -> AppResult<(ModelResponse, Option<TokenUsage>)> {
    let mut full_text = String::new();
    let result = parse_anthropic_stream_inner(reader, events, &mut full_text);
    events.on_assistant_done(&full_text);
    result
}

fn parse_anthropic_stream_inner<R: BufRead>(
    reader: &mut R,
    events: &mut dyn StreamEvents,
    full_text: &mut String,
) -> AppResult<(ModelResponse, Option<TokenUsage>)> {
    let mut tool_assembly: Option<AnthropicToolAssembly> = None;
    let mut usage_prompt: Option<u64> = None;
    let mut usage_completion: Option<u64> = None;

    while let Some(frame) =
        read_frame(reader).map_err(|e| app_error(format!("sse read failed: {e}")))?
    {
        let event_kind = frame.event.as_deref().unwrap_or("");
        let data = frame.data.trim();
        if data.is_empty() {
            if event_kind == "message_stop" {
                break;
            }
            continue;
        }
        let root = parse_root_object(data)
            .map_err(|e| tool_failure(format!("malformed anthropic sse frame: {e}")))?;

        match event_kind {
            "message_start" => {
                if let Some(message) = root.get("message").and_then(json_as_object) {
                    if let Some(usage_obj) = message.get("usage").and_then(json_as_object) {
                        if let Some(p) = usage_obj.get("input_tokens").and_then(json_as_u64) {
                            usage_prompt = Some(p);
                        }
                        if let Some(c) = usage_obj.get("output_tokens").and_then(json_as_u64) {
                            usage_completion = Some(c);
                        }
                    }
                }
            }
            "content_block_start" => {
                if let Some(block) = root.get("content_block").and_then(json_as_object) {
                    if block.get("type").and_then(json_as_string) == Some("tool_use") {
                        let block_index = root.get("index").and_then(json_as_u64).unwrap_or(0);
                        if let Some(existing) = tool_assembly.as_ref() {
                            if existing.index == block_index {
                                return Err(tool_failure(format!(
                                    "anthropic stream re-emitted content_block_start for tool_use at index {} (server bug)",
                                    block_index
                                )));
                            }
                            return Err(tool_failure(format!(
                                "anthropic stream emitted multiple parallel tool_use blocks (indices {} and {}); only one is supported per turn",
                                existing.index, block_index
                            )));
                        } else {
                            let id = block.get("id").and_then(json_as_string).map(str::to_string);
                            let name = block
                                .get("name")
                                .and_then(json_as_string)
                                .map(str::to_string);
                            tool_assembly = Some(AnthropicToolAssembly {
                                index: block_index,
                                id,
                                name,
                                partial_json: String::new(),
                            });
                        }
                    }
                }
            }
            "content_block_delta" => {
                if let Some(delta) = root.get("delta").and_then(json_as_object) {
                    let delta_type = delta.get("type").and_then(json_as_string).unwrap_or("");
                    match delta_type {
                        "text_delta" => {
                            if let Some(text) = delta.get("text").and_then(json_as_string) {
                                if !text.is_empty() {
                                    events.on_text_delta(text);
                                    full_text.push_str(text);
                                }
                            }
                        }
                        "input_json_delta" => {
                            if let Some(partial) =
                                delta.get("partial_json").and_then(json_as_string)
                            {
                                let delta_index = root.get("index").and_then(json_as_u64);
                                if let Some(assembly) = tool_assembly.as_mut() {
                                    let matches_assembly = match delta_index {
                                        Some(idx) => assembly.index == idx,
                                        None => true, // continue current assembly when index absent
                                    };
                                    if matches_assembly {
                                        assembly.partial_json.push_str(partial);
                                    }
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            "message_delta" => {
                if let Some(usage_obj) = root.get("usage").and_then(json_as_object) {
                    if let Some(c) = usage_obj.get("output_tokens").and_then(json_as_u64) {
                        usage_completion = Some(c);
                    }
                    if let Some(p) = usage_obj.get("input_tokens").and_then(json_as_u64) {
                        usage_prompt = Some(p);
                    }
                }
            }
            "message_stop" => {
                break;
            }
            "error" => {
                let message = root
                    .get("error")
                    .and_then(json_as_object)
                    .and_then(|e| e.get("message"))
                    .and_then(json_as_string)
                    .unwrap_or("anthropic stream error");
                return Err(tool_failure(format!("anthropic api error: {message}")));
            }
            _ => {}
        }
    }

    let action = if let Some(assembly) = tool_assembly {
        let name = assembly
            .name
            .ok_or_else(|| tool_failure("anthropic tool_use missing name"))?;
        let arguments = if assembly.partial_json.trim().is_empty() {
            std::collections::BTreeMap::new()
        } else {
            parse_tool_arguments(&assembly.partial_json)?
        };
        events.on_tool_call(&name, &arguments);
        ModelAction::CallTool {
            tool_name: name,
            input: ToolInput { args: arguments },
        }
    } else {
        ModelAction::Finish
    };

    let usage = match (usage_prompt, usage_completion) {
        (Some(p), Some(c)) => Some(TokenUsage {
            prompt: p,
            completion: c,
        }),
        _ => None,
    };

    let message = if full_text.is_empty() && matches!(action, ModelAction::CallTool { .. }) {
        "DeepSeek selected a tool.".to_string()
    } else if full_text.is_empty() {
        "DeepSeek returned no content.".to_string()
    } else {
        std::mem::take(full_text)
    };

    Ok((ModelResponse { message, action }, usage))
}

#[allow(dead_code)]
fn parse_openai_chat_completion(body: &str) -> AppResult<ModelResponse> {
    let root = parse_root_object(body)?;
    let choices = root
        .get("choices")
        .and_then(json_as_array)
        .ok_or_else(|| app_error("chat completion response missing `choices` array"))?;
    let first_choice = choices
        .first()
        .and_then(json_as_object)
        .ok_or_else(|| app_error("chat completion response missing first choice"))?;
    let message = first_choice
        .get("message")
        .and_then(json_as_object)
        .ok_or_else(|| app_error("chat completion response missing message object"))?;

    if let Some(tool_calls) = message.get("tool_calls").and_then(json_as_array) {
        let first_call = tool_calls
            .first()
            .and_then(json_as_object)
            .ok_or_else(|| app_error("tool_calls array was empty"))?;
        let function = first_call
            .get("function")
            .and_then(json_as_object)
            .ok_or_else(|| app_error("tool call missing function object"))?;
        let tool_name = function
            .get("name")
            .and_then(json_as_string)
            .ok_or_else(|| app_error("tool call missing function name"))?;
        let arguments_raw = function
            .get("arguments")
            .and_then(json_as_string)
            .ok_or_else(|| app_error("tool call missing function arguments"))?;
        let arguments = parse_tool_arguments(arguments_raw)?;
        let assistant_message = message
            .get("content")
            .and_then(json_as_string)
            .unwrap_or("DeepSeek selected a tool.")
            .to_string();

        return Ok(ModelResponse {
            message: assistant_message,
            action: ModelAction::CallTool {
                tool_name: tool_name.to_string(),
                input: ToolInput { args: arguments },
            },
        });
    }

    let message = message
        .get("content")
        .and_then(json_as_string)
        .unwrap_or("DeepSeek returned no content.")
        .to_string();

    Ok(ModelResponse {
        message,
        action: ModelAction::Finish,
    })
}

#[allow(dead_code)]
fn parse_openai_usage(body: &str) -> Option<TokenUsage> {
    let root = parse_root_object(body).ok()?;
    let usage = json_as_object(root.get("usage")?)?;
    let prompt = json_as_u64(usage.get("prompt_tokens")?)?;
    let completion = json_as_u64(usage.get("completion_tokens")?)?;
    Some(TokenUsage { prompt, completion })
}

#[allow(dead_code)]
fn parse_anthropic_messages(body: &str) -> AppResult<ModelResponse> {
    let root = parse_root_object(body)?;

    if let Some(error) = root.get("error").and_then(json_as_object) {
        let message = error
            .get("message")
            .and_then(json_as_string)
            .unwrap_or("anthropic api returned an error");
        return Err(app_error(format!("anthropic error: {message}")));
    }

    let content = root
        .get("content")
        .and_then(json_as_array)
        .ok_or_else(|| app_error("anthropic response missing `content` array"))?;

    let mut text_chunks = Vec::new();
    for item in content {
        let Some(block) = json_as_object(item) else {
            continue;
        };
        let block_type = block.get("type").and_then(json_as_string).unwrap_or("");
        match block_type {
            "tool_use" => {
                let tool_name = block
                    .get("name")
                    .and_then(json_as_string)
                    .ok_or_else(|| app_error("tool_use block missing `name`"))?;
                let input_obj = block
                    .get("input")
                    .ok_or_else(|| app_error("tool_use block missing `input`"))?;
                let arguments = json_object_to_string_args(input_obj)?;
                let assistant_message = if text_chunks.is_empty() {
                    "DeepSeek selected a tool.".to_string()
                } else {
                    text_chunks.join("\n")
                };
                return Ok(ModelResponse {
                    message: assistant_message,
                    action: ModelAction::CallTool {
                        tool_name: tool_name.to_string(),
                        input: ToolInput { args: arguments },
                    },
                });
            }
            "text" => {
                if let Some(value) = block.get("text").and_then(json_as_string) {
                    text_chunks.push(value.to_string());
                }
            }
            _ => {}
        }
    }

    let message = if text_chunks.is_empty() {
        "DeepSeek returned no content.".to_string()
    } else {
        text_chunks.join("\n")
    };

    Ok(ModelResponse {
        message,
        action: ModelAction::Finish,
    })
}

#[allow(dead_code)]
fn parse_anthropic_usage(body: &str) -> Option<TokenUsage> {
    let root = parse_root_object(body).ok()?;
    let usage = json_as_object(root.get("usage")?)?;
    let prompt = json_as_u64(usage.get("input_tokens")?)?;
    let completion = json_as_u64(usage.get("output_tokens")?)?;
    Some(TokenUsage { prompt, completion })
}

fn json_object_to_string_args(value: &JsonValue) -> AppResult<BTreeMap<String, String>> {
    let JsonValue::Object(map) = value else {
        return Err(app_error("tool input must be a json object"));
    };

    let mut result = BTreeMap::new();
    for (key, value) in map {
        match value {
            JsonValue::String(value) => {
                result.insert(key.clone(), value.clone());
            }
            JsonValue::Number(value) => {
                result.insert(key.clone(), value.clone());
            }
            JsonValue::Bool(value) => {
                result.insert(
                    key.clone(),
                    if *value { "true" } else { "false" }.to_string(),
                );
            }
            JsonValue::Null => {
                result.insert(key.clone(), "null".to_string());
            }
            JsonValue::Object(_) | JsonValue::Array(_) => {
                // Re-serialize nested values back to JSON strings so ToolInput.args
                // (BTreeMap<String, String>) can carry them. Tools that need a nested
                // structure decode again via parse_json_value. Fixes Phase 10a items
                // transport for todo_write (literal-array form).
                result.insert(key.clone(), crate::util::json::json_value_to_string(value));
            }
        }
    }
    Ok(result)
}

fn parse_tool_arguments(input: &str) -> AppResult<BTreeMap<String, String>> {
    let root = parse_root_object(input)?;
    json_object_to_string_args(&JsonValue::Object(root))
}

fn derive_search_query(task: &str) -> Option<String> {
    if let Some(quoted) = first_quoted_segment(task) {
        return Some(quoted);
    }

    if let Some(identifier) = identifier_like_token(task) {
        return Some(identifier);
    }

    for marker in ["search ", "find ", "grep ", "look for "] {
        if let Some(index) = task.find(marker) {
            let value = task[index + marker.len()..]
                .split_whitespace()
                .take(3)
                .collect::<Vec<_>>()
                .join(" ");
            if !value.is_empty() {
                return Some(value);
            }
        }
    }

    None
}

fn identifier_like_token(task: &str) -> Option<String> {
    task.split_whitespace()
        .map(|word| {
            word.trim_matches(|ch: char| {
                !ch.is_ascii_alphanumeric() && ch != '_' && ch != ':' && ch != '-'
            })
        })
        .find(|word| {
            !word.is_empty()
                && (word.contains('_')
                    || word.contains("::")
                    || word.chars().any(|ch| ch.is_ascii_uppercase()))
                && !matches!(*word, "DeepSeek" | "DeepseekCode" | "Cargo" | "README")
        })
        .map(str::to_string)
}

fn first_quoted_segment(task: &str) -> Option<String> {
    quoted_segments(task).into_iter().next()
}

fn wants_validation(task: &str) -> bool {
    ["test", "fix", "validate", "check", "lint"]
        .iter()
        .any(|word| task.contains(word))
}

#[derive(Debug, Clone)]
struct EditRequest {
    path: String,
    find: String,
    replace: String,
}

pub(crate) fn task_has_direct_edit_request(task: &str) -> bool {
    derive_edit_request(task).is_some()
}

fn derive_edit_request(task: &str) -> Option<EditRequest> {
    let task_lower = task.to_lowercase();
    if !task_lower.contains("replace ")
        || !task_lower.contains(" with ")
        || !task_lower.contains(" in ")
    {
        return None;
    }

    let quoted = quoted_segments(task);
    if quoted.len() < 2 {
        return None;
    }
    let replacement_pair = &quoted[quoted.len() - 2..];

    let in_index = task_lower.rfind(" in ")?;
    let path = trim_edit_path_suffix(&task[in_index + 4..])
        .trim()
        .trim_matches('`')
        .trim()
        .to_string();
    if path.is_empty() {
        return None;
    }

    Some(EditRequest {
        path,
        find: replacement_pair[0].clone(),
        replace: replacement_pair[1].clone(),
    })
}

fn trim_edit_path_suffix(raw: &str) -> &str {
    let lowered = raw.to_lowercase();
    let mut cut = raw.len();
    for marker in [
        " and validate ",
        " then validate ",
        " and rerun ",
        " then rerun ",
        " and run ",
        " then run ",
        " and check ",
        " then check ",
    ] {
        if let Some(index) = lowered.find(marker) {
            cut = cut.min(index);
        }
    }
    raw[..cut].trim_end_matches(['.', ',']).trim()
}

fn quoted_segments(task: &str) -> Vec<String> {
    let bytes = task.as_bytes();
    let mut start = None;
    let mut delimiter = None;
    let mut values = Vec::new();

    for (index, byte) in bytes.iter().enumerate() {
        if is_supported_quote_delimiter(bytes, index, *byte) {
            if let (Some(begin), Some(active_delimiter)) = (start, delimiter) {
                if active_delimiter != *byte {
                    continue;
                }
                let segment = task[begin + 1..index].trim();
                if !segment.is_empty() {
                    values.push(segment.to_string());
                }
                start = None;
                delimiter = None;
            } else {
                start = Some(index);
                delimiter = Some(*byte);
            }
        }
    }

    values
}

fn is_supported_quote_delimiter(bytes: &[u8], index: usize, byte: u8) -> bool {
    match byte {
        b'"' | b'`' => true,
        b'\'' => {
            let prev_is_word = index
                .checked_sub(1)
                .and_then(|idx| bytes.get(idx))
                .is_some_and(|ch| ch.is_ascii_alphanumeric() || *ch == b'_');
            let next_is_word = bytes
                .get(index + 1)
                .is_some_and(|ch| ch.is_ascii_alphanumeric() || *ch == b'_');
            !(prev_is_word && next_is_word)
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        api_flavor, build_anthropic_tools, build_openai_tools, child_files_from_summary,
        derive_edit_request, derive_search_query, last_patched_file_path, parse_anthropic_messages,
        parse_anthropic_usage, parse_openai_chat_completion, parse_openai_usage, ApiFlavor,
        DeepSeekClient,
    };
    use crate::config::types::ModelConfig;
    use crate::model::client::ModelClient;
    use crate::model::protocol::{ModelAction, ModelRequest, Observation};
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn detects_anthropic_base_url() {
        assert!(matches!(
            api_flavor("https://api.deepseek.com/anthropic"),
            ApiFlavor::Anthropic
        ));
        assert!(matches!(
            api_flavor("https://api.deepseek.com"),
            ApiFlavor::OpenAi
        ));
    }

    #[test]
    fn parses_openai_tool_call_response() {
        let body = r#"{
            "choices": [
                {
                    "message": {
                        "role": "assistant",
                        "content": null,
                        "tool_calls": [
                            {
                                "id": "call_1",
                                "type": "function",
                                "function": {
                                    "name": "read_file",
                                    "arguments": "{\"path\":\"README.md\",\"max_lines\":\"20\"}"
                                }
                            }
                        ]
                    },
                    "finish_reason": "tool_calls",
                    "index": 0
                }
            ]
        }"#;

        let response = parse_openai_chat_completion(body).unwrap();
        match response.action {
            ModelAction::CallTool { tool_name, input } => {
                assert_eq!(tool_name, "read_file");
                assert_eq!(input.get("path"), Some("README.md"));
                assert_eq!(input.get("max_lines"), Some("20"));
            }
            ModelAction::Finish => panic!("expected tool call"),
        }
    }

    #[test]
    fn builds_openai_tool_specs_for_known_tools() {
        let tools = build_openai_tools(&["read_file".to_string(), "git_diff".to_string()]);
        assert!(tools.contains("\"name\":\"read_file\""));
        assert!(tools.contains("\"name\":\"git_diff\""));
    }

    #[test]
    fn builds_anthropic_tool_specs_for_known_tools() {
        let tools = build_anthropic_tools(&[
            "read_file".to_string(),
            "git_diff".to_string(),
            "apply_patch".to_string(),
        ]);
        assert!(tools.contains("\"name\":\"read_file\""));
        assert!(tools.contains("\"name\":\"git_diff\""));
        assert!(tools.contains("\"name\":\"apply_patch\""));
        assert!(tools.contains("\"input_schema\":"));
        assert!(!tools.contains("\"function\":"));
    }

    #[test]
    fn build_openai_tools_includes_todo_write() {
        let tools = build_openai_tools(&["todo_write".to_string()]);
        assert!(tools.contains("\"name\":\"todo_write\""));
        assert!(tools.contains("\"items\""));
    }

    #[test]
    fn build_anthropic_tools_includes_todo_write() {
        let tools = build_anthropic_tools(&["todo_write".to_string()]);
        assert!(tools.contains("\"name\":\"todo_write\""));
        assert!(tools.contains("\"items\""));
    }

    #[test]
    fn build_openai_tools_includes_dispatch_subagent() {
        let tools = build_openai_tools(&["dispatch_subagent".to_string()]);
        assert!(tools.contains("\"name\":\"dispatch_subagent\""));
        assert!(tools.contains("\"task\""));
        assert!(tools.contains("\"steps\""));
    }

    #[test]
    fn build_anthropic_tools_includes_dispatch_subagent() {
        let tools = build_anthropic_tools(&["dispatch_subagent".to_string()]);
        assert!(tools.contains("\"name\":\"dispatch_subagent\""));
        assert!(tools.contains("\"task\""));
        assert!(tools.contains("\"steps\""));
    }

    fn empty_request_with_todos(todos: Vec<crate::core::todos::Todo>) -> ModelRequest {
        ModelRequest {
            system_prompt: String::new(),
            task: "test".to_string(),
            profile_name: "rust".to_string(),
            profile_hints: Vec::new(),
            primary_file: None,
            suggested_test_command: None,
            available_tools: vec!["todo_write".to_string()],
            observations: Vec::new(),
            todos,
            planning_mode: false,
            recent_steps: Vec::new(),
        }
    }

    #[test]
    fn build_user_prompt_omits_todos_block_when_empty() {
        let prompt = super::build_user_prompt(&empty_request_with_todos(Vec::new()));
        assert!(
            !prompt.contains("Todos:"),
            "expected no Todos: section: {prompt}"
        );
    }

    #[test]
    fn build_user_prompt_requires_validation_after_successful_patch() {
        let mut req = empty_request_with_todos(Vec::new());
        req.suggested_test_command = Some("cargo test".to_string());
        req.available_tools = vec!["run_shell".to_string(), "read_file".to_string()];
        req.observations = vec![
            Observation::ok(
                "apply_patch",
                "Updated src/lib.rs using single replacement mode.",
            ),
            Observation::ok("read_file", "1 pub fn add(a: i32, b: i32) -> i32 {"),
        ];
        let prompt = super::build_user_prompt(&req);
        assert!(
            prompt.contains("Next required action: call run_shell with command `cargo test` now")
        );
        assert!(prompt.contains("do not call read_file or git_diff before validation"));
    }

    #[test]
    fn build_user_prompt_requires_patch_after_reading_direct_edit_target() {
        let mut req = empty_request_with_todos(Vec::new());
        req.task =
            "replace `a - b` with `a + b` in src/lib.rs and validate with cargo test".to_string();
        req.suggested_test_command = Some("cargo test".to_string());
        req.available_tools = vec![
            "apply_patch".to_string(),
            "run_shell".to_string(),
            "read_file".to_string(),
        ];
        req.observations = vec![Observation::ok(
            "read_file",
            "1 pub fn add(a: i32, b: i32) -> i32 {\n2     a - b",
        )];
        let prompt = super::build_user_prompt(&req);
        assert!(prompt.contains(
            "Next required action: call apply_patch with path `src/lib.rs`, find `a - b`, and replace `a + b` now"
        ));
        assert!(prompt.contains("do not call read_file again before patching"));
    }

    #[test]
    fn build_user_prompt_requires_finish_after_successful_validation() {
        let mut req = empty_request_with_todos(Vec::new());
        req.suggested_test_command = Some("cargo test".to_string());
        req.available_tools = vec!["run_shell".to_string(), "read_file".to_string()];
        req.observations = vec![
            Observation::ok(
                "apply_patch",
                "Updated src/lib.rs using single replacement mode.",
            ),
            Observation::ok(
                "run_shell",
                "meta.command_kind=test\nmeta.exit_code=0\nmeta.result=ok\nexit_code: 0",
            ),
        ];
        let prompt = super::build_user_prompt(&req);
        assert!(prompt.contains(
            "Next required action: validation already passed; finish with a concise summary"
        ));
    }

    #[test]
    fn build_user_prompt_renders_recent_steps_block_when_present() {
        let mut req = empty_request_with_todos(Vec::new());
        req.recent_steps = vec![
            "Listing files in src/util".to_string(),
            "Read first 50 lines".to_string(),
        ];
        let prompt = super::build_user_prompt(&req);
        assert!(prompt.contains("Recent agent steps"));
        assert!(prompt.contains("Listing files in src/util"));
        assert!(prompt.contains("Read first 50 lines"));
        // Empty case
        let mut req2 = empty_request_with_todos(Vec::new());
        req2.recent_steps = Vec::new();
        let p2 = super::build_user_prompt(&req2);
        assert!(!p2.contains("Recent agent steps"));
    }

    #[test]
    fn build_user_prompt_renders_todos_in_status_content_format() {
        use crate::core::todos::{Todo, TodoStatus};
        let todos = vec![
            Todo {
                content: "Pen".to_string(),
                active_form: "Penning".to_string(),
                status: TodoStatus::Pending,
            },
            Todo {
                content: "Pro".to_string(),
                active_form: "Proing".to_string(),
                status: TodoStatus::InProgress,
            },
            Todo {
                content: "Don".to_string(),
                active_form: "Doning".to_string(),
                status: TodoStatus::Completed,
            },
        ];
        let prompt = super::build_user_prompt(&empty_request_with_todos(todos));
        assert!(
            prompt.contains("Todos:\n- [pending] Pen\n- [in_progress] Pro\n- [completed] Don\n"),
            "prompt: {prompt}"
        );
        // active_form must NOT leak into the user prompt:
        assert!(!prompt.contains("Penning"));
        assert!(!prompt.contains("Proing"));
        assert!(!prompt.contains("Doning"));
    }

    #[test]
    fn build_user_prompt_renders_execution_plan_and_current_step() {
        use crate::core::todos::{Todo, TodoStatus};
        let mut req = empty_request_with_todos(vec![
            Todo {
                content: "Inspect files".to_string(),
                active_form: "Inspecting files".to_string(),
                status: TodoStatus::Completed,
            },
            Todo {
                content: "Implement fix".to_string(),
                active_form: "Implementing fix".to_string(),
                status: TodoStatus::InProgress,
            },
            Todo {
                content: "Run tests".to_string(),
                active_form: "Running tests".to_string(),
                status: TodoStatus::Pending,
            },
        ]);
        req.planning_mode = true;
        let prompt = super::build_user_prompt(&req);
        assert!(prompt.contains("Execution plan:"));
        assert!(prompt.contains("1. [completed] Inspect files"));
        assert!(prompt.contains("2. [in_progress] Implement fix"));
        assert!(prompt.contains("Current plan step: 2/3 [in_progress] Implement fix"));
    }

    #[test]
    fn build_user_prompt_mentions_missing_plan_when_planning_mode_enabled() {
        let mut req = empty_request_with_todos(Vec::new());
        req.planning_mode = true;
        let prompt = super::build_user_prompt(&req);
        assert!(prompt
            .contains("Current plan step: no plan yet; create one with todo_write before acting."));
    }

    #[test]
    fn deepseek_body_contains_tool_choice_auto() {
        // NEW-2: pin tool_choice="auto" against future PR drift.
        // Inspect the source file directly to confirm the literal is present.
        // We expect at least two matches (format string + this assertion); if
        // the format string is removed we will see exactly one match, which
        // still trips a follow-up safety check below.
        let source = include_str!("deepseek.rs");
        let openai_lit = r#""\"tool_choice\":\"auto\","#;
        let openai_parallel_lit = r#""\"parallel_tool_calls\":false,"#;
        let anthropic_lit = r#""\"tool_choice\":{{\"type\":\"auto\"}},"#;
        let openai_count = source.matches(openai_lit).count();
        let openai_parallel_count = source.matches(openai_parallel_lit).count();
        let anthropic_count = source.matches(anthropic_lit).count();
        assert!(
            openai_count >= 2,
            "OpenAI body must include tool_choice auto (count={openai_count})"
        );
        assert!(
            openai_parallel_count >= 2,
            "OpenAI body must disable parallel tool calls (count={openai_parallel_count})"
        );
        assert!(
            anthropic_count >= 2,
            "Anthropic body must include tool_choice auto (count={anthropic_count})"
        );
    }

    #[test]
    fn parses_anthropic_tool_use_response() {
        let body = r#"{
            "id": "msg_1",
            "type": "message",
            "role": "assistant",
            "content": [
                {"type": "text", "text": "Reading the file."},
                {
                    "type": "tool_use",
                    "id": "tu_1",
                    "name": "read_file",
                    "input": {"path": "README.md", "max_lines": "20"}
                }
            ],
            "stop_reason": "tool_use"
        }"#;

        let response = parse_anthropic_messages(body).unwrap();
        match response.action {
            ModelAction::CallTool { tool_name, input } => {
                assert_eq!(tool_name, "read_file");
                assert_eq!(input.get("path"), Some("README.md"));
                assert_eq!(input.get("max_lines"), Some("20"));
            }
            ModelAction::Finish => panic!("expected tool call"),
        }
        assert_eq!(response.message, "Reading the file.");
    }

    #[test]
    fn parses_anthropic_text_only_response_as_finish() {
        let body = r#"{
            "id": "msg_2",
            "content": [{"type": "text", "text": "All done."}],
            "stop_reason": "end_turn"
        }"#;

        let response = parse_anthropic_messages(body).unwrap();
        assert!(matches!(response.action, ModelAction::Finish));
        assert_eq!(response.message, "All done.");
    }

    #[test]
    fn parses_anthropic_tool_use_with_numeric_input() {
        let body = r#"{
            "content": [
                {
                    "type": "tool_use",
                    "id": "tu_2",
                    "name": "read_file",
                    "input": {"path": "README.md", "max_lines": 20}
                }
            ]
        }"#;

        let response = parse_anthropic_messages(body).unwrap();
        match response.action {
            ModelAction::CallTool { tool_name, input } => {
                assert_eq!(tool_name, "read_file");
                assert_eq!(input.get("max_lines"), Some("20"));
            }
            ModelAction::Finish => panic!("expected tool call"),
        }
    }

    #[test]
    fn anthropic_response_surfaces_api_errors() {
        let body = r#"{"error": {"type": "invalid_request_error", "message": "missing tools"}}"#;
        let error = parse_anthropic_messages(body).unwrap_err();
        assert!(error.to_string().contains("missing tools"));
    }

    #[test]
    fn anthropic_tool_use_wins_over_text_blocks() {
        let body = r#"{
            "content": [
                {"type": "text", "text": "I will do this."},
                {"type": "text", "text": "Now using a tool."},
                {
                    "type": "tool_use",
                    "id": "tu_3",
                    "name": "git_diff",
                    "input": {}
                }
            ]
        }"#;

        let response = parse_anthropic_messages(body).unwrap();
        match response.action {
            ModelAction::CallTool { tool_name, .. } => assert_eq!(tool_name, "git_diff"),
            ModelAction::Finish => panic!("expected tool call"),
        }
        assert!(response.message.contains("I will do this."));
        assert!(response.message.contains("Now using a tool."));
    }

    fn unique_planner_dir() -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        std::env::temp_dir().join(format!("dscode_planner_test_{nanos}"))
    }

    fn planner() -> DeepSeekClient {
        DeepSeekClient {
            config: ModelConfig {
                base_url: "https://api.deepseek.com".to_string(),
                model: "deepseek-coder".to_string(),
                api_key_env: "DSCODE_TEST_NO_KEY".to_string(),
            },
        }
    }

    #[test]
    fn offline_planner_emits_patch_mode_when_possible() {
        let dir = unique_planner_dir();
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("note.txt");
        fs::write(&file, "alpha\nbeta gamma\ndelta\n").unwrap();
        let path = file.to_str().unwrap().to_string();

        let request = ModelRequest {
            system_prompt: String::new(),
            task: format!("replace \"gamma\" with \"GAMMA\" in {path}"),
            profile_name: "generic".to_string(),
            profile_hints: Vec::new(),
            primary_file: None,
            suggested_test_command: None,
            available_tools: vec![
                "apply_patch".to_string(),
                "read_file".to_string(),
                "list_files".to_string(),
            ],
            observations: vec![
                Observation::ok("list_files", "noop"),
                Observation::ok("read_file", "noop"),
            ],
            todos: Vec::new(),
            planning_mode: false,
            recent_steps: Vec::new(),
        };

        let response = planner()
            .respond(request, &mut crate::ui::stream::NoopStreamEvents)
            .unwrap()
            .0;
        match response.action {
            ModelAction::CallTool { tool_name, input } => {
                assert_eq!(tool_name, "apply_patch");
                let patch = input.get("patch").expect("expected patch arg");
                assert!(patch.contains("@@ -2,1 +2,1 @@"), "patch: {patch}");
                assert!(patch.contains("-beta gamma"), "patch: {patch}");
                assert!(patch.contains("+beta GAMMA"), "patch: {patch}");
                assert!(patch.contains("--- note.txt"), "patch: {patch}");
                assert_eq!(input.get("cwd"), Some(dir.to_string_lossy().as_ref()));
                assert!(input.get("find").is_none());
            }
            ModelAction::Finish => panic!("expected tool call"),
        }

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn offline_planner_skips_inspection_when_patch_can_be_built_directly() {
        let dir = unique_planner_dir();
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("note.txt");
        fs::write(&file, "alpha\nbeta gamma\ndelta\n").unwrap();
        let path = file.to_str().unwrap().to_string();

        let request = ModelRequest {
            system_prompt: String::new(),
            task: format!("replace \"gamma\" with \"GAMMA\" in {path}"),
            profile_name: "generic".to_string(),
            profile_hints: Vec::new(),
            primary_file: None,
            suggested_test_command: None,
            available_tools: vec![
                "apply_patch".to_string(),
                "read_file".to_string(),
                "list_files".to_string(),
            ],
            observations: vec![],
            todos: Vec::new(),
            planning_mode: false,
            recent_steps: Vec::new(),
        };

        let response = planner()
            .respond(request, &mut crate::ui::stream::NoopStreamEvents)
            .unwrap()
            .0;
        match response.action {
            ModelAction::CallTool { tool_name, input } => {
                assert_eq!(tool_name, "apply_patch");
                assert!(input.get("patch").is_some(), "expected patch-mode shortcut");
            }
            ModelAction::Finish => panic!("expected tool call"),
        }
        assert!(
            response.message.contains("skipped inspection"),
            "message: {}",
            response.message
        );

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn offline_planner_falls_back_to_text_replace_when_patch_unavailable() {
        let dir = unique_planner_dir();
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("dup.txt");
        fs::write(&file, "alpha\nalpha\n").unwrap();
        let path = file.to_str().unwrap().to_string();

        let request = ModelRequest {
            system_prompt: String::new(),
            task: format!("replace \"alpha\" with \"ALPHA\" in {path}"),
            profile_name: "generic".to_string(),
            profile_hints: Vec::new(),
            primary_file: None,
            suggested_test_command: None,
            available_tools: vec![
                "apply_patch".to_string(),
                "read_file".to_string(),
                "list_files".to_string(),
            ],
            observations: vec![
                Observation::ok("list_files", "noop"),
                Observation::ok("read_file", "noop"),
            ],
            todos: Vec::new(),
            planning_mode: false,
            recent_steps: Vec::new(),
        };

        let response = planner()
            .respond(request, &mut crate::ui::stream::NoopStreamEvents)
            .unwrap()
            .0;
        match response.action {
            ModelAction::CallTool { tool_name, input } => {
                assert_eq!(tool_name, "apply_patch");
                assert!(input.get("patch").is_none());
                assert_eq!(input.get("find"), Some("alpha"));
                assert_eq!(input.get("replace"), Some("ALPHA"));
            }
            ModelAction::Finish => panic!("expected tool call"),
        }

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn offline_planner_retries_with_text_replace_after_patch_failure() {
        let dir = unique_planner_dir();
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("note.txt");
        fs::write(&file, "alpha\nbeta gamma\ndelta\n").unwrap();
        let path = file.to_str().unwrap().to_string();

        let request = ModelRequest {
            system_prompt: String::new(),
            task: format!("replace \"gamma\" with \"GAMMA\" in {path}"),
            profile_name: "generic".to_string(),
            profile_hints: Vec::new(),
            primary_file: None,
            suggested_test_command: None,
            available_tools: vec!["apply_patch".to_string(), "read_file".to_string(), "list_files".to_string()],
            observations: vec![
                Observation::ok("list_files", "noop"),
                Observation::ok("read_file", "noop"),
                Observation::failed(
                    "apply_patch",
                    "patch dry-run failed: hunk #1 did not match the target file (the surrounding context drifted)",
                ),
            ],
            todos: Vec::new(),
            planning_mode: false,
            recent_steps: Vec::new(),
        };

        let response = planner()
            .respond(request, &mut crate::ui::stream::NoopStreamEvents)
            .unwrap()
            .0;
        match response.action {
            ModelAction::CallTool { tool_name, input } => {
                assert_eq!(tool_name, "apply_patch");
                assert!(input.get("patch").is_none(), "expected text-replace retry");
                assert_eq!(input.get("find"), Some("gamma"));
                assert_eq!(input.get("replace"), Some("GAMMA"));
            }
            ModelAction::Finish => panic!("expected retry tool call"),
        }

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn offline_planner_skips_git_diff_when_apply_patch_failed() {
        let dir = unique_planner_dir();
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("note.txt");
        fs::write(&file, "alpha\nbeta\n").unwrap();
        let path = file.to_str().unwrap().to_string();

        let request = ModelRequest {
            system_prompt: String::new(),
            task: format!("replace \"missing\" with \"x\" in {path}"),
            profile_name: "generic".to_string(),
            profile_hints: Vec::new(),
            primary_file: None,
            suggested_test_command: None,
            available_tools: vec![
                "apply_patch".to_string(),
                "read_file".to_string(),
                "list_files".to_string(),
                "git_diff".to_string(),
            ],
            observations: vec![
                Observation::ok("list_files", "noop"),
                Observation::ok("read_file", "noop"),
                Observation::failed("apply_patch", "apply_patch requires a path"),
            ],
            todos: Vec::new(),
            planning_mode: false,
            recent_steps: Vec::new(),
        };

        let response = planner()
            .respond(request, &mut crate::ui::stream::NoopStreamEvents)
            .unwrap()
            .0;
        match response.action {
            ModelAction::CallTool { tool_name, .. } => {
                assert_ne!(
                    tool_name, "git_diff",
                    "git_diff should not run after a failed apply_patch"
                );
            }
            ModelAction::Finish => {}
        }

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn derive_edit_request_ignores_trailing_validation_clause() {
        let request = derive_edit_request(
            "replace \"a - b\" with \"a + b\" in src/lib.rs and validate with cargo test",
        )
        .expect("expected edit request");
        assert_eq!(request.path, "src/lib.rs");
        assert_eq!(request.find, "a - b");
        assert_eq!(request.replace, "a + b");
    }

    #[test]
    fn derive_edit_request_supports_backtick_quoted_segments() {
        let request = derive_edit_request(
            "replace `a - b` with `a + b` in src/lib.rs and validate with cargo test",
        )
        .expect("expected edit request");
        assert_eq!(request.path, "src/lib.rs");
        assert_eq!(request.find, "a - b");
        assert_eq!(request.replace, "a + b");
    }

    #[test]
    fn derive_edit_request_ignores_trailing_rerun_clause() {
        let request = derive_edit_request(
            "CI job `test-js` failed. Replace `run bench` with `run benchmark` in src/index.js and rerun npm test.",
        )
        .expect("expected edit request");
        assert_eq!(request.path, "src/index.js");
        assert_eq!(request.find, "run bench");
        assert_eq!(request.replace, "run benchmark");
    }

    #[test]
    fn reproduce_then_explicit_edit_continues_into_apply_patch() {
        let request = ModelRequest {
            system_prompt: String::new(),
            task: "CI job `test-rust` failed. Reproduce locally, replace `route_bench_subcommand()` with `route_benchmark_subcommand()` in src/cli/app.rs, and rerun cargo test.".to_string(),
            profile_name: "rust".to_string(),
            profile_hints: Vec::new(),
            primary_file: Some("src/cli/app.rs".to_string()),
            suggested_test_command: Some("cargo test".to_string()),
            available_tools: vec![
                "run_shell".to_string(),
                "read_file".to_string(),
                "apply_patch".to_string(),
                "git_diff".to_string(),
            ],
            observations: vec![
                Observation::ok(
                    "run_shell",
                    "meta.command_kind=test\nmeta.exit_code=101\nmeta.result=failed\nmeta.failure_kind=test_failure\nmeta.stderr_summary=cannot find function `route_bench_subcommand` in this scope\nexit_code: 101\nstderr:\ncannot find function `route_bench_subcommand` in this scope",
                ),
                Observation::ok(
                    "read_file",
                    "1 pub fn cli_from_argv(args: &[String]) -> &'static str {\n2     match args.first().map(String::as_str) {\n3         Some(\"benchmark\") => route_bench_subcommand(),",
                ),
            ],
            todos: Vec::new(),
            planning_mode: false,
            recent_steps: Vec::new(),
        };

        let response = planner()
            .respond(request, &mut crate::ui::stream::NoopStreamEvents)
            .unwrap()
            .0;
        match response.action {
            ModelAction::CallTool { tool_name, input } => {
                assert_eq!(tool_name, "apply_patch");
                assert_eq!(input.get("path"), Some("src/cli/app.rs"));
                assert_eq!(input.get("find"), Some("route_bench_subcommand()"));
                assert_eq!(input.get("replace"), Some("route_benchmark_subcommand()"));
            }
            ModelAction::Finish => panic!("expected apply_patch after repro + readback"),
        }
    }

    #[test]
    fn reproduce_then_explicit_edit_reruns_validation_after_patch_diff() {
        let request = ModelRequest {
            system_prompt: String::new(),
            task: "CI job `test-rust` failed. Reproduce locally, replace `route_bench_subcommand()` with `route_benchmark_subcommand()` in src/cli/app.rs, and rerun cargo test.".to_string(),
            profile_name: "rust".to_string(),
            profile_hints: Vec::new(),
            primary_file: Some("src/cli/app.rs".to_string()),
            suggested_test_command: Some("cargo test".to_string()),
            available_tools: vec!["run_shell".to_string()],
            observations: vec![
                Observation::ok(
                    "run_shell",
                    "meta.command_kind=test\nmeta.exit_code=101\nmeta.result=failed\nmeta.failure_kind=test_failure\nmeta.stderr_summary=cannot find function `route_bench_subcommand` in this scope",
                ),
                Observation::ok(
                    "read_file",
                    "1 pub fn cli_from_argv(args: &[String]) -> &'static str {\n2     Some(\"benchmark\") => route_bench_subcommand(),",
                ),
                Observation::ok("apply_patch", "patched src/cli/app.rs"),
                Observation::ok("git_diff", "diff --git a/src/cli/app.rs b/src/cli/app.rs"),
            ],
            todos: Vec::new(),
            planning_mode: false,
            recent_steps: Vec::new(),
        };

        let response = planner()
            .respond(request, &mut crate::ui::stream::NoopStreamEvents)
            .unwrap()
            .0;
        match response.action {
            ModelAction::CallTool { tool_name, input } => {
                assert_eq!(tool_name, "run_shell");
                assert_eq!(input.get("command"), Some("cargo test"));
            }
            ModelAction::Finish => panic!("expected validation rerun after patch diff"),
        }
    }

    #[test]
    fn derive_search_query_reads_single_quoted_pr_title() {
        let query = derive_search_query(
            "Address review feedback or apply the requested change in PR #42 'Route benchmark command'. PR diff is the current head; propose minimal additional changes.",
        )
        .expect("expected query");
        assert_eq!(query, "Route benchmark command");
    }

    #[test]
    fn offline_planner_creates_initial_todo_plan_in_planning_mode() {
        let request = ModelRequest {
            system_prompt: String::new(),
            task: "implement the feature and verify the tests still pass".to_string(),
            profile_name: "generic".to_string(),
            profile_hints: Vec::new(),
            primary_file: None,
            suggested_test_command: Some("cargo test".to_string()),
            available_tools: vec![
                "todo_write".to_string(),
                "list_files".to_string(),
                "read_file".to_string(),
                "apply_patch".to_string(),
                "run_shell".to_string(),
                "git_diff".to_string(),
            ],
            observations: Vec::new(),
            todos: Vec::new(),
            planning_mode: true,
            recent_steps: Vec::new(),
        };

        let response = planner()
            .respond(request, &mut crate::ui::stream::NoopStreamEvents)
            .unwrap()
            .0;
        match response.action {
            ModelAction::CallTool { tool_name, input } => {
                assert_eq!(tool_name, "todo_write");
                let items = input.get("items").expect("expected items arg");
                assert!(
                    items.contains("\"status\":\"in_progress\""),
                    "items: {items}"
                );
                assert!(
                    items.contains("Implement the requested changes"),
                    "items: {items}"
                );
                assert!(
                    items.contains("Validate with `cargo test`"),
                    "items: {items}"
                );
            }
            ModelAction::Finish => panic!("expected planning tool call"),
        }
    }

    #[test]
    fn offline_planner_lookup_plan_does_not_add_validation_step() {
        let request = ModelRequest {
            system_prompt: String::new(),
            task: "find where `route_benchmark_subcommand` is implemented and inspect the code"
                .to_string(),
            profile_name: "rust".to_string(),
            profile_hints: Vec::new(),
            primary_file: Some("Cargo.toml".to_string()),
            suggested_test_command: Some("cargo test".to_string()),
            available_tools: vec![
                "todo_write".to_string(),
                "search_text".to_string(),
                "read_file".to_string(),
                "apply_patch".to_string(),
                "run_shell".to_string(),
            ],
            observations: Vec::new(),
            todos: Vec::new(),
            planning_mode: true,
            recent_steps: Vec::new(),
        };

        let response = planner()
            .respond(request, &mut crate::ui::stream::NoopStreamEvents)
            .unwrap()
            .0;
        match response.action {
            ModelAction::CallTool { tool_name, input } => {
                assert_eq!(tool_name, "todo_write");
                let items = input.get("items").expect("expected items arg");
                assert!(
                    items.contains("Search for the relevant code paths"),
                    "items: {items}"
                );
                assert!(
                    !items.contains("Validate with `cargo test`"),
                    "items: {items}"
                );
            }
            ModelAction::Finish => panic!("expected planning tool call"),
        }
    }

    #[test]
    fn offline_planner_skips_initial_todo_plan_when_planning_mode_disabled() {
        let request = ModelRequest {
            system_prompt: String::new(),
            task: "inspect repository".to_string(),
            profile_name: "generic".to_string(),
            profile_hints: Vec::new(),
            primary_file: None,
            suggested_test_command: None,
            available_tools: vec![
                "todo_write".to_string(),
                "list_files".to_string(),
                "read_file".to_string(),
            ],
            observations: Vec::new(),
            todos: Vec::new(),
            planning_mode: false,
            recent_steps: Vec::new(),
        };

        let response = planner()
            .respond(request, &mut crate::ui::stream::NoopStreamEvents)
            .unwrap()
            .0;
        match response.action {
            ModelAction::CallTool { tool_name, .. } => assert_eq!(tool_name, "list_files"),
            ModelAction::Finish => panic!("expected list_files tool call"),
        }
    }

    #[test]
    fn offline_planner_dispatches_subagent_for_complex_planned_exploration() {
        let request = ModelRequest {
            system_prompt: String::new(),
            task: "inspect the repo and implement the feature across multiple files".to_string(),
            profile_name: "generic".to_string(),
            profile_hints: Vec::new(),
            primary_file: None,
            suggested_test_command: Some("cargo test".to_string()),
            available_tools: vec![
                "todo_write".to_string(),
                "dispatch_subagent".to_string(),
                "list_files".to_string(),
                "read_file".to_string(),
                "apply_patch".to_string(),
            ],
            observations: Vec::new(),
            todos: vec![
                crate::core::todos::Todo {
                    content: "Inspect the repository layout".to_string(),
                    active_form: "Inspecting the repository layout".to_string(),
                    status: crate::core::todos::TodoStatus::InProgress,
                },
                crate::core::todos::Todo {
                    content: "Read the most relevant files".to_string(),
                    active_form: "Reading the most relevant files".to_string(),
                    status: crate::core::todos::TodoStatus::Pending,
                },
                crate::core::todos::Todo {
                    content: "Implement the requested changes".to_string(),
                    active_form: "Implementing the requested changes".to_string(),
                    status: crate::core::todos::TodoStatus::Pending,
                },
                crate::core::todos::Todo {
                    content: "Review the resulting diff".to_string(),
                    active_form: "Reviewing the resulting diff".to_string(),
                    status: crate::core::todos::TodoStatus::Pending,
                },
            ],
            planning_mode: true,
            recent_steps: Vec::new(),
        };

        let response = planner()
            .respond(request, &mut crate::ui::stream::NoopStreamEvents)
            .unwrap()
            .0;
        match response.action {
            ModelAction::CallTool { tool_name, input } => {
                assert_eq!(tool_name, "dispatch_subagent");
                let task = input.get("task").expect("expected task arg");
                assert!(task.contains("Delegated todo step:"), "task: {task}");
                assert!(
                    task.contains("Inspect the repository layout"),
                    "task: {task}"
                );
                assert!(task.contains("Parent task:"), "task: {task}");
                assert_eq!(input.get("steps"), Some("2"));
            }
            ModelAction::Finish => panic!("expected dispatch_subagent tool call"),
        }
    }

    #[test]
    fn offline_planner_does_not_dispatch_subagent_twice_for_same_plan() {
        let request = ModelRequest {
            system_prompt: String::new(),
            task: "inspect the repo and implement the feature across multiple files".to_string(),
            profile_name: "generic".to_string(),
            profile_hints: Vec::new(),
            primary_file: None,
            suggested_test_command: None,
            available_tools: vec![
                "todo_write".to_string(),
                "dispatch_subagent".to_string(),
                "list_files".to_string(),
                "read_file".to_string(),
            ],
            observations: vec![Observation::ok(
                "dispatch_subagent",
                "subagent finished task `Inspect the repository layout`",
            )],
            todos: vec![
                crate::core::todos::Todo {
                    content: "Inspect the repository layout".to_string(),
                    active_form: "Inspecting the repository layout".to_string(),
                    status: crate::core::todos::TodoStatus::InProgress,
                },
                crate::core::todos::Todo {
                    content: "Read the most relevant files".to_string(),
                    active_form: "Reading the most relevant files".to_string(),
                    status: crate::core::todos::TodoStatus::Pending,
                },
                crate::core::todos::Todo {
                    content: "Implement the requested changes".to_string(),
                    active_form: "Implementing the requested changes".to_string(),
                    status: crate::core::todos::TodoStatus::Pending,
                },
                crate::core::todos::Todo {
                    content: "Review the resulting diff".to_string(),
                    active_form: "Reviewing the resulting diff".to_string(),
                    status: crate::core::todos::TodoStatus::Pending,
                },
            ],
            planning_mode: true,
            recent_steps: Vec::new(),
        };

        let response = planner()
            .respond(request, &mut crate::ui::stream::NoopStreamEvents)
            .unwrap()
            .0;
        match response.action {
            ModelAction::CallTool { tool_name, .. } => {
                assert_ne!(tool_name, "dispatch_subagent");
            }
            ModelAction::Finish => panic!("expected the planner to keep working locally"),
        }
    }

    #[test]
    fn offline_planner_plan_only_task_stops_after_todo_write_exists() {
        let request = ModelRequest {
            system_prompt: String::new(),
            task: "plan an end-to-end improvement for benchmark reliability and report the execution steps before acting".to_string(),
            profile_name: "generic".to_string(),
            profile_hints: Vec::new(),
            primary_file: Some("Cargo.toml".to_string()),
            suggested_test_command: Some("cargo test".to_string()),
            available_tools: vec![
                "todo_write".to_string(),
                "list_files".to_string(),
                "read_file".to_string(),
                "dispatch_subagent".to_string(),
            ],
            observations: vec![Observation::ok("todo_write", "plan created")],
            todos: vec![
                crate::core::todos::Todo {
                    content: "Search for the relevant code paths".to_string(),
                    active_form: "Searching for the relevant code paths".to_string(),
                    status: crate::core::todos::TodoStatus::InProgress,
                },
                crate::core::todos::Todo {
                    content: "Report the planned execution steps".to_string(),
                    active_form: "Reporting the planned execution steps".to_string(),
                    status: crate::core::todos::TodoStatus::Pending,
                },
            ],
            planning_mode: true,
            recent_steps: Vec::new(),
        };

        let response = planner()
            .respond(request, &mut crate::ui::stream::NoopStreamEvents)
            .unwrap()
            .0;
        assert!(matches!(response.action, ModelAction::Finish));
        assert!(response.message.contains("stopping before acting"));
    }

    #[test]
    fn offline_planner_lookup_task_searches_before_listing_or_dispatch() {
        let request = ModelRequest {
            system_prompt: String::new(),
            task: "find where dispatch_subagent is implemented and summarize how parent and child loops coordinate".to_string(),
            profile_name: "generic".to_string(),
            profile_hints: Vec::new(),
            primary_file: Some("Cargo.toml".to_string()),
            suggested_test_command: None,
            available_tools: vec![
                "todo_write".to_string(),
                "dispatch_subagent".to_string(),
                "list_files".to_string(),
                "read_file".to_string(),
                "search_text".to_string(),
            ],
            observations: vec![Observation::ok("todo_write", "plan created")],
            todos: vec![
                crate::core::todos::Todo {
                    content: "Search for the relevant code paths".to_string(),
                    active_form: "Searching for the relevant code paths".to_string(),
                    status: crate::core::todos::TodoStatus::InProgress,
                },
                crate::core::todos::Todo {
                    content: "Read the most relevant files".to_string(),
                    active_form: "Reading the most relevant files".to_string(),
                    status: crate::core::todos::TodoStatus::Pending,
                },
            ],
            planning_mode: true,
            recent_steps: Vec::new(),
        };

        let response = planner()
            .respond(request, &mut crate::ui::stream::NoopStreamEvents)
            .unwrap()
            .0;
        match response.action {
            ModelAction::CallTool { tool_name, input } => {
                assert_eq!(tool_name, "search_text");
                assert_eq!(input.get("query"), Some("dispatch_subagent"));
            }
            ModelAction::Finish => panic!("expected search_text tool call"),
        }
    }

    #[test]
    fn offline_planner_reads_first_search_match_before_primary_file() {
        let request = ModelRequest {
            system_prompt: String::new(),
            task: "find where dispatch_subagent is implemented and inspect the code".to_string(),
            profile_name: "generic".to_string(),
            profile_hints: Vec::new(),
            primary_file: Some("Cargo.toml".to_string()),
            suggested_test_command: None,
            available_tools: vec![
                "search_text".to_string(),
                "read_file".to_string(),
                "list_files".to_string(),
            ],
            observations: vec![Observation::ok(
                "search_text",
                "src/tools/dispatch_subagent.rs:12: pub struct DispatchSubagentTool {",
            )],
            todos: Vec::new(),
            planning_mode: false,
            recent_steps: Vec::new(),
        };

        let response = planner()
            .respond(request, &mut crate::ui::stream::NoopStreamEvents)
            .unwrap()
            .0;
        match response.action {
            ModelAction::CallTool { tool_name, input } => {
                assert_eq!(tool_name, "read_file");
                assert_eq!(input.get("path"), Some("src/tools/dispatch_subagent.rs"));
            }
            ModelAction::Finish => panic!("expected read_file tool call"),
        }
    }

    #[test]
    fn offline_planner_reads_child_file_after_dispatch_subagent_summary() {
        let request = ModelRequest {
            system_prompt: String::new(),
            task: "inspect the repository layout and continue from the subagent findings".to_string(),
            profile_name: "generic".to_string(),
            profile_hints: Vec::new(),
            primary_file: Some("Cargo.toml".to_string()),
            suggested_test_command: None,
            available_tools: vec![
                "read_file".to_string(),
                "list_files".to_string(),
                "search_text".to_string(),
            ],
            observations: vec![Observation::ok(
                "dispatch_subagent",
                "meta.child_outcome=ok\nmeta.child_files=src/cli/app.rs,src/main.rs\nmeta.child_final_message=read the CLI routing file",
            )],
            todos: Vec::new(),
            planning_mode: false,
            recent_steps: Vec::new(),
        };

        let response = planner()
            .respond(request, &mut crate::ui::stream::NoopStreamEvents)
            .unwrap()
            .0;
        match response.action {
            ModelAction::CallTool { tool_name, input } => {
                assert_eq!(tool_name, "read_file");
                assert_eq!(input.get("path"), Some("src/cli/app.rs"));
            }
            ModelAction::Finish => panic!("expected read_file tool call"),
        }
    }

    #[test]
    fn offline_planner_reads_next_child_file_after_first_child_file_was_read() {
        let request = ModelRequest {
            system_prompt: String::new(),
            task: "inspect the repository layout and continue from the subagent findings".to_string(),
            profile_name: "generic".to_string(),
            profile_hints: Vec::new(),
            primary_file: Some("Cargo.toml".to_string()),
            suggested_test_command: None,
            available_tools: vec![
                "read_file".to_string(),
                "list_files".to_string(),
                "search_text".to_string(),
            ],
            observations: vec![
                Observation::ok(
                    "dispatch_subagent",
                    "meta.child_outcome=ok\nmeta.child_files=src/cli/app.rs,src/main.rs\nmeta.child_final_message=read the CLI routing files",
                ),
                Observation::ok("read_file", "   1 use std::env;"),
            ],
            todos: Vec::new(),
            planning_mode: false,
            recent_steps: Vec::new(),
        };

        let response = planner()
            .respond(request, &mut crate::ui::stream::NoopStreamEvents)
            .unwrap()
            .0;
        match response.action {
            ModelAction::CallTool { tool_name, input } => {
                assert_eq!(tool_name, "read_file");
                assert_eq!(input.get("path"), Some("src/main.rs"));
            }
            ModelAction::Finish => panic!("expected read_file tool call"),
        }
    }

    #[test]
    fn offline_planner_reads_next_child_file_for_pr_workflow_without_subagent_wording() {
        let request = ModelRequest {
            system_prompt: String::new(),
            task: "Review pull request #42 'Route benchmark command'. Inspect the touched files before summarizing risks.".to_string(),
            profile_name: "rust".to_string(),
            profile_hints: Vec::new(),
            primary_file: None,
            suggested_test_command: None,
            available_tools: vec![
                "read_file".to_string(),
                "list_files".to_string(),
                "search_text".to_string(),
            ],
            observations: vec![
                Observation::ok(
                    "dispatch_subagent",
                    "meta.child_outcome=ok\nmeta.child_files=src/cli/app.rs,src/main.rs\nmeta.child_final_message=read the CLI routing files",
                ),
                Observation::ok("read_file", "   1 pub fn cli_from_argv(args: &[String]) -> &'static str {"),
            ],
            todos: Vec::new(),
            planning_mode: false,
            recent_steps: Vec::new(),
        };

        let response = planner()
            .respond(request, &mut crate::ui::stream::NoopStreamEvents)
            .unwrap()
            .0;
        match response.action {
            ModelAction::CallTool { tool_name, input } => {
                assert_eq!(tool_name, "read_file");
                assert_eq!(input.get("path"), Some("src/main.rs"));
            }
            ModelAction::Finish => panic!("expected read_file tool call"),
        }
    }

    #[test]
    fn offline_planner_finishes_after_pr_workflow_child_files_are_consumed() {
        let request = ModelRequest {
            system_prompt: String::new(),
            task: "Review pull request #42 'Route benchmark command'. Inspect the touched files before summarizing risks.".to_string(),
            profile_name: "rust".to_string(),
            profile_hints: Vec::new(),
            primary_file: None,
            suggested_test_command: None,
            available_tools: vec![
                "read_file".to_string(),
                "list_files".to_string(),
                "search_text".to_string(),
            ],
            observations: vec![
                Observation::ok(
                    "dispatch_subagent",
                    "meta.child_outcome=ok\nmeta.child_files=src/cli/app.rs,src/main.rs\nmeta.child_final_message=read the CLI routing files",
                ),
                Observation::ok(
                    "read_file",
                    "   1 pub fn cli_from_argv(args: &[String]) -> &'static str {",
                ),
                Observation::ok("read_file", "   1 fn main() {"),
            ],
            todos: Vec::new(),
            planning_mode: false,
            recent_steps: Vec::new(),
        };

        let response = planner()
            .respond(request, &mut crate::ui::stream::NoopStreamEvents)
            .unwrap()
            .0;
        assert!(
            matches!(response.action, ModelAction::Finish),
            "expected finish after child files are consumed, got {:?}",
            response.action
        );
    }

    #[test]
    fn offline_planner_prefers_changed_file_read_for_pr_patch_task() {
        let request = ModelRequest {
            system_prompt: String::new(),
            task: "Address review feedback or apply the requested change in PR #42 'Route benchmark command'. PR diff is the current head; propose minimal additional changes.".to_string(),
            profile_name: "rust".to_string(),
            profile_hints: Vec::new(),
            primary_file: None,
            suggested_test_command: None,
            available_tools: vec![
                "todo_write".to_string(),
                "dispatch_subagent".to_string(),
                "search_text".to_string(),
                "read_file".to_string(),
                "list_files".to_string(),
            ],
            observations: vec![
                Observation::ok(
                    "git_diff",
                    "diff --git a/src/cli/app.rs b/src/cli/app.rs\n@@\n-        Some(\"benchmark\") => route_benchmark_subcommand(),\n+        Some(\"bench\") => route_benchmark_subcommand(),",
                ),
                Observation::ok("list_files", "src/cli/app.rs\nsrc/main.rs"),
            ],
            todos: Vec::new(),
            planning_mode: false,
            recent_steps: Vec::new(),
        };

        let response = planner()
            .respond(request, &mut crate::ui::stream::NoopStreamEvents)
            .unwrap()
            .0;
        match response.action {
            ModelAction::CallTool { tool_name, input } => {
                assert_eq!(tool_name, "read_file");
                assert_eq!(input.get("path"), Some("src/cli/app.rs"));
            }
            ModelAction::Finish => panic!("expected read_file tool call"),
        }
    }

    #[test]
    fn offline_planner_does_not_search_pr_title_after_changed_file_read() {
        let request = ModelRequest {
            system_prompt: String::new(),
            task: "Address review feedback or apply the requested change in PR #42 'Route benchmark command'. PR diff is the current head; propose minimal additional changes.".to_string(),
            profile_name: "rust".to_string(),
            profile_hints: Vec::new(),
            primary_file: None,
            suggested_test_command: None,
            available_tools: vec!["search_text".to_string(), "read_file".to_string()],
            observations: vec![
                Observation::ok(
                    "git_diff",
                    "diff --git a/src/cli/app.rs b/src/cli/app.rs\n@@\n-        Some(\"benchmark\") => route_benchmark_subcommand(),\n+        Some(\"bench\") => route_benchmark_subcommand(),",
                ),
                Observation::ok("list_files", "src/cli/app.rs\nsrc/main.rs"),
                Observation::ok("read_file", "   1 pub fn cli_from_argv(args: &[String]) -> &'static str {"),
            ],
            todos: Vec::new(),
            planning_mode: false,
            recent_steps: Vec::new(),
        };

        let response = planner()
            .respond(request, &mut crate::ui::stream::NoopStreamEvents)
            .unwrap()
            .0;
        assert!(matches!(response.action, ModelAction::Finish));
    }

    #[test]
    fn offline_planner_skips_initial_todo_write_for_pr_patch_with_diff_context() {
        let request = ModelRequest {
            system_prompt: String::new(),
            task: "Address review feedback or apply the requested change in PR #42 'Route benchmark command'. PR diff is the current head; propose minimal additional changes.".to_string(),
            profile_name: "rust".to_string(),
            profile_hints: Vec::new(),
            primary_file: None,
            suggested_test_command: None,
            available_tools: vec![
                "todo_write".to_string(),
                "dispatch_subagent".to_string(),
                "search_text".to_string(),
                "read_file".to_string(),
                "list_files".to_string(),
            ],
            observations: vec![
                Observation::ok(
                    "git_diff",
                    "diff --git a/src/cli/app.rs b/src/cli/app.rs\n@@\n-        Some(\"benchmark\") => route_benchmark_subcommand(),\n+        Some(\"bench\") => route_benchmark_subcommand(),",
                ),
                Observation::ok("list_files", "src/cli/app.rs\nsrc/main.rs"),
            ],
            todos: Vec::new(),
            planning_mode: false,
            recent_steps: Vec::new(),
        };

        let response = planner()
            .respond(request, &mut crate::ui::stream::NoopStreamEvents)
            .unwrap()
            .0;
        match response.action {
            ModelAction::CallTool { tool_name, input } => {
                assert_eq!(tool_name, "read_file");
                assert_eq!(input.get("path"), Some("src/cli/app.rs"));
            }
            ModelAction::Finish => panic!("expected read_file tool call"),
        }
    }

    #[test]
    fn offline_planner_finishes_after_targeted_pr_fix_readback() {
        let request = ModelRequest {
            system_prompt: String::new(),
            task: "CI job `test-rust` (run #555) on PR #42 failed at step `cargo test`. Reproduce locally, fix the root cause, and rerun the failing test. Failed log tail follows.".to_string(),
            profile_name: "rust".to_string(),
            profile_hints: Vec::new(),
            primary_file: None,
            suggested_test_command: None,
            available_tools: vec![
                "todo_write".to_string(),
                "search_text".to_string(),
                "read_file".to_string(),
                "list_files".to_string(),
            ],
            observations: vec![
                Observation::ok(
                    "run_shell",
                    "meta.command_kind=test\nmeta.exit_code=101\nmeta.result=failed\nmeta.failure_kind=test_failure\nmeta.failed_tests=src/cli/app.rs::cli_from_argv\nmeta.stderr_summary=test failed\nexit_code: 101\nstderr:\ntest failed",
                ),
                Observation::ok(
                    "recovery_hint",
                    "after=run_shell; next=read_file; path=src/cli/app.rs; reason=run_shell reported failing tests (src/cli/app.rs::cli_from_argv), inspect the relevant code or diff before retrying the command",
                ),
                Observation::ok("read_file", "   1 pub fn cli_from_argv(args: &[String]) -> &'static str {"),
            ],
            todos: Vec::new(),
            planning_mode: false,
            recent_steps: Vec::new(),
        };

        let response = planner()
            .respond(request, &mut crate::ui::stream::NoopStreamEvents)
            .unwrap()
            .0;
        assert!(matches!(response.action, ModelAction::Finish));
    }

    #[test]
    fn offline_planner_uses_child_final_message_query_when_task_has_no_query() {
        let request = ModelRequest {
            system_prompt: String::new(),
            task: "investigate the code path the subagent just found".to_string(),
            profile_name: "generic".to_string(),
            profile_hints: Vec::new(),
            primary_file: None,
            suggested_test_command: None,
            available_tools: vec![
                "search_text".to_string(),
                "read_file".to_string(),
            ],
            observations: vec![Observation::ok(
                "dispatch_subagent",
                "meta.child_outcome=ok\nmeta.child_final_message=deepseek-v4-pro planner is searching for `route_benchmark_subcommand`.",
            )],
            todos: Vec::new(),
            planning_mode: false,
            recent_steps: Vec::new(),
        };

        let response = planner()
            .respond(request, &mut crate::ui::stream::NoopStreamEvents)
            .unwrap()
            .0;
        match response.action {
            ModelAction::CallTool { tool_name, input } => {
                assert_eq!(tool_name, "search_text");
                assert_eq!(input.get("query"), Some("route_benchmark_subcommand"));
            }
            ModelAction::Finish => panic!("expected search_text tool call"),
        }
    }

    #[test]
    fn offline_planner_does_not_jump_back_to_search_after_child_file_was_already_read() {
        let request = ModelRequest {
            system_prompt: String::new(),
            task: "inspect the repository layout and continue from the subagent findings".to_string(),
            profile_name: "generic".to_string(),
            profile_hints: Vec::new(),
            primary_file: Some("Cargo.toml".to_string()),
            suggested_test_command: None,
            available_tools: vec![
                "read_file".to_string(),
                "list_files".to_string(),
                "search_text".to_string(),
            ],
            observations: vec![
                Observation::ok(
                    "dispatch_subagent",
                    "meta.child_outcome=ok\nmeta.child_files=src/cli/app.rs,src/main.rs\nmeta.child_final_message=search for `route_benchmark_subcommand`",
                ),
                Observation::ok(
                    "read_file",
                    "src/cli/app.rs:1: use crate::cli::app::Command;",
                ),
            ],
            todos: Vec::new(),
            planning_mode: false,
            recent_steps: Vec::new(),
        };

        let response = planner()
            .respond(request, &mut crate::ui::stream::NoopStreamEvents)
            .unwrap()
            .0;
        match response.action {
            ModelAction::CallTool { tool_name, input } => {
                assert_ne!(tool_name, "search_text");
                if tool_name == "read_file" {
                    assert_eq!(input.get("path"), Some("src/main.rs"));
                }
            }
            ModelAction::Finish => panic!("expected follow-up tool call"),
        }
    }

    #[test]
    fn child_file_paths_fallback_to_child_final_message() {
        let files = child_files_from_summary(
            "meta.child_outcome=ok\nmeta.child_final_message=main entrypoints are in `src/main.rs` and `src/cli/app.rs`",
        );
        assert_eq!(
            files,
            vec!["src/main.rs".to_string(), "src/cli/app.rs".to_string()]
        );
    }

    #[test]
    fn child_file_paths_prefer_next_action_read_file() {
        let files = child_files_from_summary(
            "meta.child_outcome=ok\nmeta.child_next_action=read_file:src/tools/dispatch_subagent.rs\nmeta.child_files=src/main.rs,src/lib.rs\nmeta.child_final_message=read the tool implementation",
        );
        assert_eq!(
            files,
            vec![
                "src/tools/dispatch_subagent.rs".to_string(),
                "src/main.rs".to_string(),
                "src/lib.rs".to_string()
            ]
        );
    }

    #[test]
    fn offline_planner_does_not_use_child_query_after_a_read() {
        let request = ModelRequest {
            system_prompt: String::new(),
            task: "inspect repository layout and summarize the main entrypoints".to_string(),
            profile_name: "generic".to_string(),
            profile_hints: Vec::new(),
            primary_file: None,
            suggested_test_command: None,
            available_tools: vec![
                "read_file".to_string(),
                "list_files".to_string(),
                "search_text".to_string(),
            ],
            observations: vec![
                Observation::ok(
                    "dispatch_subagent",
                    "meta.child_outcome=ok\nmeta.child_final_message=search for `route_benchmark_subcommand`",
                ),
                Observation::ok("list_files", "src/\nCargo.toml\nsrc/main.rs\n"),
                Observation::ok("read_file", "src/main.rs:1: fn main() {}"),
            ],
            todos: Vec::new(),
            planning_mode: false,
            recent_steps: Vec::new(),
        };

        let response = planner()
            .respond(request, &mut crate::ui::stream::NoopStreamEvents)
            .unwrap()
            .0;
        match response.action {
            ModelAction::Finish => {}
            ModelAction::CallTool { tool_name, .. } => {
                assert_ne!(tool_name, "search_text");
            }
        }
    }

    #[test]
    fn offline_planner_uses_recovery_hint_after_search_text_no_matches() {
        let request = ModelRequest {
            system_prompt: String::new(),
            task: "find where missing_symbol is implemented".to_string(),
            profile_name: "generic".to_string(),
            profile_hints: Vec::new(),
            primary_file: None,
            suggested_test_command: None,
            available_tools: vec![
                "search_text".to_string(),
                "list_files".to_string(),
                "read_file".to_string(),
            ],
            observations: vec![
                Observation::ok("search_text", "No matches for `missing_symbol`."),
                Observation::ok(
                    "recovery_hint",
                    "after=search_text; next=list_files; reason=search_text returned no matches, inspect the repository layout or broaden the lookup before retrying the query",
                ),
            ],
            todos: Vec::new(),
            planning_mode: false,
            recent_steps: Vec::new(),
        };

        let response = planner()
            .respond(request, &mut crate::ui::stream::NoopStreamEvents)
            .unwrap()
            .0;
        match response.action {
            ModelAction::CallTool { tool_name, input } => {
                assert_eq!(tool_name, "list_files");
                assert_eq!(input.get("max_depth"), Some("3"));
            }
            ModelAction::Finish => panic!("expected list_files recovery tool call"),
        }
    }

    #[test]
    fn offline_planner_uses_recovery_hint_after_read_file_failure() {
        let request = ModelRequest {
            system_prompt: String::new(),
            task: "find where dispatch_subagent is implemented and inspect the code".to_string(),
            profile_name: "generic".to_string(),
            profile_hints: Vec::new(),
            primary_file: None,
            suggested_test_command: None,
            available_tools: vec![
                "search_text".to_string(),
                "read_file".to_string(),
                "list_files".to_string(),
            ],
            observations: vec![
                Observation::failed("read_file", "No such file or directory (os error 2)"),
                Observation::ok(
                    "recovery_hint",
                    "after=read_file; next=search_text; reason=read_file failed, locate the correct file path before retrying the read",
                ),
            ],
            todos: Vec::new(),
            planning_mode: false,
            recent_steps: Vec::new(),
        };

        let response = planner()
            .respond(request, &mut crate::ui::stream::NoopStreamEvents)
            .unwrap()
            .0;
        match response.action {
            ModelAction::CallTool { tool_name, input } => {
                assert_eq!(tool_name, "search_text");
                assert_eq!(input.get("query"), Some("dispatch_subagent"));
            }
            ModelAction::Finish => panic!("expected search_text recovery tool call"),
        }
    }

    #[test]
    fn offline_planner_reviews_diff_after_failed_validation_recovery_hint() {
        let request = ModelRequest {
            system_prompt: String::new(),
            task: "implement the fix and validate with cargo test".to_string(),
            profile_name: "generic".to_string(),
            profile_hints: Vec::new(),
            primary_file: Some("src/lib.rs".to_string()),
            suggested_test_command: Some("cargo test".to_string()),
            available_tools: vec![
                "apply_patch".to_string(),
                "run_shell".to_string(),
                "git_diff".to_string(),
                "read_file".to_string(),
            ],
            observations: vec![
                Observation::ok("apply_patch", "patched src/lib.rs"),
                Observation::ok("run_shell", "exit_code: 101\nstderr:\ntest failed"),
                Observation::ok(
                    "recovery_hint",
                    "after=run_shell; next=git_diff; reason=run_shell exited non-zero, inspect the relevant code or diff before retrying the command",
                ),
            ],
            todos: Vec::new(),
            planning_mode: false,
            recent_steps: Vec::new(),
        };

        let response = planner()
            .respond(request, &mut crate::ui::stream::NoopStreamEvents)
            .unwrap()
            .0;
        match response.action {
            ModelAction::CallTool { tool_name, .. } => assert_eq!(tool_name, "git_diff"),
            ModelAction::Finish => panic!("expected git_diff recovery tool call"),
        }
    }

    #[test]
    fn offline_planner_reads_patched_file_after_failed_validation_when_diff_already_reviewed() {
        let request = ModelRequest {
            system_prompt: String::new(),
            task: "implement the fix and validate with cargo test".to_string(),
            profile_name: "rust".to_string(),
            profile_hints: Vec::new(),
            primary_file: Some("Cargo.toml".to_string()),
            suggested_test_command: Some("cargo test".to_string()),
            available_tools: vec![
                "apply_patch".to_string(),
                "run_shell".to_string(),
                "git_diff".to_string(),
                "read_file".to_string(),
            ],
            observations: vec![
                Observation::ok("apply_patch", "patched src/lib.rs"),
                Observation::ok("git_diff", "diff --git a/src/lib.rs b/src/lib.rs"),
                Observation::ok(
                    "run_shell",
                    "meta.command_kind=test\nmeta.exit_code=101\nmeta.result=failed\nmeta.failure_kind=test_failure\nmeta.failed_tests=tests::adds_numbers\nmeta.stderr_summary=test failed\nexit_code: 101\nstderr:\ntest failed",
                ),
                Observation::ok(
                    "recovery_hint",
                    "after=run_shell; next=git_diff; reason=run_shell reported failing tests (tests::adds_numbers), inspect the relevant code or diff before retrying the command",
                ),
            ],
            todos: Vec::new(),
            planning_mode: false,
            recent_steps: Vec::new(),
        };

        let response = planner()
            .respond(request, &mut crate::ui::stream::NoopStreamEvents)
            .unwrap()
            .0;
        match response.action {
            ModelAction::CallTool { tool_name, input } => {
                assert_eq!(tool_name, "read_file");
                assert_eq!(input.get("path"), Some("src/lib.rs"));
            }
            ModelAction::Finish => panic!("expected read_file recovery tool call"),
        }
    }

    #[test]
    fn offline_planner_applies_retry_patch_after_failed_validation_readback() {
        let client = planner();
        let request = ModelRequest {
            system_prompt: "system".to_string(),
            task:
                "replace `a - b` with `a * b` in src/lib.rs and validate with cargo test until the tests pass"
                    .to_string(),
            profile_name: "rust".to_string(),
            profile_hints: vec![],
            primary_file: Some("src/lib.rs".to_string()),
            suggested_test_command: Some("cargo test".to_string()),
            available_tools: vec![
                "apply_patch".to_string(),
                "git_diff".to_string(),
                "run_shell".to_string(),
                "read_file".to_string(),
            ],
            observations: vec![
                Observation::ok("apply_patch", "patched src/lib.rs"),
                Observation::ok("git_diff", "diff --git a/src/lib.rs b/src/lib.rs"),
                Observation::ok(
                    "run_shell",
                    "meta.command_kind=test\nmeta.exit_code=101\nmeta.result=failed\nmeta.failure_kind=test_failure\nmeta.failed_tests=tests::adds_numbers\nmeta.stderr_summary=test failed\nexit_code: 101\nstderr:\ntest failed",
                ),
                Observation::ok(
                    "recovery_hint",
                    "after=run_shell; next=git_diff; reason=run_shell reported failing tests (tests::adds_numbers), inspect the relevant code or diff before retrying the command",
                ),
                Observation::ok("read_file", "pub fn add(a: i32, b: i32) -> i32 {\n    a * b\n}"),
            ],
            todos: Vec::new(),
            planning_mode: false,
            recent_steps: Vec::new(),
        };

        let response = client.respond_offline(request);
        match response.action {
            ModelAction::CallTool { tool_name, input } => {
                assert_eq!(tool_name, "apply_patch");
                assert_eq!(input.get("path"), Some("src/lib.rs"));
                assert_eq!(input.get("find"), Some("a * b"));
                assert_eq!(input.get("replace"), Some("a + b"));
            }
            ModelAction::Finish => panic!("expected retry apply_patch after failed validation"),
        }
    }

    #[test]
    fn offline_planner_applies_retry_patch_after_js_test_readback() {
        let client = planner();
        let request = ModelRequest {
            system_prompt: "system".to_string(),
            task:
                "replace `a - b` with `a * b` in src/math.js and validate with npm test until the tests pass"
                    .to_string(),
            profile_name: "javascript".to_string(),
            profile_hints: vec![],
            primary_file: Some("src/math.js".to_string()),
            suggested_test_command: Some("npm test".to_string()),
            available_tools: vec![
                "apply_patch".to_string(),
                "git_diff".to_string(),
                "run_shell".to_string(),
                "read_file".to_string(),
            ],
            observations: vec![
                Observation::ok("apply_patch", "patched src/math.js"),
                Observation::ok("git_diff", "diff --git a/src/math.js b/src/math.js"),
                Observation::ok(
                    "run_shell",
                    "meta.command_kind=test\nmeta.exit_code=1\nmeta.result=failed\nmeta.failure_kind=test_failure\nmeta.failed_tests=test/math.test.js\nmeta.stderr_summary=test failed\nexit_code: 1\nstderr:\ntest failed",
                ),
                Observation::ok(
                    "recovery_hint",
                    "after=run_shell; next=read_file; path=test/math.test.js; reason=run_shell reported failing tests (test/math.test.js), inspect the relevant code or diff before retrying the command",
                ),
                Observation::ok(
                    "read_file",
                    "   1 import test from \"node:test\";\n   2 import assert from \"node:assert/strict\";\n   3 \n   4 import { add } from \"../src/math.js\";\n   5 \n   6 test(\"add returns the sum\", () => {\n   7   assert.equal(add(2, 3), 5);\n   8 });",
                ),
            ],
            todos: Vec::new(),
            planning_mode: false,
            recent_steps: Vec::new(),
        };

        let response = client.respond_offline(request);
        match response.action {
            ModelAction::CallTool { tool_name, input } => {
                assert_eq!(tool_name, "apply_patch");
                assert_eq!(input.get("path"), Some("src/math.js"));
                assert_eq!(input.get("find"), Some("a * b"));
                assert_eq!(input.get("replace"), Some("a + b"));
            }
            ModelAction::Finish => {
                panic!("expected retry apply_patch after JavaScript test readback")
            }
        }
    }

    #[test]
    fn offline_planner_reviews_diff_after_retry_patch() {
        let client = planner();
        let request = ModelRequest {
            system_prompt: "system".to_string(),
            task:
                "replace `a - b` with `a * b` in src/lib.rs and validate with cargo test until the tests pass"
                    .to_string(),
            profile_name: "rust".to_string(),
            profile_hints: vec![],
            primary_file: Some("src/lib.rs".to_string()),
            suggested_test_command: Some("cargo test".to_string()),
            available_tools: vec!["git_diff".to_string(), "run_shell".to_string()],
            observations: vec![
                Observation::ok("apply_patch", "patched src/lib.rs"),
                Observation::ok("git_diff", "diff --git a/src/lib.rs b/src/lib.rs"),
                Observation::ok(
                    "run_shell",
                    "meta.command_kind=test\nmeta.exit_code=101\nmeta.result=failed\nmeta.failure_kind=test_failure\nmeta.failed_tests=tests::adds_numbers\nmeta.stderr_summary=test failed\nexit_code: 101\nstderr:\ntest failed",
                ),
                Observation::ok("apply_patch", "updated src/lib.rs"),
            ],
            todos: Vec::new(),
            planning_mode: false,
            recent_steps: Vec::new(),
        };

        let response = client.respond_offline(request);
        match response.action {
            ModelAction::CallTool { tool_name, .. } => assert_eq!(tool_name, "git_diff"),
            ModelAction::Finish => panic!("expected git_diff after retry apply_patch"),
        }
    }

    #[test]
    fn offline_planner_reruns_validation_after_retry_diff() {
        let client = planner();
        let request = ModelRequest {
            system_prompt: "system".to_string(),
            task:
                "replace `a - b` with `a * b` in src/lib.rs and validate with cargo test until the tests pass"
                    .to_string(),
            profile_name: "rust".to_string(),
            profile_hints: vec![],
            primary_file: Some("src/lib.rs".to_string()),
            suggested_test_command: Some("cargo test".to_string()),
            available_tools: vec!["run_shell".to_string()],
            observations: vec![
                Observation::ok("apply_patch", "patched src/lib.rs"),
                Observation::ok("git_diff", "diff --git a/src/lib.rs b/src/lib.rs"),
                Observation::ok(
                    "run_shell",
                    "meta.command_kind=test\nmeta.exit_code=101\nmeta.result=failed\nmeta.failure_kind=test_failure\nmeta.failed_tests=tests::adds_numbers\nmeta.stderr_summary=test failed\nexit_code: 101\nstderr:\ntest failed",
                ),
                Observation::ok("apply_patch", "updated src/lib.rs"),
                Observation::ok("git_diff", "diff --git a/src/lib.rs b/src/lib.rs"),
            ],
            todos: Vec::new(),
            planning_mode: false,
            recent_steps: Vec::new(),
        };

        let response = client.respond_offline(request);
        match response.action {
            ModelAction::CallTool { tool_name, input } => {
                assert_eq!(tool_name, "run_shell");
                assert_eq!(input.get("command"), Some("cargo test"));
            }
            ModelAction::Finish => panic!("expected rerun of cargo test after retry patch"),
        }
    }

    #[test]
    fn last_patched_file_path_reads_unified_patch_summary() {
        let observations = vec![Observation::ok(
            "apply_patch",
            "Applied unified patch in src (touched 1 file).\nmodified:\n  - lib.rs\nstdout:\npatching file lib.rs",
        )];
        assert_eq!(
            last_patched_file_path(&observations).as_deref(),
            Some("src/lib.rs")
        );
    }

    #[test]
    fn offline_planner_uses_recovery_hint_query_for_search_text() {
        let request = ModelRequest {
            system_prompt: String::new(),
            task: "fix the lint failure".to_string(),
            profile_name: "generic".to_string(),
            profile_hints: Vec::new(),
            primary_file: None,
            suggested_test_command: None,
            available_tools: vec!["search_text".to_string(), "read_file".to_string()],
            observations: vec![Observation::ok(
                "recovery_hint",
                "after=run_shell; next=search_text; query=dispatch_subagent; reason=run_shell reported a lint failure",
            )],
            todos: Vec::new(),
            planning_mode: false,
            recent_steps: Vec::new(),
        };

        let response = planner()
            .respond(request, &mut crate::ui::stream::NoopStreamEvents)
            .unwrap()
            .0;
        match response.action {
            ModelAction::CallTool { tool_name, input } => {
                assert_eq!(tool_name, "search_text");
                assert_eq!(input.get("query"), Some("dispatch_subagent"));
            }
            ModelAction::Finish => panic!("expected search_text recovery tool call"),
        }
    }

    #[test]
    fn offline_planner_uses_recovery_hint_path_for_read_file() {
        let request = ModelRequest {
            system_prompt: String::new(),
            task: "investigate the failing test".to_string(),
            profile_name: "generic".to_string(),
            profile_hints: Vec::new(),
            primary_file: None,
            suggested_test_command: None,
            available_tools: vec!["read_file".to_string(), "search_text".to_string()],
            observations: vec![Observation::ok(
                "recovery_hint",
                "after=run_shell; next=read_file; path=src/cli/app.rs; reason=run_shell reported failing tests",
            )],
            todos: Vec::new(),
            planning_mode: false,
            recent_steps: Vec::new(),
        };

        let response = planner()
            .respond(request, &mut crate::ui::stream::NoopStreamEvents)
            .unwrap()
            .0;
        match response.action {
            ModelAction::CallTool { tool_name, input } => {
                assert_eq!(tool_name, "read_file");
                assert_eq!(input.get("path"), Some("src/cli/app.rs"));
            }
            ModelAction::Finish => panic!("expected read_file recovery tool call"),
        }
    }

    #[test]
    fn offline_planner_finishes_after_failure_repro_readback() {
        let request = ModelRequest {
            system_prompt: String::new(),
            task: "investigate why npm test fails in the JavaScript CLI and inspect the failing test file before retrying".to_string(),
            profile_name: "javascript".to_string(),
            profile_hints: Vec::new(),
            primary_file: None,
            suggested_test_command: Some("npm test".to_string()),
            available_tools: vec![
                "run_shell".to_string(),
                "read_file".to_string(),
                "search_text".to_string(),
                "todo_write".to_string(),
            ],
            observations: vec![
                Observation::ok(
                    "run_shell",
                    "meta.command_kind=test\nmeta.exit_code=1\nmeta.result=failed\nmeta.failure_kind=test_failure\nmeta.failed_tests=test/route-benchmark.test.js\nmeta.stderr_summary=test failed\nexit_code: 1\nstderr:\ntest failed",
                ),
                Observation::ok(
                    "recovery_hint",
                    "after=run_shell; next=read_file; path=test/route-benchmark.test.js; reason=run_shell reported failing tests (test/route-benchmark.test.js), inspect the relevant code or diff before retrying the command",
                ),
                Observation::ok(
                    "read_file",
                    "   1 import test from \"node:test\";\n   2 import assert from \"node:assert/strict\";\n   3 \n   4 import { main, routeBenchmarkCommand } from \"../src/index.js\";",
                ),
            ],
            todos: vec![
                crate::core::todos::Todo {
                    content: "Reproduce the failing validation command".to_string(),
                    active_form: "Reproducing the failing validation command".to_string(),
                    status: crate::core::todos::TodoStatus::Completed,
                },
                crate::core::todos::Todo {
                    content: "Read the most relevant files".to_string(),
                    active_form: "Reading the most relevant files".to_string(),
                    status: crate::core::todos::TodoStatus::InProgress,
                },
            ],
            planning_mode: true,
            recent_steps: Vec::new(),
        };

        let response = planner()
            .respond(request, &mut crate::ui::stream::NoopStreamEvents)
            .unwrap()
            .0;
        assert!(matches!(response.action, ModelAction::Finish));
    }

    #[test]
    fn offline_planner_reproduces_failure_before_searching_for_recovery_tasks() {
        let request = ModelRequest {
            system_prompt: String::new(),
            task: "investigate why npm test fails in the JavaScript CLI and inspect the failing test file before retrying".to_string(),
            profile_name: "javascript".to_string(),
            profile_hints: Vec::new(),
            primary_file: None,
            suggested_test_command: Some("npm test".to_string()),
            available_tools: vec![
                "todo_write".to_string(),
                "run_shell".to_string(),
                "search_text".to_string(),
                "read_file".to_string(),
                "list_files".to_string(),
            ],
            observations: vec![Observation::ok("todo_write", "plan created")],
            todos: vec![
                crate::core::todos::Todo {
                    content: "Reproduce the failing validation command".to_string(),
                    active_form: "Reproducing the failing validation command".to_string(),
                    status: crate::core::todos::TodoStatus::InProgress,
                },
                crate::core::todos::Todo {
                    content: "Read the most relevant files".to_string(),
                    active_form: "Reading the most relevant files".to_string(),
                    status: crate::core::todos::TodoStatus::Pending,
                },
            ],
            planning_mode: true,
            recent_steps: Vec::new(),
        };

        let response = planner()
            .respond(request, &mut crate::ui::stream::NoopStreamEvents)
            .unwrap()
            .0;
        match response.action {
            ModelAction::CallTool { tool_name, input } => {
                assert_eq!(tool_name, "run_shell");
                assert_eq!(input.get("command"), Some("npm test"));
            }
            ModelAction::Finish => panic!("expected run_shell repro tool call"),
        }
    }

    #[test]
    fn offline_planner_still_reproduces_failure_before_search_even_with_repo_signal() {
        let request = ModelRequest {
            system_prompt: String::new(),
            task: "investigate why npm test fails in the JavaScript CLI and inspect the failing test file before retrying".to_string(),
            profile_name: "javascript".to_string(),
            profile_hints: Vec::new(),
            primary_file: None,
            suggested_test_command: Some("npm test".to_string()),
            available_tools: vec![
                "run_shell".to_string(),
                "search_text".to_string(),
                "read_file".to_string(),
                "list_files".to_string(),
            ],
            observations: vec![Observation::ok("search_text", "src/index.js: routeBenchmarkCommand")],
            todos: Vec::new(),
            planning_mode: false,
            recent_steps: Vec::new(),
        };

        let response = planner()
            .respond(request, &mut crate::ui::stream::NoopStreamEvents)
            .unwrap()
            .0;
        match response.action {
            ModelAction::CallTool { tool_name, input } => {
                assert_eq!(tool_name, "run_shell");
                assert_eq!(input.get("command"), Some("npm test"));
            }
            ModelAction::Finish => panic!("expected run_shell repro tool call"),
        }
    }

    #[test]
    fn build_initial_todo_plan_prioritizes_failure_repro_before_search() {
        let request = ModelRequest {
            system_prompt: String::new(),
            task: "investigate why npm test fails in the JavaScript CLI and inspect the failing test file before retrying".to_string(),
            profile_name: "javascript".to_string(),
            profile_hints: Vec::new(),
            primary_file: None,
            suggested_test_command: Some("npm test".to_string()),
            available_tools: vec![
                "todo_write".to_string(),
                "run_shell".to_string(),
                "search_text".to_string(),
                "read_file".to_string(),
            ],
            observations: Vec::new(),
            todos: Vec::new(),
            planning_mode: true,
            recent_steps: Vec::new(),
        };

        let plan = super::build_initial_todo_plan_json(&request);
        let repro_index = plan
            .find("Reproduce the failing validation command")
            .unwrap();
        let read_index = plan.find("Read the most relevant files").unwrap();
        assert!(repro_index < read_index, "plan: {plan}");
    }

    #[test]
    fn offline_planner_replans_after_replan_hint() {
        let request = ModelRequest {
            system_prompt: String::new(),
            task: "implement the fix and validate with cargo test".to_string(),
            profile_name: "generic".to_string(),
            profile_hints: Vec::new(),
            primary_file: Some("src/lib.rs".to_string()),
            suggested_test_command: Some("cargo test".to_string()),
            available_tools: vec![
                "todo_write".to_string(),
                "read_file".to_string(),
                "apply_patch".to_string(),
                "run_shell".to_string(),
                "git_diff".to_string(),
            ],
            observations: vec![Observation::ok(
                "replan_hint",
                "reason=multiple recovery hints in recent steps; action=replan the remaining todo list before continuing",
            )],
            todos: vec![
                crate::core::todos::Todo {
                    content: "Implement the requested changes".to_string(),
                    active_form: "Implementing the requested changes".to_string(),
                    status: crate::core::todos::TodoStatus::InProgress,
                },
                crate::core::todos::Todo {
                    content: "Validate with `cargo test`".to_string(),
                    active_form: "Validating with `cargo test`".to_string(),
                    status: crate::core::todos::TodoStatus::Pending,
                },
            ],
            planning_mode: true,
            recent_steps: Vec::new(),
        };

        let response = planner()
            .respond(request, &mut crate::ui::stream::NoopStreamEvents)
            .unwrap()
            .0;
        match response.action {
            ModelAction::CallTool { tool_name, input } => {
                assert_eq!(tool_name, "todo_write");
                let items = input.get("items").expect("expected items arg");
                assert!(
                    items.contains("Reassess the plan using the latest blocker or recovery signal")
                );
                assert!(items.contains("Implement the requested changes"));
            }
            ModelAction::Finish => panic!("expected todo_write replan tool call"),
        }
    }

    #[test]
    fn parse_openai_usage_extracts_prompt_and_completion() {
        let body = r#"{
            "choices": [{"message": {"role": "assistant", "content": "ok"}}],
            "usage": {"prompt_tokens": 12, "completion_tokens": 5}
        }"#;
        let usage = parse_openai_usage(body).unwrap();
        assert_eq!(usage.prompt, 12);
        assert_eq!(usage.completion, 5);
    }

    #[test]
    fn parse_openai_usage_returns_none_when_missing() {
        let body = r#"{"choices":[{"message":{"role":"assistant","content":"ok"}}]}"#;
        assert!(parse_openai_usage(body).is_none());
    }

    #[test]
    fn parse_anthropic_usage_extracts_input_and_output() {
        let body = r#"{
            "content": [{"type":"text","text":"ok"}],
            "usage": {"input_tokens": 30, "output_tokens": 11}
        }"#;
        let usage = parse_anthropic_usage(body).unwrap();
        assert_eq!(usage.prompt, 30);
        assert_eq!(usage.completion, 11);
    }

    use crate::ui::stream::{NoopStreamEvents, StreamEvents};
    use std::cell::RefCell;
    use std::collections::BTreeMap;
    use std::io::Cursor;

    #[derive(Default)]
    struct CapturingEvents {
        chunks: RefCell<Vec<String>>,
        done: RefCell<Vec<String>>,
        tool_calls: RefCell<Vec<(String, BTreeMap<String, String>)>>,
    }

    impl StreamEvents for CapturingEvents {
        fn on_text_delta(&mut self, chunk: &str) {
            self.chunks.borrow_mut().push(chunk.to_string());
        }
        fn on_assistant_done(&mut self, full_text: &str) {
            self.done.borrow_mut().push(full_text.to_string());
        }
        fn on_tool_call(&mut self, name: &str, input: &BTreeMap<String, String>) {
            self.tool_calls
                .borrow_mut()
                .push((name.to_string(), input.clone()));
        }
    }

    #[test]
    fn parse_openai_stream_emits_text_deltas_and_finishes() {
        let body = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"Hel\"},\"finish_reason\":null}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"lo\"},\"finish_reason\":null}]}\n\n",
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
            "data: {\"usage\":{\"prompt_tokens\":3,\"completion_tokens\":2}}\n\n",
            "data: [DONE]\n\n",
        );
        let mut cur = Cursor::new(body.as_bytes().to_vec());
        let mut events = CapturingEvents::default();
        let (resp, usage) = super::parse_openai_stream(&mut cur, &mut events).unwrap();
        assert_eq!(resp.message, "Hello");
        assert!(matches!(resp.action, super::ModelAction::Finish));
        let usage = usage.expect("usage");
        assert_eq!(usage.prompt, 3);
        assert_eq!(usage.completion, 2);
        let chunks = events.chunks.borrow();
        assert_eq!(*chunks, vec!["Hel".to_string(), "lo".to_string()]);
        assert_eq!(events.done.borrow().len(), 1);
        assert!(events.tool_calls.borrow().is_empty());
    }

    #[test]
    fn parse_openai_stream_assembles_tool_call_across_chunks() {
        let body = concat!(
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_a\",\"type\":\"function\",\"function\":{\"name\":\"read_file\",\"arguments\":\"{\\\"pa\"}}]},\"finish_reason\":null}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"th\\\":\\\"a.rs\\\"}\"}}]},\"finish_reason\":null}]}\n\n",
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n",
            "data: [DONE]\n\n",
        );
        let mut cur = Cursor::new(body.as_bytes().to_vec());
        let mut events = CapturingEvents::default();
        let (resp, _usage) = super::parse_openai_stream(&mut cur, &mut events).unwrap();
        match resp.action {
            super::ModelAction::CallTool { tool_name, input } => {
                assert_eq!(tool_name, "read_file");
                assert_eq!(input.get("path"), Some("a.rs"));
            }
            super::ModelAction::Finish => panic!("expected tool call"),
        }
        let calls = events.tool_calls.borrow();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "read_file");
        assert_eq!(calls[0].1.get("path").map(String::as_str), Some("a.rs"));
    }

    #[test]
    fn parse_openai_stream_returns_none_usage_when_omitted() {
        let body = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"ok\"},\"finish_reason\":null}]}\n\n",
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
            "data: [DONE]\n\n",
        );
        let mut cur = Cursor::new(body.as_bytes().to_vec());
        let mut events = NoopStreamEvents;
        let (_resp, usage) = super::parse_openai_stream(&mut cur, &mut events).unwrap();
        assert!(usage.is_none());
    }

    #[test]
    fn parse_openai_stream_errors_on_malformed_tool_arguments() {
        let body = concat!(
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"x\",\"type\":\"function\",\"function\":{\"name\":\"git_diff\",\"arguments\":\"{not_json\"}}]},\"finish_reason\":null}]}\n\n",
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n",
            "data: [DONE]\n\n",
        );
        let mut cur = Cursor::new(body.as_bytes().to_vec());
        let mut events = NoopStreamEvents;
        let result = super::parse_openai_stream(&mut cur, &mut events);
        assert!(result.is_err());
    }

    #[test]
    fn parse_anthropic_stream_emits_text_deltas_and_message_stop() {
        let body = concat!(
            "event: message_start\ndata: {\"message\":{\"usage\":{\"input_tokens\":10,\"output_tokens\":0}}}\n\n",
            "event: content_block_start\ndata: {\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
            "event: content_block_delta\ndata: {\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"hi \"}}\n\n",
            "event: content_block_delta\ndata: {\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"there\"}}\n\n",
            "event: content_block_stop\ndata: {\"index\":0}\n\n",
            "event: message_delta\ndata: {\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":2}}\n\n",
            "event: message_stop\ndata: {}\n\n",
        );
        let mut cur = Cursor::new(body.as_bytes().to_vec());
        let mut events = CapturingEvents::default();
        let (resp, usage) = super::parse_anthropic_stream(&mut cur, &mut events).unwrap();
        assert_eq!(resp.message, "hi there");
        assert!(matches!(resp.action, super::ModelAction::Finish));
        let usage = usage.expect("usage");
        assert_eq!(usage.prompt, 10);
        assert_eq!(usage.completion, 2);
        let chunks = events.chunks.borrow();
        assert_eq!(*chunks, vec!["hi ".to_string(), "there".to_string()]);
    }

    #[test]
    fn parse_anthropic_stream_assembles_tool_use_input_json() {
        let body = concat!(
            "event: message_start\ndata: {\"message\":{\"usage\":{\"input_tokens\":1,\"output_tokens\":0}}}\n\n",
            "event: content_block_start\ndata: {\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"tu_1\",\"name\":\"read_file\",\"input\":{}}}\n\n",
            "event: content_block_delta\ndata: {\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"path\\\":\"}}\n\n",
            "event: content_block_delta\ndata: {\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"\\\"a.rs\\\"}\"}}\n\n",
            "event: content_block_stop\ndata: {\"index\":0}\n\n",
            "event: message_delta\ndata: {\"delta\":{\"stop_reason\":\"tool_use\"},\"usage\":{\"output_tokens\":2}}\n\n",
            "event: message_stop\ndata: {}\n\n",
        );
        let mut cur = Cursor::new(body.as_bytes().to_vec());
        let mut events = CapturingEvents::default();
        let (resp, _usage) = super::parse_anthropic_stream(&mut cur, &mut events).unwrap();
        match resp.action {
            super::ModelAction::CallTool { tool_name, input } => {
                assert_eq!(tool_name, "read_file");
                assert_eq!(input.get("path"), Some("a.rs"));
            }
            super::ModelAction::Finish => panic!("expected tool call"),
        }
        let calls = events.tool_calls.borrow();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "read_file");
    }

    #[test]
    fn parse_anthropic_stream_keeps_initial_usage_when_message_delta_missing_input() {
        let body = concat!(
            "event: message_start\ndata: {\"message\":{\"usage\":{\"input_tokens\":7,\"output_tokens\":0}}}\n\n",
            "event: content_block_start\ndata: {\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
            "event: content_block_delta\ndata: {\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"x\"}}\n\n",
            "event: message_delta\ndata: {\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":4}}\n\n",
            "event: message_stop\ndata: {}\n\n",
        );
        let mut cur = Cursor::new(body.as_bytes().to_vec());
        let mut events = NoopStreamEvents;
        let (_resp, usage) = super::parse_anthropic_stream(&mut cur, &mut events).unwrap();
        let usage = usage.expect("usage");
        assert_eq!(usage.prompt, 7);
        assert_eq!(usage.completion, 4);
    }

    #[test]
    fn parse_anthropic_stream_errors_on_malformed_tool_input() {
        let body = concat!(
            "event: message_start\ndata: {\"message\":{\"usage\":{\"input_tokens\":1,\"output_tokens\":0}}}\n\n",
            "event: content_block_start\ndata: {\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"tu\",\"name\":\"read_file\",\"input\":{}}}\n\n",
            "event: content_block_delta\ndata: {\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"not_json\"}}\n\n",
            "event: content_block_stop\ndata: {\"index\":0}\n\n",
            "event: message_stop\ndata: {}\n\n",
        );
        let mut cur = Cursor::new(body.as_bytes().to_vec());
        let mut events = NoopStreamEvents;
        let result = super::parse_anthropic_stream(&mut cur, &mut events);
        assert!(result.is_err());
    }

    #[test]
    fn parse_openai_stream_calls_on_assistant_done_on_error() {
        // After streaming partial text, malformed JSON should still trigger on_assistant_done.
        let body = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"hi \"},\"finish_reason\":null}]}\n\n",
            "data: {garbage_not_json}\n\n",
        );
        let mut cur = Cursor::new(body.as_bytes().to_vec());
        let mut events = CapturingEvents::default();
        let result = super::parse_openai_stream(&mut cur, &mut events);
        assert!(result.is_err(), "expected malformed-frame error");
        // Trait contract: on_assistant_done called exactly once even on error.
        assert_eq!(
            events.done.borrow().len(),
            1,
            "on_assistant_done not called on error"
        );
        // Partial text should still have been streamed before error.
        let chunks = events.chunks.borrow();
        assert_eq!(*chunks, vec!["hi ".to_string()]);
    }

    #[test]
    fn parse_anthropic_stream_calls_on_assistant_done_on_error() {
        let body = concat!(
            "event: message_start\ndata: {\"message\":{\"usage\":{\"input_tokens\":1,\"output_tokens\":0}}}\n\n",
            "event: content_block_start\ndata: {\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
            "event: content_block_delta\ndata: {\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"hi\"}}\n\n",
            "event: content_block_delta\ndata: {garbage_not_json}\n\n",
        );
        let mut cur = Cursor::new(body.as_bytes().to_vec());
        let mut events = CapturingEvents::default();
        let result = super::parse_anthropic_stream(&mut cur, &mut events);
        assert!(result.is_err());
        assert_eq!(events.done.borrow().len(), 1);
        let chunks = events.chunks.borrow();
        assert_eq!(*chunks, vec!["hi".to_string()]);
    }

    #[test]
    fn parse_openai_stream_calls_on_assistant_done_exactly_once_on_post_loop_error() {
        // Stream completes normally but tool args are malformed JSON.
        // Inner used to call on_assistant_done before erroring; outer used
        // to call it again on Err. Now: exactly once.
        let body = concat!(
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"x\",\"type\":\"function\",\"function\":{\"name\":\"git_diff\",\"arguments\":\"NOT_JSON\"}}]},\"finish_reason\":null}]}\n\n",
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n",
            "data: [DONE]\n\n",
        );
        let mut cur = Cursor::new(body.as_bytes().to_vec());
        let mut events = CapturingEvents::default();
        let result = super::parse_openai_stream(&mut cur, &mut events);
        assert!(result.is_err(), "expected post-loop tool-args parse error");
        assert_eq!(
            events.done.borrow().len(),
            1,
            "on_assistant_done must fire exactly once even when post-loop parsing errors"
        );
    }

    #[test]
    fn parse_anthropic_stream_calls_on_assistant_done_exactly_once_on_post_loop_error() {
        // Stream emits valid frames but partial_json never assembles to valid JSON.
        let body = concat!(
            "event: message_start\ndata: {\"message\":{\"usage\":{\"input_tokens\":1,\"output_tokens\":0}}}\n\n",
            "event: content_block_start\ndata: {\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"tu\",\"name\":\"git_diff\",\"input\":{}}}\n\n",
            "event: content_block_delta\ndata: {\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"NOT_JSON\"}}\n\n",
            "event: content_block_stop\ndata: {\"index\":0}\n\n",
            "event: message_stop\ndata: {}\n\n",
        );
        let mut cur = Cursor::new(body.as_bytes().to_vec());
        let mut events = CapturingEvents::default();
        let result = super::parse_anthropic_stream(&mut cur, &mut events);
        assert!(result.is_err());
        assert_eq!(
            events.done.borrow().len(),
            1,
            "on_assistant_done must fire exactly once even when post-loop parsing errors"
        );
    }

    #[test]
    fn parse_openai_stream_keeps_first_parallel_tool_call() {
        // Some OpenAI-compatible gateways may ignore `parallel_tool_calls:false`.
        // Execute the first call and ignore later same-turn calls so the loop can
        // continue serially on the next model turn.
        let body = concat!(
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"a\",\"type\":\"function\",\"function\":{\"name\":\"read_file\",\"arguments\":\"{\\\"path\\\":\\\"a.rs\\\"}\"}}]},\"finish_reason\":null}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":1,\"id\":\"b\",\"type\":\"function\",\"function\":{\"name\":\"git_diff\",\"arguments\":\"{}\"}}]},\"finish_reason\":null}]}\n\n",
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n",
            "data: [DONE]\n\n",
        );
        let mut cur = Cursor::new(body.as_bytes().to_vec());
        let mut events = NoopStreamEvents;
        let (resp, _usage) = super::parse_openai_stream(&mut cur, &mut events).unwrap();
        match resp.action {
            super::ModelAction::CallTool { tool_name, input } => {
                assert_eq!(tool_name, "read_file");
                assert_eq!(input.get("path"), Some("a.rs"));
            }
            _ => panic!("expected first tool call to be used"),
        }
    }

    #[test]
    fn parse_openai_stream_handles_explicit_index_zero() {
        // index field present but always 0 — should still parse correctly.
        let body = concat!(
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"a\",\"type\":\"function\",\"function\":{\"name\":\"read_file\",\"arguments\":\"{\\\"path\\\":\\\"a.rs\\\"}\"}}]},\"finish_reason\":null}]}\n\n",
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n",
            "data: [DONE]\n\n",
        );
        let mut cur = Cursor::new(body.as_bytes().to_vec());
        let mut events = NoopStreamEvents;
        let (resp, _usage) = super::parse_openai_stream(&mut cur, &mut events).unwrap();
        match resp.action {
            super::ModelAction::CallTool { tool_name, input } => {
                assert_eq!(tool_name, "read_file");
                assert_eq!(input.get("path"), Some("a.rs"));
            }
            super::ModelAction::Finish => panic!("expected tool call"),
        }
    }

    #[test]
    fn parse_anthropic_stream_errors_on_parallel_tool_use_blocks() {
        let body = concat!(
            "event: message_start\ndata: {\"message\":{\"usage\":{\"input_tokens\":1,\"output_tokens\":0}}}\n\n",
            "event: content_block_start\ndata: {\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"tu_0\",\"name\":\"read_file\",\"input\":{}}}\n\n",
            "event: content_block_delta\ndata: {\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{}\"}}\n\n",
            "event: content_block_stop\ndata: {\"index\":0}\n\n",
            "event: content_block_start\ndata: {\"index\":1,\"content_block\":{\"type\":\"tool_use\",\"id\":\"tu_1\",\"name\":\"git_diff\",\"input\":{}}}\n\n",
            "event: message_stop\ndata: {}\n\n",
        );
        let mut cur = Cursor::new(body.as_bytes().to_vec());
        let mut events = NoopStreamEvents;
        let result = super::parse_anthropic_stream(&mut cur, &mut events);
        let err = result.unwrap_err();
        assert!(
            err.to_string()
                .contains("multiple parallel tool_use blocks"),
            "unexpected error: {}",
            err
        );
    }

    #[test]
    fn parse_anthropic_stream_ignores_input_json_delta_for_unrelated_index() {
        // tool_use is at index 0; an input_json_delta at index 1 (mismatched)
        // should be ignored, not corrupt the assembled JSON.
        let body = concat!(
            "event: message_start\ndata: {\"message\":{\"usage\":{\"input_tokens\":1,\"output_tokens\":0}}}\n\n",
            "event: content_block_start\ndata: {\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"tu\",\"name\":\"read_file\",\"input\":{}}}\n\n",
            "event: content_block_delta\ndata: {\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"path\\\":\\\"a.rs\\\"}\"}}\n\n",
            "event: content_block_delta\ndata: {\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"GARBAGE\"}}\n\n",
            "event: content_block_stop\ndata: {\"index\":0}\n\n",
            "event: message_stop\ndata: {}\n\n",
        );
        let mut cur = Cursor::new(body.as_bytes().to_vec());
        let mut events = NoopStreamEvents;
        let (resp, _usage) = super::parse_anthropic_stream(&mut cur, &mut events).unwrap();
        match resp.action {
            super::ModelAction::CallTool { tool_name, input } => {
                assert_eq!(tool_name, "read_file");
                assert_eq!(input.get("path"), Some("a.rs"));
            }
            super::ModelAction::Finish => panic!("expected tool call"),
        }
    }

    #[test]
    fn parse_openai_stream_handles_missing_index_on_followup_chunk() {
        // First chunk has index=1, follow-up omits index. Should NOT trigger
        // the parallel-tool-calls error (continue current assembly).
        let body = concat!(
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":1,\"id\":\"x\",\"type\":\"function\",\"function\":{\"name\":\"read_file\",\"arguments\":\"{\\\"pa\"}}]},\"finish_reason\":null}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"function\":{\"arguments\":\"th\\\":\\\"a.rs\\\"}\"}}]},\"finish_reason\":null}]}\n\n",
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n",
            "data: [DONE]\n\n",
        );
        let mut cur = Cursor::new(body.as_bytes().to_vec());
        let mut events = NoopStreamEvents;
        let (resp, _usage) = super::parse_openai_stream(&mut cur, &mut events).unwrap();
        match resp.action {
            super::ModelAction::CallTool { tool_name, input } => {
                assert_eq!(tool_name, "read_file");
                assert_eq!(input.get("path"), Some("a.rs"));
            }
            super::ModelAction::Finish => panic!("expected tool call"),
        }
    }

    #[test]
    fn parse_anthropic_stream_errors_on_repeated_content_block_start_at_same_index() {
        let body = concat!(
            "event: message_start\ndata: {\"message\":{\"usage\":{\"input_tokens\":1,\"output_tokens\":0}}}\n\n",
            "event: content_block_start\ndata: {\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"a\",\"name\":\"read_file\",\"input\":{}}}\n\n",
            "event: content_block_start\ndata: {\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"b\",\"name\":\"git_diff\",\"input\":{}}}\n\n",
            "event: message_stop\ndata: {}\n\n",
        );
        let mut cur = Cursor::new(body.as_bytes().to_vec());
        let mut events = NoopStreamEvents;
        let result = super::parse_anthropic_stream(&mut cur, &mut events);
        let err = result.unwrap_err();
        assert!(
            err.to_string().contains("re-emitted content_block_start"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn parse_openai_stream_handles_empty_tool_arguments() {
        // Tool with required:[] schema (e.g. git_diff) — model may emit no
        // function.arguments at all, leaving assembly.arguments empty.
        let body = concat!(
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"x\",\"type\":\"function\",\"function\":{\"name\":\"git_diff\"}}]},\"finish_reason\":null}]}\n\n",
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n",
            "data: [DONE]\n\n",
        );
        let mut cur = Cursor::new(body.as_bytes().to_vec());
        let mut events = NoopStreamEvents;
        let (resp, _usage) = super::parse_openai_stream(&mut cur, &mut events).unwrap();
        match resp.action {
            super::ModelAction::CallTool { tool_name, input } => {
                assert_eq!(tool_name, "git_diff");
                assert!(input.args.is_empty());
            }
            super::ModelAction::Finish => panic!("expected tool call"),
        }
    }

    #[test]
    fn parse_anthropic_stream_handles_empty_tool_input_partial_json() {
        // tool_use with no input_json_delta events emits an empty partial_json.
        let body = concat!(
            "event: message_start\ndata: {\"message\":{\"usage\":{\"input_tokens\":1,\"output_tokens\":0}}}\n\n",
            "event: content_block_start\ndata: {\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"a\",\"name\":\"git_diff\",\"input\":{}}}\n\n",
            "event: content_block_stop\ndata: {\"index\":0}\n\n",
            "event: message_stop\ndata: {}\n\n",
        );
        let mut cur = Cursor::new(body.as_bytes().to_vec());
        let mut events = NoopStreamEvents;
        let (resp, _usage) = super::parse_anthropic_stream(&mut cur, &mut events).unwrap();
        match resp.action {
            super::ModelAction::CallTool { tool_name, input } => {
                assert_eq!(tool_name, "git_diff");
                assert!(input.args.is_empty());
            }
            super::ModelAction::Finish => panic!("expected tool call"),
        }
    }

    #[test]
    fn parse_openai_stream_collects_usage_frame_after_done_marker() {
        // Some compatible servers emit usage AFTER [DONE]. We continue
        // draining and capture it.
        let body = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"ok\"},\"finish_reason\":null}]}\n\n",
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
            "data: [DONE]\n\n",
            "data: {\"usage\":{\"prompt_tokens\":11,\"completion_tokens\":2}}\n\n",
        );
        let mut cur = Cursor::new(body.as_bytes().to_vec());
        let mut events = NoopStreamEvents;
        let (_resp, usage) = super::parse_openai_stream(&mut cur, &mut events).unwrap();
        let usage = usage.expect("expected usage from trailing frame");
        assert_eq!(usage.prompt, 11);
        assert_eq!(usage.completion, 2);
    }

    #[test]
    fn respond_offline_fallback_only_runs_when_api_key_missing() {
        // Ensure no DSCODE_TEST_NO_KEY is exported (planner() uses this env var name).
        let original = std::env::var("DSCODE_TEST_NO_KEY").ok();
        std::env::remove_var("DSCODE_TEST_NO_KEY");

        let request = ModelRequest {
            system_prompt: String::new(),
            task: "say hi".to_string(),
            profile_name: "generic".to_string(),
            profile_hints: Vec::new(),
            primary_file: None,
            suggested_test_command: None,
            available_tools: vec![],
            observations: vec![],
            todos: Vec::new(),
            planning_mode: false,
            recent_steps: Vec::new(),
        };

        let mut events = CapturingEvents::default();
        let (_resp, _usage) = planner().respond(request, &mut events).unwrap();

        // Trait contract: exactly one on_assistant_done in the offline path.
        assert_eq!(
            events.done.borrow().len(),
            1,
            "expected exactly one on_assistant_done call in offline fallback"
        );

        if let Some(value) = original {
            std::env::set_var("DSCODE_TEST_NO_KEY", value);
        }
    }

    #[test]
    fn json_object_to_string_args_re_serializes_nested_array_values() {
        let body = r#"{"items":[{"content":"Run tests","status":"pending"}]}"#;
        let value = crate::util::json::parse_json_value(body).unwrap();
        let args = super::json_object_to_string_args(&value).unwrap();
        let items_str = args.get("items").expect("items present");
        let reparsed = crate::util::json::parse_json_value(items_str).unwrap();
        match reparsed {
            crate::util::json::JsonValue::Array(a) => {
                assert_eq!(a.len(), 1);
            }
            _ => panic!("expected array, got {reparsed:?}"),
        }
    }
}
