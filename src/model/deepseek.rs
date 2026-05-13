use std::borrow::Cow;
use std::collections::BTreeMap;
use std::env;
use std::io::{self, BufRead, Read};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread;
use std::time::Duration;

use crate::config::types::ModelConfig;
use crate::error::app_error;
use crate::error::tool_failure;
use crate::error::AppResult;
use crate::model::client::ModelClient;
use crate::model::protocol::{ImageInput, ModelAction, ModelRequest, ModelResponse, TokenUsage};
use crate::tools::types::ToolInput;
use crate::ui::stream::StreamEvents;
use crate::util::cancel::CancellationCheck;
use crate::util::json::{
    json_as_array, json_as_object, json_as_string, json_as_u64, json_escape, json_value_to_string,
    parse_root_object, JsonValue,
};
use crate::util::process::StreamingProcess;
use crate::util::sse::{read_frame, SseFrame};

pub struct DeepSeekClient {
    pub config: ModelConfig,
}

impl ModelClient for DeepSeekClient {
    fn respond(
        &self,
        input: ModelRequest,
        events: &mut dyn crate::ui::stream::StreamEvents,
    ) -> AppResult<(ModelResponse, Option<TokenUsage>)> {
        self.respond_with_cancel(input, events, None)
    }

    fn respond_with_cancel(
        &self,
        input: ModelRequest,
        events: &mut dyn crate::ui::stream::StreamEvents,
        mut cancel_check: Option<&mut dyn CancellationCheck>,
    ) -> AppResult<(ModelResponse, Option<TokenUsage>)> {
        let api_key = env::var(&self.config.api_key_env)
            .ok()
            .filter(|key| !key.trim().is_empty());

        if let Some(api_key) = api_key {
            // Remote stream attempted: surface success or error directly.
            // Stream errors propagate so partial text isn't double-rendered
            // by the offline fallback (per StreamEvents "exactly once" contract).
            return self.respond_remote(&input, &api_key, events, cancel_check);
        }

        poll_model_cancel(&mut cancel_check)?;
        // No API key configured → run offline planner and drive events.
        let response = self.respond_offline(input);
        events.on_text_delta(&response.message);
        events.on_assistant_done(&response.message);
        if let ModelAction::CallTool { tool_name, input } = &response.action {
            events.on_tool_call(tool_name, &input.args);
        }
        poll_model_cancel(&mut cancel_check)?;
        Ok((response, None))
    }
}

impl DeepSeekClient {
    fn respond_remote(
        &self,
        input: &ModelRequest,
        api_key: &str,
        events: &mut dyn StreamEvents,
        cancel_check: Option<&mut dyn CancellationCheck>,
    ) -> AppResult<(ModelResponse, Option<TokenUsage>)> {
        let route = ModelRoute::resolve(&self.config, input);
        match api_flavor(&self.config.base_url) {
            ApiFlavor::OpenAi => {
                self.respond_remote_openai(input, api_key, events, &route, cancel_check)
            }
            ApiFlavor::Anthropic => {
                self.respond_remote_anthropic(input, api_key, events, &route, cancel_check)
            }
        }
    }

    fn respond_remote_openai(
        &self,
        input: &ModelRequest,
        api_key: &str,
        events: &mut dyn StreamEvents,
        route: &ModelRoute,
        cancel_check: Option<&mut dyn CancellationCheck>,
    ) -> AppResult<(ModelResponse, Option<TokenUsage>)> {
        let endpoint = format!(
            "{}/chat/completions",
            self.config.base_url.trim_end_matches('/')
        );
        let system_prompt = build_openai_tool_system_prompt(&input.system_prompt);
        let user_prompt = build_user_prompt(input);
        let user_message = build_openai_user_message(input, &user_prompt, &self.config)?;
        let reasoning = route.reasoning;
        let temperature_field = if reasoning.thinking_enabled() {
            ""
        } else {
            "\"temperature\":0,"
        };
        let tool_fields = openai_tool_fields(&input.available_tools, reasoning);
        let reasoning_fields = reasoning.openai_fields();
        let body = format!(
            concat!(
                "{{",
                "\"model\":\"{}\",",
                "{}",
                "{}",
                "\"max_tokens\":1024,",
                "\"stream\":true,",
                "\"stream_options\":{{\"include_usage\":true}},",
                "{}",
                "\"messages\":[",
                "{{\"role\":\"system\",\"content\":\"{}\"}},",
                "{}",
                "]",
                "}}"
            ),
            json_escape(&route.model),
            temperature_field,
            reasoning_fields,
            tool_fields,
            json_escape(&system_prompt),
            user_message,
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
            "@-",
        ];

        let mut process =
            match crate::util::process::spawn_streaming_with_stdin("curl", &args, body.as_str()) {
                Ok(p) => p,
                Err(error) => {
                    events.on_assistant_done("");
                    return Err(error);
                }
            };
        let parsed = parse_openai_process_stream(&mut process, events, cancel_check);
        if parsed.is_err() {
            drop(process);
            return attach_usage_model(parsed, &route.model);
        }
        let (status, stderr_tail) = process.finish()?;
        if !status.success() {
            return Err(tool_failure(format!(
                "deepseek openai stream failed (exit {:?}): {}",
                status.code(),
                stderr_tail.trim()
            )));
        }
        attach_usage_model(parsed, &route.model)
    }

    fn respond_remote_anthropic(
        &self,
        input: &ModelRequest,
        api_key: &str,
        events: &mut dyn StreamEvents,
        route: &ModelRoute,
        cancel_check: Option<&mut dyn CancellationCheck>,
    ) -> AppResult<(ModelResponse, Option<TokenUsage>)> {
        let endpoint = format!("{}/messages", self.config.base_url.trim_end_matches('/'));
        let system_prompt = build_anthropic_tool_system_prompt(&input.system_prompt);
        let user_prompt = build_user_prompt(input);
        let user_content = build_anthropic_user_content(input, &user_prompt, &self.config)?;
        let reasoning = route.reasoning;
        let tool_fields = anthropic_tool_fields(&input.available_tools, reasoning);
        let reasoning_fields = reasoning.anthropic_fields();
        let body = format!(
            concat!(
                "{{",
                "\"model\":\"{}\",",
                "\"max_tokens\":1024,",
                "\"stream\":true,",
                "{}",
                "{}",
                "\"system\":\"{}\",",
                "\"messages\":[",
                "{{\"role\":\"user\",\"content\":{}}}",
                "]",
                "}}"
            ),
            json_escape(&route.model),
            reasoning_fields,
            tool_fields,
            json_escape(&system_prompt),
            user_content,
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
            "@-",
        ];

        let mut process =
            match crate::util::process::spawn_streaming_with_stdin("curl", &args, body.as_str()) {
                Ok(p) => p,
                Err(error) => {
                    events.on_assistant_done("");
                    return Err(error);
                }
            };
        let parsed = parse_anthropic_process_stream(&mut process, events, cancel_check);
        if parsed.is_err() {
            drop(process);
            return attach_usage_model(parsed, &route.model);
        }
        let (status, stderr_tail) = process.finish()?;
        if !status.success() {
            return Err(tool_failure(format!(
                "deepseek anthropic stream failed (exit {:?}): {}",
                status.code(),
                stderr_tail.trim()
            )));
        }
        attach_usage_model(parsed, &route.model)
    }

    fn respond_offline(&self, input: ModelRequest) -> ModelResponse {
        let route = ModelRoute::resolve(&self.config, &input);
        let model_name = route.model.as_str();
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
        let github_pr_context_request = derive_github_pr_context_request(&task);
        let remote_pr_review_requested = task_requests_remote_pr_review(&task_lower)
            || (task_requests_remote_pr_comment_plan(&task_lower)
                && task_looks_like_pr_workflow(&task_lower)
                && github_pr_context_request.is_some());
        let latest_github_pr_context =
            latest_successful_tool_summary(&input.observations, "github_pr_context");
        let latest_review_output = latest_successful_tool_summary(&input.observations, "review");
        let latest_pr_review_comment_plan =
            latest_successful_tool_summary(&input.observations, "pr_review_comment_plan");
        let last_failed_github_comment =
            last_failed_tool_summary(&input.observations, "github_comment");
        let last_failed_github_pr_review_comment =
            last_failed_tool_summary(&input.observations, "github_pr_review_comment");
        let last_failed_pr_comment_write =
            last_failed_github_comment.or(last_failed_github_pr_review_comment);
        let pr_review_comment_plan_call_count =
            tool_call_count(&input.observations, "pr_review_comment_plan");
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
                    model_name
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
                    model_name
                ),
                action: ModelAction::Finish,
            };
        }

        if let Some(replan_response) = build_replan_response(model_name, &input) {
            return replan_response;
        }

        if let Some(recovery_response) = build_recovery_response(
            model_name,
            &input,
            &used_tools,
            &succeeded_tools,
            search_query.as_deref(),
        ) {
            return recovery_response;
        }

        if let Some(git_response) =
            build_git_history_response(model_name, &input, &used_tools, &task_lower)
        {
            return git_response;
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
                    model_name
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
                        model_name, test_command
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
                    model_name
                ),
                action: ModelAction::CallTool {
                    tool_name: "dispatch_subagent".to_string(),
                    input: ToolInput::new()
                        .with_arg("task", subagent_task)
                        .with_arg("steps", "2"),
                },
            };
        }

        if edit_request.is_none() && remote_pr_review_requested {
            if let Some(pr_request) = github_pr_context_request.as_ref() {
                if tool_available("github_pr_context")
                    && !used_tools.contains("github_pr_context")
                    && !observations_include_pr_review_signal(&input.observations)
                {
                    let mut tool_input = ToolInput::new()
                        .with_arg("number", pr_request.number.clone())
                        .with_arg("include_diff", "true");
                    if let Some(repo) = pr_request.repo.as_ref() {
                        tool_input = tool_input.with_arg("repo", repo.clone());
                    }
                    return ModelResponse {
                        message: format!(
                            "{} planner is gathering GitHub PR context with the patch diff before review.",
                            model_name
                        ),
                        action: ModelAction::CallTool {
                            tool_name: "github_pr_context".to_string(),
                            input: tool_input,
                        },
                    };
                }
            }
            if tool_available("review") && !used_tools.contains("review") {
                if let Some(context) = latest_github_pr_context {
                    let mut tool_input = ToolInput::new()
                        .with_arg("target", "github_pr_context")
                        .with_arg("github_context", context.to_string());
                    if task_requests_semantic_review(&task_lower) {
                        tool_input = tool_input.with_arg("semantic", "true");
                    }
                    return ModelResponse {
                        message: format!(
                            "{} planner is running structured review over the gathered GitHub PR context.",
                            model_name
                        ),
                        action: ModelAction::CallTool {
                            tool_name: "review".to_string(),
                            input: tool_input,
                        },
                    };
                }
            }
            if succeeded_tools.contains("review")
                && task_requests_remote_pr_comment_plan(&task_lower)
                && tool_available("pr_review_comment_plan")
                && !used_tools.contains("pr_review_comment_plan")
            {
                if let Some(review_output) = latest_review_output {
                    let mut tool_input =
                        ToolInput::new().with_arg("review_output", review_output.to_string());
                    if let Some(context) = latest_github_pr_context {
                        tool_input = tool_input.with_arg("pr_context", context.to_string());
                    }
                    if let Some(pr_request) = github_pr_context_request.as_ref() {
                        tool_input = tool_input.with_arg("number", pr_request.number.clone());
                        if let Some(repo) = pr_request.repo.as_ref() {
                            tool_input = tool_input.with_arg("repo", repo.clone());
                        }
                    }
                    return ModelResponse {
                        message: format!(
                            "{} planner is drafting an evidence-backed PR review comment plan.",
                            model_name
                        ),
                        action: ModelAction::CallTool {
                            tool_name: "pr_review_comment_plan".to_string(),
                            input: tool_input,
                        },
                    };
                }
            }
            if succeeded_tools.contains("pr_review_comment_plan")
                && task_requests_remote_pr_inline_comment_post(&task_lower)
                && tool_available("github_pr_review_comment")
                && !used_tools.contains("github_pr_review_comment")
            {
                if let Some(plan) = latest_pr_review_comment_plan {
                    if let Some(tool_input) =
                        github_pr_review_comment_input_from_pr_review_comment_plan(plan, false)
                    {
                        return ModelResponse {
                            message: format!(
                                "{} planner is posting prepared inline PR review comments through the guarded GitHub write tool.",
                                model_name
                            ),
                            action: ModelAction::CallTool {
                                tool_name: "github_pr_review_comment".to_string(),
                                input: tool_input,
                            },
                        };
                    }
                }
            }
            if succeeded_tools.contains("pr_review_comment_plan")
                && task_requests_remote_pr_comment_post(&task_lower)
                && !task_requests_remote_pr_inline_comment_post(&task_lower)
                && tool_available("github_comment")
                && !used_tools.contains("github_comment")
            {
                if let Some(plan) = latest_pr_review_comment_plan {
                    if let Some(tool_input) =
                        github_comment_input_from_pr_review_comment_plan(plan, false)
                    {
                        return ModelResponse {
                            message: format!(
                                "{} planner is posting the prepared PR review comment through the guarded GitHub write tool.",
                                model_name
                            ),
                            action: ModelAction::CallTool {
                                tool_name: "github_comment".to_string(),
                                input: tool_input,
                            },
                        };
                    }
                }
            }
            if let Some(comment_error) = last_failed_pr_comment_write {
                if task_requests_remote_pr_comment_post(&task_lower)
                    && tool_available("pr_review_comment_plan")
                    && pr_review_comment_plan_call_count < 2
                {
                    if let Some(review_output) = latest_review_output {
                        let mut tool_input = ToolInput::new()
                            .with_arg("review_output", review_output.to_string())
                            .with_arg("comment_error", comment_error.to_string());
                        if let Some(context) = latest_github_pr_context {
                            tool_input = tool_input.with_arg("pr_context", context.to_string());
                        }
                        if let Some(pr_request) = github_pr_context_request.as_ref() {
                            tool_input = tool_input.with_arg("number", pr_request.number.clone());
                            if let Some(repo) = pr_request.repo.as_ref() {
                                tool_input = tool_input.with_arg("repo", repo.clone());
                            }
                        }
                        return ModelResponse {
                            message: format!(
                                "{} planner is rebuilding the PR review comment plan after the guarded GitHub comment attempt failed.",
                                model_name
                            ),
                            action: ModelAction::CallTool {
                                tool_name: "pr_review_comment_plan".to_string(),
                                input: tool_input,
                            },
                        };
                    }
                }
            }
            if succeeded_tools.contains("github_comment") {
                return ModelResponse {
                    message: format!(
                        "{} offline planner completed the guarded PR review comment step.",
                        model_name
                    ),
                    action: ModelAction::Finish,
                };
            }
            if succeeded_tools.contains("github_pr_review_comment") {
                return ModelResponse {
                    message: format!(
                        "{} offline planner completed the guarded inline PR review comment step.",
                        model_name
                    ),
                    action: ModelAction::Finish,
                };
            }
            if succeeded_tools.contains("pr_review_comment_plan") {
                return ModelResponse {
                    message: format!(
                        "{} offline planner completed the structured PR review comment plan.",
                        model_name
                    ),
                    action: ModelAction::Finish,
                };
            }
            if succeeded_tools.contains("review") {
                return ModelResponse {
                    message: format!(
                        "{} offline planner completed the structured PR review pass.",
                        model_name
                    ),
                    action: ModelAction::Finish,
                };
            }
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
                            model_name
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
                        message: format!("{} planner is searching for `{query}`.", model_name),
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
                            model_name
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
                            model_name, edit_request.path
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
                    model_name
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
                    model_name
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
                            model_name
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
                        model_name
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
                            model_name, retry_request.path
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
                                model_name, edit_request.path
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
                            model_name, edit_request.path
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
                            model_name, edit_request.path
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
                        message: format!("{} planner is reading the primary file.", model_name),
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
                message: format!("{} planner is reviewing the resulting diff.", model_name),
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
                        model_name, test_command
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
            model_name,
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

fn attach_usage_model(
    parsed: AppResult<(ModelResponse, Option<TokenUsage>)>,
    model: &str,
) -> AppResult<(ModelResponse, Option<TokenUsage>)> {
    let (response, usage) = parsed?;
    Ok((
        response,
        usage.map(|mut usage| {
            if usage.model.is_none() {
                usage.model = Some(model.to_string());
            }
            usage
        }),
    ))
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ModelRoute {
    model: String,
    reasoning: ReasoningTier,
}

impl ModelRoute {
    fn resolve(config: &ModelConfig, input: &ModelRequest) -> Self {
        let complexity = RouteComplexity::classify(input);
        let model = if is_auto_model(&config.model) {
            match complexity {
                RouteComplexity::Simple => "deepseek-v4-flash".to_string(),
                RouteComplexity::Complex | RouteComplexity::Deep => "deepseek-v4-pro".to_string(),
            }
        } else {
            config.model.trim().to_string()
        };
        let reasoning = if is_auto_reasoning(&config.reasoning_effort) {
            match complexity {
                RouteComplexity::Simple => ReasoningTier::Off,
                RouteComplexity::Complex => ReasoningTier::High,
                RouteComplexity::Deep => ReasoningTier::Max,
            }
        } else {
            ReasoningTier::from_config(&config.reasoning_effort)
        };
        Self { model, reasoning }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RouteComplexity {
    Simple,
    Complex,
    Deep,
}

impl RouteComplexity {
    fn classify(input: &ModelRequest) -> Self {
        let mut score = 0u8;
        let text = route_text(input);
        if input.planning_mode {
            score = score.saturating_add(2);
        }
        if !input.image_inputs.is_empty() {
            score = score.saturating_add(2);
        }
        if input
            .observations
            .iter()
            .any(|observation| observation.is_failure())
        {
            score = score.saturating_add(2);
        }
        if input.observations.len() >= 4 {
            score = score.saturating_add(2);
        } else if input.observations.len() >= 2 {
            score = score.saturating_add(1);
        }
        if input.todos.len() >= 4 {
            score = score.saturating_add(1);
        }
        if text.len() >= 2_000 {
            score = score.saturating_add(2);
        } else if text.len() >= 800 {
            score = score.saturating_add(1);
        }
        for needle in [
            "architecture",
            "architectural",
            "设计",
            "架构",
            "threat model",
            "security",
            "安全",
            "migration",
            "migrate",
            "重构",
            "refactor",
            "performance",
            "性能",
            "review",
            "audit",
            "parity",
            "差距",
            "roadmap",
            "multi-step",
            "complex",
            "复杂",
        ] {
            if text.contains(needle) {
                score = score.saturating_add(2);
            }
        }
        for needle in ["quick", "simple", "trivial", "read-only", "lookup", "简单"] {
            if text.contains(needle) {
                score = score.saturating_sub(1);
            }
        }

        if score >= 6 {
            Self::Deep
        } else if score >= 3 {
            Self::Complex
        } else {
            Self::Simple
        }
    }
}

fn route_text(input: &ModelRequest) -> String {
    let mut text = input.task.to_ascii_lowercase();
    text.push(' ');
    text.push_str(&input.profile_name.to_ascii_lowercase());
    for hint in &input.profile_hints {
        text.push(' ');
        text.push_str(&hint.to_ascii_lowercase());
    }
    text
}

fn is_auto_model(model: &str) -> bool {
    matches!(
        model.trim().to_ascii_lowercase().as_str(),
        "auto" | "auto-deepseek" | "deepseek-auto"
    )
}

fn is_auto_reasoning(value: &str) -> bool {
    matches!(value.trim().to_ascii_lowercase().as_str(), "auto")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReasoningTier {
    Off,
    High,
    Max,
}

impl ReasoningTier {
    fn from_config(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "high" | "low" | "medium" | "enabled" | "on" => Self::High,
            "max" | "xhigh" => Self::Max,
            _ => Self::Off,
        }
    }

    fn thinking_enabled(self) -> bool {
        !matches!(self, Self::Off)
    }

    fn openai_fields(self) -> &'static str {
        match self {
            Self::Off => "\"thinking\":{\"type\":\"disabled\"},",
            Self::High => "\"thinking\":{\"type\":\"enabled\"},\"reasoning_effort\":\"high\",",
            Self::Max => "\"thinking\":{\"type\":\"enabled\"},\"reasoning_effort\":\"max\",",
        }
    }

    fn anthropic_fields(self) -> &'static str {
        match self {
            Self::Off => "",
            Self::High => "\"output_config\":{\"effort\":\"high\"},",
            Self::Max => "\"output_config\":{\"effort\":\"max\"},",
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

fn build_openai_user_message(
    input: &ModelRequest,
    user_prompt: &str,
    config: &ModelConfig,
) -> AppResult<String> {
    if input.image_inputs.is_empty() || !supports_native_image_input(config) {
        return Ok(format!(
            r#"{{"role":"user","content":"{}"}}"#,
            json_escape(user_prompt)
        ));
    }

    let mut parts = vec![format!(
        r#"{{"type":"text","text":"{}"}}"#,
        json_escape(user_prompt)
    )];
    for image in &input.image_inputs {
        validate_supported_image_media_type(image)?;
        parts.push(format!(
            r#"{{"type":"image_url","image_url":{{"url":"data:{};base64,{}"}}}}"#,
            json_escape(&image.media_type),
            image.data_base64,
        ));
    }
    Ok(format!(
        r#"{{"role":"user","content":[{}]}}"#,
        parts.join(",")
    ))
}

fn build_anthropic_user_content(
    input: &ModelRequest,
    user_prompt: &str,
    config: &ModelConfig,
) -> AppResult<String> {
    let mut parts = Vec::new();
    if supports_native_image_input(config) {
        for image in &input.image_inputs {
            validate_supported_image_media_type(image)?;
            parts.push(format!(
                r#"{{"type":"image","source":{{"type":"base64","media_type":"{}","data":"{}"}}}}"#,
                json_escape(&image.media_type),
                image.data_base64,
            ));
        }
    }
    parts.push(format!(
        r#"{{"type":"text","text":"{}"}}"#,
        json_escape(user_prompt)
    ));
    Ok(format!("[{}]", parts.join(",")))
}

fn supports_native_image_input(config: &ModelConfig) -> bool {
    let model = config.model.to_ascii_lowercase();
    let base_url = config.base_url.to_ascii_lowercase();
    if model.contains("deepseek") || base_url.contains("deepseek") {
        return false;
    }
    match api_flavor(&config.base_url) {
        ApiFlavor::Anthropic => model.contains("claude"),
        ApiFlavor::OpenAi => {
            model.contains("codex")
                || model.contains("gpt-4")
                || model.contains("gpt-5")
                || model.contains("o3")
                || model.contains("o4")
        }
    }
}

fn validate_supported_image_media_type(image: &ImageInput) -> AppResult<()> {
    match image.media_type.as_str() {
        "image/jpeg" | "image/png" | "image/gif" | "image/webp" => Ok(()),
        other => Err(app_error(format!(
            "unsupported native image media type `{other}` for {}",
            image.path
        ))),
    }
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
        prompt.push_str("Recent agent steps (prior assistant messages and reasoning summaries, oldest first — do NOT repeat work already done):\n");
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
    let product_readiness = task_looks_like_product_readiness(&task_lower);

    if let Some(reason) = replan_reason {
        let reason = clip_reason_for_todo(reason);
        steps.push((
            format!("Reassess the plan using the latest blocker or recovery signal ({reason})"),
            "Reassessing the plan using the latest blocker or recovery signal".to_string(),
        ));
    }

    if product_readiness {
        steps.push((
            "Assess current capability gaps against the target product behavior".to_string(),
            "Assessing current capability gaps against the target product behavior".to_string(),
        ));
    } else if failure_repro_first && tool_available("run_shell") {
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

    if product_readiness && tool_available("read_file") {
        steps.push((
            "Read the implementation and roadmap sections behind the selected gap".to_string(),
            "Reading the implementation and roadmap sections behind the selected gap".to_string(),
        ));
    } else if tool_available("read_file") {
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
    } else if product_readiness && tool_available("apply_patch") {
        steps.push((
            "Implement the smallest high-impact product gap closure slice".to_string(),
            "Implementing the smallest high-impact product gap closure slice".to_string(),
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

    if product_readiness && tool_available("run_shell") {
        steps.push((
            "Validate the product gap slice with tests or benchmark".to_string(),
            "Validating the product gap slice with tests or benchmark".to_string(),
        ));
    } else if let Some(command) = input.suggested_test_command.as_deref() {
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

fn build_git_history_response(
    model_name: &str,
    input: &ModelRequest,
    used_tools: &std::collections::BTreeSet<&str>,
    task_lower: &str,
) -> Option<ModelResponse> {
    let tool_available = |name: &str| input.available_tools.iter().any(|tool| tool == name);
    if used_tools.contains("git_log")
        || used_tools.contains("git_show")
        || used_tools.contains("git_blame")
    {
        return Some(ModelResponse {
            message: format!(
                "{model_name} offline planner inspected the requested Git history and is stopping."
            ),
            action: ModelAction::Finish,
        });
    }

    if task_requests_git_blame(task_lower) && tool_available("git_blame") {
        let path = derive_git_path(input)?;
        let mut tool_input = ToolInput::new().with_arg("path", path.clone());
        if let Some(line) = derive_line_number(task_lower) {
            let line = line.to_string();
            tool_input = tool_input
                .with_arg("line_start", line.clone())
                .with_arg("line_end", line);
        }
        return Some(ModelResponse {
            message: format!("{model_name} planner is inspecting git blame for `{path}`."),
            action: ModelAction::CallTool {
                tool_name: "git_blame".to_string(),
                input: tool_input,
            },
        });
    }

    if task_requests_git_show(task_lower) && tool_available("git_show") {
        let mut tool_input = ToolInput::new();
        if let Some(path) = derive_git_path(input) {
            tool_input = tool_input.with_arg("path", path);
        }
        return Some(ModelResponse {
            message: format!("{model_name} planner is showing the requested git revision."),
            action: ModelAction::CallTool {
                tool_name: "git_show".to_string(),
                input: tool_input,
            },
        });
    }

    if task_requests_git_log(task_lower) && tool_available("git_log") {
        let mut tool_input = ToolInput::new().with_arg("limit", "10");
        if let Some(path) = derive_git_path(input) {
            tool_input = tool_input.with_arg("path", path);
        }
        return Some(ModelResponse {
            message: format!("{model_name} planner is reading recent git history."),
            action: ModelAction::CallTool {
                tool_name: "git_log".to_string(),
                input: tool_input,
            },
        });
    }

    None
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

fn task_requests_git_log(task_lower: &str) -> bool {
    task_lower.contains("git log")
        || task_lower.contains("commit history")
        || task_lower.contains("recent commits")
        || task_lower.contains("recent git commits")
}

fn task_requests_git_show(task_lower: &str) -> bool {
    task_lower.contains("git show")
        || task_lower.contains("show head")
        || task_lower.contains("show latest commit")
        || task_lower.contains("show last commit")
}

fn task_requests_git_blame(task_lower: &str) -> bool {
    task_lower.contains("git blame") || task_lower.starts_with("blame ")
}

fn derive_git_path(input: &ModelRequest) -> Option<String> {
    input
        .primary_file
        .clone()
        .or_else(|| path_like_tokens(&input.task).into_iter().next())
}

fn derive_line_number(task_lower: &str) -> Option<usize> {
    for marker in ["line ", "lines "] {
        let Some(index) = task_lower.find(marker) else {
            continue;
        };
        let digits = task_lower[index + marker.len()..]
            .chars()
            .skip_while(|ch| !ch.is_ascii_digit())
            .take_while(|ch| ch.is_ascii_digit())
            .collect::<String>();
        if let Ok(line) = digits.parse::<usize>() {
            if line > 0 {
                return Some(line);
            }
        }
    }
    None
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
        matches!(
            observation.tool_name.as_str(),
            "git_diff" | "list_files" | "github_pr_context" | "review"
        ) && !observation.is_failure()
    })
}

fn latest_successful_tool_summary<'a>(
    observations: &'a [crate::model::protocol::Observation],
    tool_name: &str,
) -> Option<&'a str> {
    observations
        .iter()
        .rev()
        .find(|observation| observation.tool_name == tool_name && !observation.is_failure())
        .map(|observation| observation.summary.as_str())
}

fn last_failed_tool_summary<'a>(
    observations: &'a [crate::model::protocol::Observation],
    tool_name: &str,
) -> Option<&'a str> {
    observations
        .last()
        .filter(|observation| observation.tool_name == tool_name && observation.is_failure())
        .map(|observation| observation.summary.as_str())
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

fn task_looks_like_product_readiness(task_lower: &str) -> bool {
    [
        " more like ",
        "close the gap",
        "gap closure",
        "production-ready",
        "production ready",
        "product-ready",
        "product ready",
        "productize",
        "productionize",
        "ship-ready",
        "ship ready",
        "daily coding",
        "daily use",
        "match codex",
        "match claude",
    ]
    .iter()
    .any(|marker| task_lower.contains(marker))
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

fn task_requests_remote_pr_review(task_lower: &str) -> bool {
    (task_lower.contains("review pull request")
        || task_lower.contains("review pr")
        || (task_lower.contains("pull request") && task_lower.contains("review")))
        && !task_lower.contains("review feedback")
        && !task_lower.contains("address review")
}

fn task_requests_remote_pr_comment_plan(task_lower: &str) -> bool {
    task_lower.contains("comment")
        || task_lower.contains("draft")
        || task_lower.contains("post a review")
        || task_lower.contains("review reply")
        || task_lower.contains("review response")
}

fn task_requests_semantic_review(task_lower: &str) -> bool {
    task_lower.contains("semantic review")
        || task_lower.contains("semantic code review")
        || task_lower.contains("deep review")
        || task_lower.contains("thorough review")
        || task_lower.contains("behavioral review")
        || task_lower.contains("real bug")
        || task_lower.contains("logic bug")
}

fn task_requests_remote_pr_comment_post(task_lower: &str) -> bool {
    !task_lower.contains("draft")
        && !task_lower.contains("prepare")
        && !task_lower.contains("plan only")
        && (task_lower.contains("post")
            || task_lower.contains("publish")
            || task_lower.contains("leave a comment")
            || task_lower.contains("add a comment")
            || task_lower.contains("submit comment")
            || task_lower.contains("send comment"))
}

fn task_requests_remote_pr_inline_comment_post(task_lower: &str) -> bool {
    task_requests_remote_pr_comment_post(task_lower)
        && (task_lower.contains("inline")
            || task_lower.contains("line comment")
            || task_lower.contains("file comment")
            || task_lower.contains("diff comment"))
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct GithubPrContextRequest {
    number: String,
    repo: Option<String>,
}

fn derive_github_pr_context_request(task: &str) -> Option<GithubPrContextRequest> {
    if let Some(request) = parse_github_pr_url_from_text(task) {
        return Some(request);
    }
    let number = first_hash_number(task)?;
    Some(GithubPrContextRequest {
        number,
        repo: first_github_repo_token(task),
    })
}

fn parse_github_pr_url_from_text(task: &str) -> Option<GithubPrContextRequest> {
    let start = task.find("https://github.com/")?;
    let tail = &task[start + "https://github.com/".len()..];
    let token = tail
        .split_whitespace()
        .next()
        .unwrap_or("")
        .trim_matches(|ch: char| matches!(ch, ')' | ']' | ',' | '.' | ';' | '\'' | '"'));
    let mut parts = token.split('/');
    let owner = parts.next()?;
    let repo = parts.next()?;
    let kind = parts.next()?;
    let number = parts
        .next()
        .unwrap_or("")
        .split(['?', '#'])
        .next()
        .unwrap_or("");
    if kind != "pull" || owner.is_empty() || repo.is_empty() || number.is_empty() {
        return None;
    }
    if !number.chars().all(|ch| ch.is_ascii_digit()) {
        return None;
    }
    Some(GithubPrContextRequest {
        number: number.to_string(),
        repo: Some(format!("{owner}/{repo}")),
    })
}

fn github_comment_input_from_pr_review_comment_plan(
    plan: &str,
    dry_run: bool,
) -> Option<ToolInput> {
    let root = parse_root_object(plan).ok()?;
    let input_object = root
        .get("github_comment_input")
        .and_then(json_as_object)
        .unwrap_or(&root);
    let target = input_object
        .get("target")
        .and_then(json_as_string)
        .unwrap_or("pr");
    let number = input_object
        .get("number")
        .and_then(json_as_string)
        .or_else(|| root.get("number").and_then(json_as_string))?;
    let body = input_object
        .get("body")
        .and_then(json_as_string)
        .or_else(|| root.get("comment_body").and_then(json_as_string))?;
    let evidence = input_object
        .get("evidence")
        .or_else(|| root.get("evidence"))
        .map(json_string_or_serialized)?;
    let mut tool_input = ToolInput::new()
        .with_arg("target", target.to_string())
        .with_arg("number", number.to_string())
        .with_arg("body", body.to_string())
        .with_arg("evidence", evidence)
        .with_arg("dry_run", if dry_run { "true" } else { "false" });
    if let Some(repo) = input_object
        .get("repo")
        .and_then(json_as_string)
        .or_else(|| root.get("repo").and_then(json_as_string))
    {
        tool_input = tool_input.with_arg("repo", repo.to_string());
    }
    Some(tool_input)
}

fn github_pr_review_comment_input_from_pr_review_comment_plan(
    plan: &str,
    dry_run: bool,
) -> Option<ToolInput> {
    let root = parse_root_object(plan).ok()?;
    let input_object = root
        .get("github_pr_review_comment_input")
        .and_then(json_as_object)?;
    let number = input_object
        .get("number")
        .and_then(json_as_string)
        .or_else(|| root.get("number").and_then(json_as_string))?;
    let evidence = input_object
        .get("evidence")
        .or_else(|| root.get("evidence"))
        .map(json_string_or_serialized)?;
    let comments = input_object
        .get("comments")
        .map(json_string_or_serialized)
        .or_else(|| root.get("comments").map(json_string_or_serialized))?;
    let mut tool_input = ToolInput::new()
        .with_arg("number", number.to_string())
        .with_arg("comments", comments)
        .with_arg("evidence", evidence)
        .with_arg("dry_run", if dry_run { "true" } else { "false" });
    if let Some(commit_id) = input_object
        .get("commit_id")
        .and_then(json_as_string)
        .or_else(|| input_object.get("head_sha").and_then(json_as_string))
        .or_else(|| input_object.get("sha").and_then(json_as_string))
    {
        tool_input = tool_input.with_arg("commit_id", commit_id.to_string());
    }
    if let Some(repo) = input_object
        .get("repo")
        .and_then(json_as_string)
        .or_else(|| root.get("repo").and_then(json_as_string))
    {
        tool_input = tool_input.with_arg("repo", repo.to_string());
    }
    Some(tool_input)
}

fn json_string_or_serialized(value: &JsonValue) -> String {
    json_as_string(value)
        .map(str::to_string)
        .unwrap_or_else(|| json_value_to_string(value))
}

fn first_hash_number(task: &str) -> Option<String> {
    for (index, ch) in task.char_indices() {
        if ch != '#' {
            continue;
        }
        let digits = task[index + 1..]
            .chars()
            .take_while(|ch| ch.is_ascii_digit())
            .collect::<String>();
        if !digits.is_empty() {
            return Some(digits);
        }
    }
    None
}

fn first_github_repo_token(task: &str) -> Option<String> {
    task.split_whitespace()
        .map(|token| {
            token.trim_matches(|ch: char| {
                matches!(
                    ch,
                    '(' | ')' | '[' | ']' | '{' | '}' | ',' | '.' | ';' | ':' | '\'' | '"'
                )
            })
        })
        .find(|token| {
            let mut parts = token.split('/');
            let Some(owner) = parts.next() else {
                return false;
            };
            let Some(repo) = parts.next() else {
                return false;
            };
            parts.next().is_none() && valid_github_repo_part(owner) && valid_github_repo_part(repo)
        })
        .map(str::to_string)
}

fn valid_github_repo_part(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
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

fn openai_tool_fields(names: &[String], reasoning: ReasoningTier) -> String {
    if names.is_empty() {
        return String::new();
    }
    let tool_choice_field = if reasoning.thinking_enabled() {
        ""
    } else {
        "\"tool_choice\":\"auto\","
    };
    format!(
        "{}\"parallel_tool_calls\":false,\"tools\":{},",
        tool_choice_field,
        build_openai_tools(names)
    )
}

fn anthropic_tool_fields(names: &[String], reasoning: ReasoningTier) -> String {
    if names.is_empty() {
        return String::new();
    }
    let tool_choice_field = if reasoning.thinking_enabled() {
        ""
    } else {
        "\"tool_choice\":{\"type\":\"auto\"},"
    };
    format!(
        "{}\"tools\":{},",
        tool_choice_field,
        build_anthropic_tools(names)
    )
}

fn render_tools(names: &[String], envelope: fn(&ToolSpec) -> String) -> String {
    let tools = names
        .iter()
        .filter_map(|name| tool_spec(name).map(|spec| envelope(&spec)))
        .collect::<Vec<_>>();
    format!("[{}]", tools.join(","))
}

fn openai_envelope(spec: &ToolSpec) -> String {
    format!(
        r#"{{"type":"function","function":{{"name":"{}","description":"{}","parameters":{{"type":"object","properties":{},"required":{},"additionalProperties":false}}}}}}"#,
        json_escape(&spec.name),
        json_escape(&spec.description),
        spec.properties_json,
        spec.required_json,
    )
}

fn anthropic_envelope(spec: &ToolSpec) -> String {
    format!(
        r#"{{"name":"{}","description":"{}","input_schema":{{"type":"object","properties":{},"required":{}}}}}"#,
        json_escape(&spec.name),
        json_escape(&spec.description),
        spec.properties_json,
        spec.required_json,
    )
}

struct ToolSpec {
    name: Cow<'static, str>,
    description: Cow<'static, str>,
    properties_json: Cow<'static, str>,
    required_json: Cow<'static, str>,
}

fn tool_spec(name: &str) -> Option<ToolSpec> {
    TOOL_SPECS
        .iter()
        .find(|spec| spec.name == name)
        .map(|spec| ToolSpec {
            name: Cow::Borrowed(spec.name),
            description: Cow::Borrowed(spec.description),
            properties_json: Cow::Borrowed(spec.properties_json),
            required_json: Cow::Borrowed(spec.required_json),
        })
        .or_else(|| dynamic_mcp_tool_spec(name))
}

pub(crate) fn static_tool_search_catalog() -> Vec<(&'static str, &'static str, &'static str)> {
    TOOL_SPECS
        .iter()
        .map(|spec| (spec.name, spec.description, spec.properties_json))
        .collect()
}

struct StaticToolSpec {
    name: &'static str,
    description: &'static str,
    properties_json: &'static str,
    required_json: &'static str,
}

fn dynamic_mcp_tool_spec(name: &str) -> Option<ToolSpec> {
    if !name.starts_with(crate::tools::mcp::MCP_DYNAMIC_TOOL_PREFIX) {
        return None;
    }
    if let Some(schema) = crate::tools::mcp::dynamic_tool_schema(name) {
        if let Some(input_schema) = schema.input_schema.as_deref() {
            if let Some((properties_json, required_json)) =
                mcp_input_schema_to_tool_parts(input_schema)
            {
                return Some(ToolSpec {
                    name: Cow::Owned(name.to_string()),
                    description: Cow::Owned(schema.description.unwrap_or_else(|| {
                        format!("Call the configured MCP remote tool `{name}` directly.")
                    })),
                    properties_json: Cow::Owned(properties_json),
                    required_json: Cow::Owned(required_json),
                });
            }
        }
    }
    Some(ToolSpec {
        name: Cow::Owned(name.to_string()),
        description: Cow::Owned(format!(
            "Call the configured MCP remote tool `{name}` directly. Use mcp_list_tools first if you need its input schema."
        )),
        properties_json: Cow::Borrowed(
            r#"{"arguments":{"type":"string","description":"JSON object string containing remote tool arguments, for example {\"path\":\"README.md\"}. Use {} when the remote tool takes no arguments."}}"#,
        ),
        required_json: Cow::Borrowed(r#"[]"#),
    })
}

fn mcp_input_schema_to_tool_parts(input_schema: &str) -> Option<(String, String)> {
    let root = crate::util::json::parse_root_object(input_schema).ok()?;
    if root.get("type").and_then(json_as_string) != Some("object") {
        return None;
    }
    let properties = root.get("properties").and_then(json_as_object)?;
    let required = root
        .get("required")
        .and_then(json_as_array)
        .cloned()
        .unwrap_or_default();
    Some((
        json_value_to_string(&JsonValue::Object(properties.clone())),
        json_value_to_string(&JsonValue::Array(required)),
    ))
}

const TOOL_SPECS: &[StaticToolSpec] = &[
    StaticToolSpec {
        name: "list_files",
        description: "List repository files and directories under a root path.",
        properties_json: r#"{"root":{"type":"string","description":"Root directory to list from, usually `.`."},"max_depth":{"type":"string","description":"Maximum directory depth to traverse, encoded as a string integer."},"limit":{"type":"string","description":"Maximum number of entries to return, encoded as a string integer."}}"#,
        required_json: r#"["root","max_depth","limit"]"#,
    },
    StaticToolSpec {
        name: "list_dir",
        description: "DeepSeek-TUI-compatible alias for listing repository files and directories under a path.",
        properties_json: r#"{"path":{"type":"string","description":"Directory to list from, usually `.`. Mapped to list_files root."},"max_depth":{"type":"string","description":"Maximum directory depth to traverse, encoded as a string integer."},"limit":{"type":"string","description":"Maximum number of entries to return, encoded as a string integer."}}"#,
        required_json: r#"[]"#,
    },
    StaticToolSpec {
        name: "read_file",
        description: "Read a text file and return a numbered excerpt.",
        properties_json: r#"{"path":{"type":"string","description":"Path to the file."},"max_lines":{"type":"string","description":"Maximum number of lines to return, encoded as a string integer."}}"#,
        required_json: r#"["path","max_lines"]"#,
    },
    StaticToolSpec {
        name: "write_file",
        description: "Write UTF-8 content to a safe relative path under the workspace. Creates parent directories and requires write approval.",
        properties_json: r#"{"path":{"type":"string","description":"Relative workspace path to create or overwrite."},"content":{"type":"string","description":"Complete UTF-8 file content to write."},"cwd":{"type":"string","description":"Optional workspace directory. Defaults to current directory."}}"#,
        required_json: r#"["path","content"]"#,
    },
    StaticToolSpec {
        name: "edit_file",
        description: "Replace exact text in one UTF-8 file under the workspace. Use apply_patch for multi-hunk or multi-file edits. Requires write approval.",
        properties_json: r#"{"path":{"type":"string","description":"Relative workspace file path to edit."},"search":{"type":"string","description":"Exact text to find."},"replace":{"type":"string","description":"Replacement text. May be empty."},"cwd":{"type":"string","description":"Optional workspace directory. Defaults to current directory."}}"#,
        required_json: r#"["path","search","replace"]"#,
    },
    StaticToolSpec {
        name: "fim_edit",
        description: "DeepSeek-TUI-compatible Fill-in-the-Middle edit tool. Finds prefix_anchor and suffix_anchor in a workspace file, generates replacement middle content through DeepSeek /beta completions, and writes the result.",
        properties_json: r#"{"path":{"type":"string","description":"Relative workspace file path to edit."},"prefix_anchor":{"type":"string","description":"Text anchor marking the end of the preserved prefix."},"suffix_anchor":{"type":"string","description":"Text anchor marking the start of the preserved suffix."},"max_tokens":{"type":"string","description":"Maximum tokens to generate through the FIM endpoint, default 1024."},"generated_text":{"type":"string","description":"Optional offline/test override for generated middle text; omit during normal model-driven use."},"cwd":{"type":"string","description":"Optional workspace directory. Defaults to current directory."}}"#,
        required_json: r#"["path","prefix_anchor","suffix_anchor"]"#,
    },
    StaticToolSpec {
        name: "retrieve_tool_result",
        description: "Retrieve a spilled large tool result by id, filename, or spillover path using summary, head, tail, lines, or query modes.",
        properties_json: r#"{"ref":{"type":"string","description":"Tool output ref, tool_result:<id>, spillover filename, or spillover path."},"mode":{"type":"string","description":"Retrieval mode: summary, head, tail, lines, or query. Defaults to summary."},"query":{"type":"string","description":"Case-insensitive substring to search for when mode=query."},"lines":{"type":"string","description":"Line selector for mode=lines, for example 10 or 10-40."},"start_line":{"type":"string","description":"1-based start line for mode=lines."},"end_line":{"type":"string","description":"1-based end line for mode=lines."},"line_count":{"type":"string","description":"Number of lines for head/tail modes."},"max_bytes":{"type":"string","description":"Maximum excerpt bytes to return."},"max_matches":{"type":"string","description":"Maximum query matches or signal lines."},"context_lines":{"type":"string","description":"Extra lines around each query match."}}"#,
        required_json: r#"["ref"]"#,
    },
    StaticToolSpec {
        name: "search_text",
        description: "Search for plain text occurrences in repository files.",
        properties_json: r#"{"root":{"type":"string","description":"Root directory to search from."},"query":{"type":"string","description":"Plain text query to find."},"limit":{"type":"string","description":"Maximum number of matches to return, encoded as a string integer."}}"#,
        required_json: r#"["root","query","limit"]"#,
    },
    StaticToolSpec {
        name: "grep_files",
        description: "DeepSeek-TUI-compatible alias for searching literal text in repository files.",
        properties_json: r#"{"pattern":{"type":"string","description":"Literal text pattern to find."},"path":{"type":"string","description":"Root directory to search from, usually `.`."},"max_results":{"type":"string","description":"Maximum number of matches to return, encoded as a string integer."},"limit":{"type":"string","description":"Alternative maximum number of matches to return, encoded as a string integer."}}"#,
        required_json: r#"["pattern"]"#,
    },
    StaticToolSpec {
        name: "file_search",
        description: "Find repository files by filename or path using simple fuzzy matching.",
        properties_json: r#"{"query":{"type":"string","description":"Filename or path query to match."},"path":{"type":"string","description":"Root directory to search from, usually `.`."},"extensions":{"type":"string","description":"Optional comma-, semicolon-, or whitespace-separated extension filter, for example `rs,md`."},"limit":{"type":"string","description":"Maximum number of file matches to return, encoded as a string integer."},"max_results":{"type":"string","description":"Alternative maximum number of file matches to return, encoded as a string integer."}}"#,
        required_json: r#"["query"]"#,
    },
    StaticToolSpec {
        name: "web_run",
        description: "DeepSeek-TUI-compatible aggregate web tool. Supports search_query, image_query, stored-ref/direct-URL open, link click, stored-ref/direct-URL find, finance, and PDF-page screenshot extraction while reporting unsupported browser-only actions.",
        properties_json: r#"{"search_query":{"type":"array","description":"Search requests such as [{\"q\":\"latest Rust release\",\"max_results\":5}]. Results are stored as searchN and turnNsearchN refs.","items":{"type":"object"}},"image_query":{"type":"array","description":"Image search requests such as [{\"q\":\"architecture diagram\",\"max_results\":5,\"domains\":[\"example.com\"]}].","items":{"type":"object"}},"open":{"type":"array","description":"Open requests such as [{\"ref_id\":\"search0\",\"lineno\":1}] or [{\"ref_id\":\"https://example.com\",\"format\":\"markdown\"}]. Opened pages return line-windowed content, expose numbered links, and are cached as openN/turnNopenN; opened PDFs cache page text when local pdftotext is available.","items":{"type":"object"}},"click":{"type":"array","description":"Click a numbered link from a cached page, for example [{\"ref_id\":\"open0\",\"id\":1,\"lineno\":1}]. Clicked pages return line-windowed content and are cached as clickN/turnNclickN.","items":{"type":"object"}},"find":{"type":"array","description":"Find requests such as [{\"ref_id\":\"open0\",\"pattern\":\"needle\"}] or direct URL variants.","items":{"type":"object"}},"finance":{"type":"array","description":"Finance quote requests such as [{\"ticker\":\"AAPL\",\"type\":\"equity\"}].","items":{"type":"object"}},"screenshot":{"type":"array","description":"Return text for a cached PDF page, for example [{\"ref_id\":\"open0\",\"pageno\":0}]. Browser/DOM screenshots are not supported.","items":{"type":"object"}},"response_length":{"type":"string","description":"Controls open/click page-window size: short=40 lines, medium=80 lines, long=160 lines."}}"#,
        required_json: r#"[]"#,
    },
    StaticToolSpec {
        name: "web_search",
        description: "Search the web and return ranked results with URLs, snippets, and ref_ids. Use fetch_url for a known canonical URL.",
        properties_json: r#"{"query":{"type":"string","description":"Search query."},"q":{"type":"string","description":"Search query alias."},"search_query":{"type":"string","description":"JSON array compatibility form, for example [{\"q\":\"latest Rust release\",\"max_results\":5}]."},"max_results":{"type":"string","description":"Maximum number of results, default 5 and max 10."},"timeout_ms":{"type":"string","description":"Request timeout in milliseconds, default 15000 and max 60000."}}"#,
        required_json: r#"[]"#,
    },
    StaticToolSpec {
        name: "fetch_url",
        description: "Fetch a known HTTP/HTTPS URL directly and return decoded text or raw content.",
        properties_json: r#"{"url":{"type":"string","description":"Absolute HTTP/HTTPS URL to fetch."},"format":{"type":"string","description":"text, markdown, or raw. Defaults to markdown/text extraction."},"max_bytes":{"type":"string","description":"Maximum response bytes to return, default 1000000."},"timeout_ms":{"type":"string","description":"Request timeout in milliseconds, default 15000 and max 60000."}}"#,
        required_json: r#"["url"]"#,
    },
    StaticToolSpec {
        name: "finance",
        description: "Fetch a live market quote for a stock, ETF, index, or crypto ticker using a Yahoo Finance-compatible endpoint.",
        properties_json: r#"{"ticker":{"type":"string","description":"Ticker symbol to look up, for example AAPL, SPY, ^GSPC, or BTC."},"symbol":{"type":"string","description":"Alias for ticker."},"type":{"type":"string","description":"Optional asset type hint such as equity, fund, crypto, or index."},"market":{"type":"string","description":"Optional market hint retained for compatibility with finance-style tool calls."},"timeout_ms":{"type":"string","description":"Request timeout in milliseconds, default 10000 and max 60000."}}"#,
        required_json: r#"[]"#,
    },
    StaticToolSpec {
        name: "pandoc_convert",
        description: "DeepSeek-TUI-compatible document conversion wrapper around local pandoc. Converts source_path to a whitelisted target_format and returns text inline or writes output_path.",
        properties_json: r#"{"source_path":{"type":"string","description":"Relative workspace source document path."},"target_format":{"type":"string","description":"One of markdown, gfm, commonmark, html, rst, latex, docx, odt, epub, plain, asciidoc."},"output_path":{"type":"string","description":"Optional relative output path. Required for binary target formats docx, odt, and epub."},"cwd":{"type":"string","description":"Optional workspace directory. Defaults to current directory."}}"#,
        required_json: r#"["source_path","target_format"]"#,
    },
    StaticToolSpec {
        name: "image_ocr",
        description: "DeepSeek-TUI-compatible local OCR wrapper around tesseract. Extracts text from a workspace image and returns it inline.",
        properties_json: r#"{"path":{"type":"string","description":"Relative workspace image path, for example PNG, JPEG, or TIFF."},"cwd":{"type":"string","description":"Optional workspace directory. Defaults to current directory."}}"#,
        required_json: r#"["path"]"#,
    },
    StaticToolSpec {
        name: "image_analyze",
        description: "DeepSeek-TUI-compatible vision analysis tool. Sends a workspace image to the configured OpenAI-compatible vision chat/completions endpoint and returns analysis JSON.",
        properties_json: r#"{"image_path":{"type":"string","description":"Relative workspace image path. Supports PNG, JPEG, GIF, WebP, and BMP."},"prompt":{"type":"string","description":"Optional prompt to guide the image analysis."},"cwd":{"type":"string","description":"Optional workspace directory. Defaults to current directory."},"model":{"type":"string","description":"Optional per-call vision model override."},"base_url":{"type":"string","description":"Optional per-call OpenAI-compatible API base URL override."},"api_key_env":{"type":"string","description":"Optional environment variable name containing the vision API key."}}"#,
        required_json: r#"["image_path"]"#,
    },
    StaticToolSpec {
        name: "review",
        description: "Run a structured local code review over a safe relative file, git diff, or github_pr_context output and return issue/suggestion JSON, including marker checks, PR review/status signals, missing tests, public API changes, and dependency/configuration changes. Set semantic=true to run a read-only child-agent semantic review over the same evidence.",
        properties_json: r#"{"target":{"type":"string","description":"File path, literal diff/staged for git diff review, or github_pr_context when passing GitHub PR context."},"kind":{"type":"string","description":"Optional target type: file or diff."},"base":{"type":"string","description":"Optional git base ref when reviewing a diff."},"staged":{"type":"string","description":"Set true to review staged changes."},"cwd":{"type":"string","description":"Workspace or git working directory. Defaults to current directory."},"github_context":{"type":"string","description":"Output from github_pr_context, preferably with include_diff=true, for remote PR review without fetching inside review."},"pr_context":{"type":"string","description":"Alias for github_context."},"max_chars":{"type":"string","description":"Maximum source characters to review, default 200000 and max 1000000."},"semantic":{"type":"string","description":"Set true/1/yes/on to run an additional child-agent semantic review. Default false."},"steps":{"type":"string","description":"Optional semantic child-agent step budget. Default 6 and maximum follows subagent limits."},"agent":{"type":"string","description":"Optional configured agent name for semantic review."},"skill":{"type":"string","description":"Optional skill name for semantic review."}}"#,
        required_json: r#"["target"]"#,
    },
    StaticToolSpec {
        name: "pr_review_comment_plan",
        description: "Turn structured review JSON plus optional github_pr_context output into a read-only GitHub PR comment plan containing Markdown body text, evidence JSON, dry-run github_comment input, and dry-run inline review-comment input when line-level findings and PR head SHA are available.",
        properties_json: r#"{"review_output":{"type":"string","description":"JSON output from the review tool."},"review_json":{"type":"string","description":"Alias for review_output."},"review":{"type":"string","description":"Alias for review_output."},"github_context":{"type":"string","description":"Optional github_pr_context output used to infer PR number, repository, and head commit."},"pr_context":{"type":"string","description":"Alias for github_context."},"context":{"type":"string","description":"Alias for github_context."},"number":{"type":"string","description":"Optional PR number when not present in context."},"pr":{"type":"string","description":"Alias for number."},"repo":{"type":"string","description":"Optional owner/repo for suggested GitHub inputs."},"repository":{"type":"string","description":"Alias for repo."},"commit_id":{"type":"string","description":"Optional PR head commit SHA for inline review comments."},"head_sha":{"type":"string","description":"Alias for commit_id."},"sha":{"type":"string","description":"Alias for commit_id."},"max_issues":{"type":"string","description":"Maximum findings to render in the comment, default 8 and max 20."}}"#,
        required_json: r#"["review_output"]"#,
    },
    StaticToolSpec {
        name: "request_user_input",
        description: "Ask the user 1-3 short structured questions with 2-3 labeled options each, then wait for their answer before continuing.",
        properties_json: r#"{"questions":{"type":"array","description":"One to three short questions for the user.","items":{"type":"object","properties":{"header":{"type":"string","description":"Short UI header for this question."},"id":{"type":"string","description":"Stable answer identifier."},"question":{"type":"string","description":"Question to show the user."},"options":{"type":"array","description":"Two or three mutually exclusive options.","items":{"type":"object","properties":{"label":{"type":"string","description":"Short user-facing option label."},"description":{"type":"string","description":"One sentence explaining the option."}},"required":["label","description"]},"minItems":2,"maxItems":3}},"required":["header","id","question","options"]},"minItems":1,"maxItems":3}}"#,
        required_json: r#"["questions"]"#,
    },
    StaticToolSpec {
        name: "tool_search_tool_regex",
        description: "Search available tool definitions using a regex-style query and return matching tool references.",
        properties_json: r#"{"query":{"type":"string","description":"Regex-style pattern to search tool names, descriptions, and schemas."},"limit":{"type":"string","description":"Maximum tool references to return, default 5 and max 20."}}"#,
        required_json: r#"["query"]"#,
    },
    StaticToolSpec {
        name: "tool_search_tool_bm25",
        description: "Search available tool definitions using natural-language matching and return ranked tool references.",
        properties_json: r#"{"query":{"type":"string","description":"Natural-language query for tool discovery."},"limit":{"type":"string","description":"Maximum tool references to return, default 5 and max 20."}}"#,
        required_json: r#"["query"]"#,
    },
    StaticToolSpec {
        name: "apply_patch",
        description: "Apply a text replacement or a unified diff patch to files.",
        properties_json: r#"{"cwd":{"type":"string","description":"Working directory used when applying a unified diff patch."},"path":{"type":"string","description":"Target file path for direct replacement mode."},"find":{"type":"string","description":"Exact text to find for direct replacement mode."},"replace":{"type":"string","description":"Replacement text for direct replacement mode."},"replace_all":{"type":"string","description":"`true` to replace all occurrences in direct replacement mode, otherwise `false`."},"patch":{"type":"string","description":"Unified diff patch content. When provided, patch mode is used and path/find/replace are optional."}}"#,
        required_json: r#"[]"#,
    },
    StaticToolSpec {
        name: "run_shell",
        description: "Run a safe allowlisted shell command in the repository.",
        properties_json: r#"{"cwd":{"type":"string","description":"Working directory for the command."},"command":{"type":"string","description":"Safe shell command to execute."}}"#,
        required_json: r#"["cwd","command"]"#,
    },
    StaticToolSpec {
        name: "exec_shell",
        description: "DeepSeek-TUI-compatible shell execution tool. Use background=true for long-running commands, then poll with exec_shell_wait. On Unix, tty=true uses the script PTY backend for background jobs.",
        properties_json: r#"{"command":{"type":"string","description":"Safe shell command to execute."},"timeout_ms":{"type":"string","description":"Compatibility timeout in milliseconds for foreground commands."},"background":{"type":"string","description":"Set true to run in the background and return task_id."},"tty":{"type":"string","description":"Set true with background=true to run through the Unix script PTY backend when available."},"tty_rows":{"type":"string","description":"Optional initial PTY row count; requires tty=true and tty_cols."},"tty_cols":{"type":"string","description":"Optional initial PTY column count; requires tty=true and tty_rows."},"stdin":{"type":"string","description":"Optional stdin data sent to a background command at start."},"input":{"type":"string","description":"Alias for stdin."},"data":{"type":"string","description":"Alias for stdin."},"cwd":{"type":"string","description":"Working directory for the command."}}"#,
        required_json: r#"["command"]"#,
    },
    StaticToolSpec {
        name: "task_shell_start",
        description: "DeepSeek-TUI-compatible background shell starter for long-running commands. Returns a task_id to poll with task_shell_wait.",
        properties_json: r#"{"command":{"type":"string","description":"Safe shell command to start in the background."},"cwd":{"type":"string","description":"Optional working directory."},"timeout_ms":{"type":"string","description":"Compatibility timeout in milliseconds."},"stdin":{"type":"string","description":"Optional stdin data sent at start."},"tty":{"type":"string","description":"Set true to run through the Unix script PTY backend when available."},"tty_rows":{"type":"string","description":"Optional initial PTY row count; requires tty=true and tty_cols."},"tty_cols":{"type":"string","description":"Optional initial PTY column count; requires tty=true and tty_rows."}}"#,
        required_json: r#"["command"]"#,
    },
    StaticToolSpec {
        name: "task_shell_wait",
        description: "DeepSeek-TUI-compatible poll/wait helper for background shell tasks started by task_shell_start or exec_shell background=true.",
        properties_json: r#"{"task_id":{"type":"string","description":"Background shell task id returned by task_shell_start or exec_shell."},"id":{"type":"string","description":"Alias for task_id."},"cwd":{"type":"string","description":"Working directory used to find detached durable shell records."},"wait":{"type":"string","description":"Set false to poll once without waiting."},"timeout_ms":{"type":"string","description":"Maximum wait milliseconds, default 5000."},"gate":{"type":"string","description":"Optional gate label for compatibility metadata."},"command":{"type":"string","description":"Optional original command for compatibility metadata."}}"#,
        required_json: r#"["task_id"]"#,
    },
    StaticToolSpec {
        name: "exec_shell_wait",
        description: "Wait for or poll a background exec_shell task and return incremental output.",
        properties_json: r#"{"task_id":{"type":"string","description":"Task id returned by exec_shell background=true."},"id":{"type":"string","description":"Alias for task_id."},"cwd":{"type":"string","description":"Working directory used to find detached durable shell records."},"timeout_ms":{"type":"string","description":"Maximum wait milliseconds, default 5000."},"wait":{"type":"string","description":"Set false to poll once without waiting."}}"#,
        required_json: r#"["task_id"]"#,
    },
    StaticToolSpec {
        name: "exec_shell_replay",
        description: "Replay durable stdout/stderr log slices for a background exec_shell task by byte offset. Use next_offset to continue replaying without rereading prior output.",
        properties_json: r#"{"task_id":{"type":"string","description":"Task id returned by exec_shell background=true."},"id":{"type":"string","description":"Alias for task_id."},"cwd":{"type":"string","description":"Working directory used to find detached durable shell records."},"stream":{"type":"string","description":"stdout, stderr, or all. Defaults to stdout."},"offset":{"type":"string","description":"Byte offset to start from. Defaults to 0."},"limit_bytes":{"type":"string","description":"Maximum bytes to return, default 20000 and capped at 100000."},"tail":{"type":"string","description":"Set true to replay the last limit_bytes bytes."}}"#,
        required_json: r#"["task_id"]"#,
    },
    StaticToolSpec {
        name: "exec_wait",
        description: "Alias for exec_shell_wait.",
        properties_json: r#"{"task_id":{"type":"string","description":"Task id returned by exec_shell background=true."},"id":{"type":"string","description":"Alias for task_id."},"cwd":{"type":"string","description":"Working directory used to find detached durable shell records."},"timeout_ms":{"type":"string","description":"Maximum wait milliseconds, default 5000."},"wait":{"type":"string","description":"Set false to poll once without waiting."}}"#,
        required_json: r#"["task_id"]"#,
    },
    StaticToolSpec {
        name: "exec_shell_interact",
        description: "Send stdin to an attached background exec_shell task, or to a detached Unix FIFO-backed task when cwd is supplied.",
        properties_json: r#"{"task_id":{"type":"string","description":"Task id returned by exec_shell background=true."},"id":{"type":"string","description":"Alias for task_id."},"cwd":{"type":"string","description":"Working directory used to find detached durable shell records."},"input":{"type":"string","description":"Input to send to stdin."},"stdin":{"type":"string","description":"Alias for input."},"data":{"type":"string","description":"Alias for input."},"timeout_ms":{"type":"string","description":"Wait milliseconds after sending input, default 1000."},"close_stdin":{"type":"string","description":"Set true to close stdin after sending input."}}"#,
        required_json: r#"["task_id"]"#,
    },
    StaticToolSpec {
        name: "exec_interact",
        description: "Alias for exec_shell_interact.",
        properties_json: r#"{"task_id":{"type":"string","description":"Task id returned by exec_shell background=true."},"id":{"type":"string","description":"Alias for task_id."},"cwd":{"type":"string","description":"Working directory used to find detached durable shell records."},"input":{"type":"string","description":"Input to send to stdin."},"stdin":{"type":"string","description":"Alias for input."},"data":{"type":"string","description":"Alias for input."},"timeout_ms":{"type":"string","description":"Wait milliseconds after sending input, default 1000."},"close_stdin":{"type":"string","description":"Set true to close stdin after sending input."}}"#,
        required_json: r#"["task_id"]"#,
    },
    StaticToolSpec {
        name: "exec_shell_cancel",
        description: "Cancel a running background exec_shell task by task_id, or all running tasks with all=true.",
        properties_json: r#"{"task_id":{"type":"string","description":"Task id returned by exec_shell background=true."},"id":{"type":"string","description":"Alias for task_id."},"cwd":{"type":"string","description":"Working directory used to find detached durable shell records."},"all":{"type":"string","description":"Set true to cancel all running background shell tasks."}}"#,
        required_json: r#"[]"#,
    },
    StaticToolSpec {
        name: "run_tests",
        description: "Run a supported test command in the repository. Infers cargo/go/node/python test commands from project files when command is omitted.",
        properties_json: r#"{"cwd":{"type":"string","description":"Working directory for the test command. Defaults to `.`."},"command":{"type":"string","description":"Optional supported test command such as `cargo test`, `go test ./...`, `pytest`, `npm test`, or `pnpm test`."},"args":{"type":"string","description":"Optional safe extra arguments appended to the test command."},"all_features":{"type":"string","description":"Set true to add `--all-features` for inferred `cargo test`."}}"#,
        required_json: r#"[]"#,
    },
    StaticToolSpec {
        name: "revert_turn",
        description: "Restore workspace files to a rollback snapshot from a recent agent turn. Use only when the user explicitly asks to undo, revert, or roll back edits.",
        properties_json: r#"{"turn_offset":{"type":"string","description":"1-based recent runtime turn snapshot offset. Defaults to 1."},"offset":{"type":"string","description":"Alias for turn_offset."},"turn_id":{"type":"string","description":"Runtime assistant turn id to restore from."},"thread_id":{"type":"string","description":"Optional runtime thread id used with turn_offset."},"snapshot_id":{"type":"string","description":"Rollback snapshot id to restore."},"checkpoint_id":{"type":"string","description":"Alias for snapshot_id."},"id":{"type":"string","description":"Alias for snapshot_id or turn id."},"dry_run":{"type":"string","description":"Set true to preview without mutating files."},"apply":{"type":"string","description":"Set false to preview without mutating files; defaults to true."}}"#,
        required_json: r#"[]"#,
    },
    StaticToolSpec {
        name: "git_status",
        description: "Show concise git status for the workspace, optionally scoped to a path.",
        properties_json: r#"{"cwd":{"type":"string","description":"Git working directory, default `.`."},"path":{"type":"string","description":"Optional repository path to scope status to."}}"#,
        required_json: r#"[]"#,
    },
    StaticToolSpec {
        name: "git_diff",
        description: "Show the current git diff for the workspace, optionally scoped to a path or staged changes.",
        properties_json: r#"{"cwd":{"type":"string","description":"Git working directory, default `.`."},"path":{"type":"string","description":"Optional repository path to scope diff to."},"cached":{"type":"string","description":"Set true to show staged changes with --cached."},"unified":{"type":"string","description":"Context lines to include, 0-50. Defaults to 3."},"max_chars":{"type":"string","description":"Maximum output characters to return."}}"#,
        required_json: r#"[]"#,
    },
    StaticToolSpec {
        name: "project_map",
        description: "Render a high-level project tree, summary counts, and key files.",
        properties_json: r#"{"path":{"type":"string","description":"Project root to map, usually `.`."},"max_depth":{"type":"string","description":"Maximum tree depth to traverse, encoded as a string integer."},"limit":{"type":"string","description":"Maximum tree entries to return, encoded as a string integer."}}"#,
        required_json: r#"[]"#,
    },
    StaticToolSpec {
        name: "diagnostics",
        description: "Run local language diagnostics for the workspace or a set of edited files. Uses stdio LSP publishDiagnostics for opened files when available, then falls back to compiler/type-check commands.",
        properties_json: r#"{"cwd":{"type":"string","description":"Working directory to run diagnostics in. Defaults to `.`."},"paths":{"type":"string","description":"Optional comma-, semicolon-, or newline-separated list of edited paths to scope diagnostics."}}"#,
        required_json: r#"[]"#,
    },
    StaticToolSpec {
        name: "validate_data",
        description: "Validate JSON or TOML data from inline content or a file path and report parser errors without modifying files.",
        properties_json: r#"{"path":{"type":"string","description":"Optional file path to validate. Mutually exclusive with content."},"content":{"type":"string","description":"Optional inline content to validate. Mutually exclusive with path."},"format":{"type":"string","description":"Validation format: auto, json, or toml. Defaults to auto."}}"#,
        required_json: r#"[]"#,
    },
    StaticToolSpec {
        name: "recall_archive",
        description: "DeepSeek-TUI-compatible archive recall tool. Searches durable runtime threads, turns, items, and compaction summaries for older context that may be missing from the current prompt.",
        properties_json: r#"{"query":{"type":"string","description":"Search query, tokenized and scored against durable runtime transcript/archive content."},"thread_id":{"type":"string","description":"Optional runtime thread id to search. Omit to search recent runtime threads."},"cycle":{"type":"string","description":"Accepted for DeepSeek-TUI compatibility; local runtime recall searches durable thread content rather than numbered cycle archive files."},"max_results":{"type":"string","description":"Maximum hits to return, default 3 and max 10."},"limit":{"type":"string","description":"Alias for max_results."}}"#,
        required_json: r#"["query"]"#,
    },
    StaticToolSpec {
        name: "git_log",
        description: "Show recent git commit history, optionally scoped to a ref or path.",
        properties_json: r#"{"cwd":{"type":"string","description":"Working directory for the git command. Defaults to `.`."},"limit":{"type":"string","description":"Maximum number of commits to return, encoded as a string integer."},"ref":{"type":"string","description":"Optional git revision or branch to inspect."},"path":{"type":"string","description":"Optional repository path to limit the history to."},"max_chars":{"type":"string","description":"Maximum output characters to return, encoded as a string integer."}}"#,
        required_json: r#"[]"#,
    },
    StaticToolSpec {
        name: "git_show",
        description: "Show a git commit, tag, or revision with stat and patch.",
        properties_json: r#"{"cwd":{"type":"string","description":"Working directory for the git command. Defaults to `.`."},"ref":{"type":"string","description":"Git revision, commit SHA, branch, or tag to show. Defaults to HEAD."},"path":{"type":"string","description":"Optional repository path to limit the shown patch to."},"max_chars":{"type":"string","description":"Maximum output characters to return, encoded as a string integer."}}"#,
        required_json: r#"[]"#,
    },
    StaticToolSpec {
        name: "git_blame",
        description: "Show git blame for a file and optional line range.",
        properties_json: r#"{"cwd":{"type":"string","description":"Working directory for the git command. Defaults to `.`."},"path":{"type":"string","description":"Repository file path to blame."},"line_start":{"type":"string","description":"Optional first line to blame, encoded as a string integer."},"line_end":{"type":"string","description":"Optional last line to blame, encoded as a string integer."},"limit":{"type":"string","description":"Number of lines to blame when line_end is omitted, encoded as a string integer."},"ref":{"type":"string","description":"Optional git revision to blame from. Defaults to HEAD."},"max_chars":{"type":"string","description":"Maximum output characters to return, encoded as a string integer."}}"#,
        required_json: r#"["path"]"#,
    },
    StaticToolSpec {
        name: "load_skill",
        description: "Load a configured DeepSeekCode TOML skill by name and return its self-contained context, references, policy, and suggested steps.",
        properties_json: r#"{"name":{"type":"string","description":"Skill name from the configured skills registry."}}"#,
        required_json: r#"["name"]"#,
    },
    StaticToolSpec {
        name: "note",
        description: "Append a persistent maintainer or agent note to the configured notes file. Use for durable decisions, blockers, or architectural context, not transient scratch.",
        properties_json: r#"{"content":{"type":"string","description":"Note content to append."},"note":{"type":"string","description":"Alias for content."}}"#,
        required_json: r#"["content"]"#,
    },
    StaticToolSpec {
        name: "remember",
        description: "Append a durable single-sentence memory note to the configured user memory file. Use only for stable user preferences, conventions, or facts; do not store secrets or transient task state.",
        properties_json: r#"{"note":{"type":"string","description":"Single-sentence durable note to remember."},"content":{"type":"string","description":"Alias for note."}}"#,
        required_json: r#"["note"]"#,
    },
    StaticToolSpec {
        name: "notify",
        description: "Fire a single terminal attention signal for long-running task completion or when the user needs to return. Use sparingly.",
        properties_json: r#"{"title":{"type":"string","description":"Short notification title, truncated to 80 characters."},"body":{"type":"string","description":"Optional notification body, truncated to 200 characters."}}"#,
        required_json: r#"["title"]"#,
    },
    StaticToolSpec {
        name: "github_issue_context",
        description: "Read GitHub issue context using the gh CLI. Read-only.",
        properties_json: r#"{"number":{"type":"string","description":"Issue number or reference."},"issue":{"type":"string","description":"Alias for number."},"ref":{"type":"string","description":"Alias for number."},"repo":{"type":"string","description":"Optional owner/repo for gh -R."},"repository":{"type":"string","description":"Alias for repo."},"include_comments":{"type":"string","description":"Set false to omit comments. Defaults to true."},"max_chars":{"type":"string","description":"Maximum JSON characters to include."}}"#,
        required_json: r#"["number"]"#,
    },
    StaticToolSpec {
        name: "github_pr_context",
        description: "Read GitHub pull request context using the gh CLI, optionally including a bounded patch diff. Read-only.",
        properties_json: r#"{"number":{"type":"string","description":"PR number or reference."},"pr":{"type":"string","description":"Alias for number."},"ref":{"type":"string","description":"Alias for number."},"repo":{"type":"string","description":"Optional owner/repo for gh -R."},"repository":{"type":"string","description":"Alias for repo."},"include_diff":{"type":"string","description":"Set true to include gh pr diff --patch output."},"max_chars":{"type":"string","description":"Maximum JSON characters to include."},"diff_max_chars":{"type":"string","description":"Maximum diff characters to include."}}"#,
        required_json: r#"["number"]"#,
    },
    StaticToolSpec {
        name: "github_comment",
        description: "Post an evidence-backed GitHub issue or PR comment using the gh CLI. Requires write approval.",
        properties_json: r#"{"target":{"type":"string","description":"Comment target: issue or pr."},"number":{"type":"string","description":"Issue or PR number."},"body":{"type":"string","description":"Comment body to post."},"evidence":{"type":"string","description":"JSON object with supporting evidence for the comment."},"repo":{"type":"string","description":"Optional owner/repo for gh -R."},"repository":{"type":"string","description":"Alias for repo."},"dry_run":{"type":"string","description":"Set true to validate without posting."}}"#,
        required_json: r#"["target","number","body","evidence"]"#,
    },
    StaticToolSpec {
        name: "github_pr_review_comment",
        description: "Post evidence-backed inline GitHub PR review comments on changed file lines using gh api. Requires write approval.",
        properties_json: r#"{"number":{"type":"string","description":"PR number."},"pr":{"type":"string","description":"Alias for number."},"comments":{"type":"string","description":"JSON array of inline comments with path, line, body, optional side/start_line/start_side/commit_id."},"path":{"type":"string","description":"Single inline comment file path when comments is omitted."},"line":{"type":"string","description":"Single inline comment line when comments is omitted."},"body":{"type":"string","description":"Single inline comment body when comments is omitted."},"commit_id":{"type":"string","description":"PR head commit SHA for the review comment."},"head_sha":{"type":"string","description":"Alias for commit_id."},"sha":{"type":"string","description":"Alias for commit_id."},"side":{"type":"string","description":"Diff side, RIGHT by default or LEFT."},"evidence":{"type":"string","description":"JSON object with supporting evidence for the inline comments."},"repo":{"type":"string","description":"Optional owner/repo for gh api endpoint."},"repository":{"type":"string","description":"Alias for repo."},"dry_run":{"type":"string","description":"Set true to validate without posting."}}"#,
        required_json: r#"["number","evidence"]"#,
    },
    StaticToolSpec {
        name: "github_close_issue",
        description: "Close a GitHub issue as completed using the gh CLI after structured acceptance evidence. Requires write approval.",
        properties_json: r#"{"number":{"type":"string","description":"Issue number."},"acceptance_criteria":{"type":"string","description":"JSON array of acceptance criteria satisfied."},"evidence":{"type":"string","description":"JSON object with files_changed, tests_run, and final_status."},"comment":{"type":"string","description":"Optional closing comment to post before closing."},"allow_dirty":{"type":"string","description":"Set true to allow closing while the local worktree is dirty."},"cwd":{"type":"string","description":"Workspace directory for git status dirty check."},"repo":{"type":"string","description":"Optional owner/repo for gh -R."},"repository":{"type":"string","description":"Alias for repo."},"dry_run":{"type":"string","description":"Set true to validate without closing."}}"#,
        required_json: r#"["number","acceptance_criteria","evidence"]"#,
    },
    StaticToolSpec {
        name: "task_create",
        description: "Create/enqueue a durable runtime task. Requires write approval because it mutates runtime work state.",
        properties_json: r#"{"prompt":{"type":"string","description":"Work prompt or summary for the durable task."},"summary":{"type":"string","description":"Alias for prompt."},"session_id":{"type":"string","description":"Optional runtime session id."},"thread_id":{"type":"string","description":"Optional runtime thread id."},"parent_task_id":{"type":"string","description":"Optional parent runtime task id."},"kind":{"type":"string","description":"Task kind. Defaults to agent."},"status":{"type":"string","description":"Initial task status. Defaults to pending."}}"#,
        required_json: r#"["prompt"]"#,
    },
    StaticToolSpec {
        name: "task_list",
        description: "List recent durable runtime tasks with optional session/thread filters.",
        properties_json: r#"{"session_id":{"type":"string","description":"Optional runtime session id filter."},"thread_id":{"type":"string","description":"Optional runtime thread id filter."},"limit":{"type":"string","description":"Maximum tasks to return, default 20 and max 100."}}"#,
        required_json: r#"[]"#,
    },
    StaticToolSpec {
        name: "task_read",
        description: "Read one durable runtime task.",
        properties_json: r#"{"task_id":{"type":"string","description":"Runtime task id."},"id":{"type":"string","description":"Alias for task_id."}}"#,
        required_json: r#"["task_id"]"#,
    },
    StaticToolSpec {
        name: "task_cancel",
        description: "Cancel a pending or running durable runtime task. Requires write approval because it mutates runtime work state.",
        properties_json: r#"{"task_id":{"type":"string","description":"Runtime task id."},"id":{"type":"string","description":"Alias for task_id."},"reason":{"type":"string","description":"Optional cancellation reason."}}"#,
        required_json: r#"["task_id"]"#,
    },
    StaticToolSpec {
        name: "task_gate_run",
        description: "Run a verification gate command through the existing safe shell path. Requires shell approval.",
        properties_json: r#"{"gate":{"type":"string","description":"Gate category: fmt, check, clippy, test, or custom."},"command":{"type":"string","description":"Safe shell command to run."},"cwd":{"type":"string","description":"Optional working directory."},"timeout_ms":{"type":"string","description":"Compatibility timeout field; execution still uses the existing cancellable shell path."}}"#,
        required_json: r#"["gate","command"]"#,
    },
    StaticToolSpec {
        name: "agent_spawn",
        description: "DeepSeek-TUI-compatible durable sub-agent spawn tool. Creates a runtime thread plus pending sub-agent task and returns agent_id immediately; run the runtime daemon or TUI runner to execute it.",
        properties_json: r#"{"prompt":{"type":"string","description":"Task description for the sub-agent."},"message":{"type":"string","description":"Alias for prompt."},"objective":{"type":"string","description":"Alias for prompt."},"task":{"type":"string","description":"Alias for prompt."},"type":{"type":"string","description":"Compatibility sub-agent type hint."},"agent_type":{"type":"string","description":"Alias for type."},"role":{"type":"string","description":"Compatibility role hint."},"model":{"type":"string","description":"Optional model for the new runtime thread."},"cwd":{"type":"string","description":"Workspace directory for the new runtime thread."},"workspace":{"type":"string","description":"Alias for cwd."},"thread_id":{"type":"string","description":"Optional existing runtime thread to attach the sub-agent task to."},"parent_task_id":{"type":"string","description":"Optional parent runtime task id."},"title":{"type":"string","description":"Optional title for a newly-created runtime thread."},"fork_context":{"type":"boolean","description":"Accepted for DeepSeek-TUI compatibility; local runtime-backed spawn records a fresh durable task."}}"#,
        required_json: r#"["prompt"]"#,
    },
    StaticToolSpec {
        name: "agent_result",
        description: "Read the latest status/result snapshot for a runtime-backed sub-agent.",
        properties_json: r#"{"agent_id":{"type":"string","description":"Agent id returned by agent_spawn."},"id":{"type":"string","description":"Alias for agent_id."},"block":{"type":"boolean","description":"Accepted for compatibility; local tool returns current durable status immediately."},"timeout_ms":{"type":"string","description":"Accepted for compatibility."}}"#,
        required_json: r#"["agent_id"]"#,
    },
    StaticToolSpec {
        name: "agent_list",
        description: "List runtime-backed sub-agents created through agent_spawn or send_input.",
        properties_json: r#"{"limit":{"type":"string","description":"Maximum agents to return, default 20 and max 100."},"include_archived":{"type":"boolean","description":"Accepted for compatibility; local runtime lists recent sub-agent tasks."}}"#,
        required_json: r#"[]"#,
    },
    StaticToolSpec {
        name: "agent_cancel",
        description: "Cancel a pending or running runtime-backed sub-agent. Requires write approval.",
        properties_json: r#"{"agent_id":{"type":"string","description":"Agent id returned by agent_spawn."},"id":{"type":"string","description":"Alias for agent_id."}}"#,
        required_json: r#"["agent_id"]"#,
    },
    StaticToolSpec {
        name: "close_agent",
        description: "DeepSeek-TUI-compatible alias for closing/cancelling a runtime-backed sub-agent. Requires write approval.",
        properties_json: r#"{"agent_id":{"type":"string","description":"Agent id returned by agent_spawn."},"id":{"type":"string","description":"Alias for agent_id."}}"#,
        required_json: r#"["agent_id"]"#,
    },
    StaticToolSpec {
        name: "resume_agent",
        description: "Resume a paused sub-agent or enqueue a new child sub-agent task from a previous sub-agent assignment. Requires write approval.",
        properties_json: r#"{"agent_id":{"type":"string","description":"Agent id returned by agent_spawn."},"id":{"type":"string","description":"Alias for agent_id."},"prompt":{"type":"string","description":"Optional replacement prompt for the resumed task."},"message":{"type":"string","description":"Alias for prompt."}}"#,
        required_json: r#"["agent_id"]"#,
    },
    StaticToolSpec {
        name: "send_input",
        description: "Send follow-up input to a runtime-backed sub-agent by appending a user message to its thread and queuing a child sub-agent task. Requires write approval.",
        properties_json: r#"{"agent_id":{"type":"string","description":"Agent id returned by agent_spawn."},"id":{"type":"string","description":"Alias for agent_id."},"message":{"type":"string","description":"Input message to send."},"input":{"type":"string","description":"Alias for message."},"prompt":{"type":"string","description":"Alias for message."}}"#,
        required_json: r#"["agent_id","message"]"#,
    },
    StaticToolSpec {
        name: "pr_attempt_record",
        description: "Capture the current git working-tree diff as a durable PR attempt with patch artifact, changed files, and verification notes.",
        properties_json: r#"{"summary":{"type":"string","description":"Short summary of this PR attempt."},"task_id":{"type":"string","description":"Optional durable runtime task id to attach the attempt to."},"attempt_group_id":{"type":"string","description":"Optional attempt group id for comparing multiple attempts."},"attempt_index":{"type":"string","description":"1-based attempt index."},"attempt_count":{"type":"string","description":"Total attempts in the group."},"verification":{"type":"string","description":"Verification notes as a JSON string array, newline list, or semicolon-separated list."},"cwd":{"type":"string","description":"Git working directory. Defaults to current directory."}}"#,
        required_json: r#"["summary"]"#,
    },
    StaticToolSpec {
        name: "pr_attempt_list",
        description: "List recent recorded PR attempts, optionally filtered by durable runtime task id.",
        properties_json: r#"{"task_id":{"type":"string","description":"Optional durable runtime task id filter."},"limit":{"type":"string","description":"Maximum attempts to return, default 20 and max 100."}}"#,
        required_json: r#"[]"#,
    },
    StaticToolSpec {
        name: "pr_attempt_read",
        description: "Read one recorded PR attempt and its patch artifact reference.",
        properties_json: r#"{"attempt_id":{"type":"string","description":"Recorded PR attempt id."},"id":{"type":"string","description":"Alias for attempt_id."},"task_id":{"type":"string","description":"Optional task id assertion."}}"#,
        required_json: r#"["attempt_id"]"#,
    },
    StaticToolSpec {
        name: "pr_attempt_preflight",
        description: "Run git apply --check for a recorded PR attempt patch without mutating the worktree.",
        properties_json: r#"{"attempt_id":{"type":"string","description":"Recorded PR attempt id."},"id":{"type":"string","description":"Alias for attempt_id."},"task_id":{"type":"string","description":"Optional task id assertion."}}"#,
        required_json: r#"["attempt_id"]"#,
    },
    StaticToolSpec {
        name: "automation_create",
        description: "Create a durable scheduled automation. DeepSeek-TUI-compatible creation requires name, prompt, and rrule, then stores it in the local runtime automation store.",
        properties_json: r#"{"name":{"type":"string","description":"Automation name."},"prompt":{"type":"string","description":"Prompt used when the automation runs."},"rrule":{"type":"string","description":"DeepSeek-TUI-compatible recurrence rule, such as FREQ=HOURLY;INTERVAL=N or FREQ=WEEKLY;BYDAY=MO;BYHOUR=9;BYMINUTE=30."},"schedule":{"type":"string","description":"Alias for rrule accepted by the local runtime."},"cwds":{"type":"array","items":{"type":"string"},"description":"DeepSeek-TUI-compatible working directories metadata; the local runtime currently stores schedule, prompt, session, and thread metadata."},"paused":{"type":"boolean","description":"Create the automation paused instead of active."},"status":{"type":"string","enum":["active","paused"],"description":"Explicit local runtime automation status."},"session_id":{"type":"string","description":"Optional runtime session id to attach."},"thread_id":{"type":"string","description":"Optional runtime thread id to attach."},"last_run_at":{"type":"string","description":"Optional existing last-run timestamp label."},"next_run_at":{"type":"string","description":"Optional next-run timestamp label."}}"#,
        required_json: r#"["name","prompt","rrule"]"#,
    },
    StaticToolSpec {
        name: "automation_list",
        description: "List durable runtime automations with optional session/thread filters.",
        properties_json: r#"{"session_id":{"type":"string","description":"Optional runtime session id filter."},"thread_id":{"type":"string","description":"Optional runtime thread id filter."},"limit":{"type":"string","description":"Maximum automations to return, default 50 and max 100."}}"#,
        required_json: r#"[]"#,
    },
    StaticToolSpec {
        name: "automation_read",
        description: "Read one durable runtime automation.",
        properties_json: r#"{"automation_id":{"type":"string","description":"Runtime automation id."},"id":{"type":"string","description":"Alias for automation_id."}}"#,
        required_json: r#"["automation_id"]"#,
    },
    StaticToolSpec {
        name: "automation_update",
        description: "Update a durable automation. DeepSeek-TUI-compatible fields include name, prompt, rrule, cwds, and status; local schedule is accepted as an rrule alias.",
        properties_json: r#"{"automation_id":{"type":"string","description":"Runtime automation id."},"id":{"type":"string","description":"Alias for automation_id."},"name":{"type":"string","description":"Optional replacement automation name."},"prompt":{"type":"string","description":"Optional replacement prompt used when the automation runs."},"rrule":{"type":"string","description":"Optional replacement DeepSeek-TUI-compatible recurrence rule."},"schedule":{"type":"string","description":"Alias for rrule accepted by the local runtime."},"cwds":{"type":"array","items":{"type":"string"},"description":"DeepSeek-TUI-compatible working directories metadata; currently accepted for schema compatibility but not persisted by the local runtime store."},"status":{"type":"string","enum":["active","paused"],"description":"Optional replacement automation status."},"paused":{"type":"boolean","description":"Compatibility alias that maps true to paused and false to active."},"next_run_at":{"type":"string","description":"Optional replacement next-run timestamp label."}}"#,
        required_json: r#"["automation_id"]"#,
    },
    StaticToolSpec {
        name: "automation_pause",
        description: "Pause a durable automation. Requires write approval.",
        properties_json: r#"{"automation_id":{"type":"string","description":"Runtime automation id."},"id":{"type":"string","description":"Alias for automation_id."}}"#,
        required_json: r#"["automation_id"]"#,
    },
    StaticToolSpec {
        name: "automation_resume",
        description: "Resume a paused durable automation. Requires write approval.",
        properties_json: r#"{"automation_id":{"type":"string","description":"Runtime automation id."},"id":{"type":"string","description":"Alias for automation_id."}}"#,
        required_json: r#"["automation_id"]"#,
    },
    StaticToolSpec {
        name: "automation_delete",
        description: "Delete a durable automation from the local runtime automation store. Requires write approval.",
        properties_json: r#"{"automation_id":{"type":"string","description":"Runtime automation id."},"id":{"type":"string","description":"Alias for automation_id."}}"#,
        required_json: r#"["automation_id"]"#,
    },
    StaticToolSpec {
        name: "automation_run",
        description: "Run an automation now by enqueuing a normal durable automation task. Requires write approval.",
        properties_json: r#"{"automation_id":{"type":"string","description":"Runtime automation id."},"id":{"type":"string","description":"Alias for automation_id."},"prompt":{"type":"string","description":"Optional prompt override for this run."},"prompt_override":{"type":"string","description":"Alias for prompt."}}"#,
        required_json: r#"["automation_id"]"#,
    },
    StaticToolSpec {
        name: "todo_write",
        description: "Replace the entire todo list with a new set of items. Use proactively for tasks with 3+ steps; mark exactly one item as in_progress at a time.",
        properties_json: r#"{"items":{"type":"string","description":"JSON array of objects with fields {content: string, activeForm: string, status: \"pending\"|\"in_progress\"|\"completed\"}. content is imperative form (e.g. \"Run tests\"); activeForm is present continuous (e.g. \"Running tests\")."}}"#,
        required_json: r#"["items"]"#,
    },
    StaticToolSpec {
        name: "update_plan",
        description: "DeepSeek-TUI-compatible structured plan update tool. Use this to track multi-step implementation progress; keep exactly one step in_progress.",
        properties_json: r#"{"explanation":{"type":"string","description":"Optional high-level explanation of the plan or approach."},"plan":{"type":"array","description":"List of plan steps.","items":{"type":"object","properties":{"step":{"type":"string","description":"Description of the step."},"status":{"type":"string","enum":["pending","in_progress","completed"],"description":"Step status."}},"required":["step","status"]}}}"#,
        required_json: r#"["plan"]"#,
    },
    StaticToolSpec {
        name: "checklist_write",
        description: "DeepSeek-TUI-compatible alias for todo_write; replace the whole checklist/todo list.",
        properties_json: r#"{"items":{"type":"string","description":"JSON array of objects with fields {content: string, activeForm: string, status: \"pending\"|\"in_progress\"|\"completed\"}."}}"#,
        required_json: r#"["items"]"#,
    },
    StaticToolSpec {
        name: "todo_add",
        description: "Add one todo/checklist item to the current plan.",
        properties_json: r#"{"content":{"type":"string","description":"Todo item text."},"activeForm":{"type":"string","description":"Optional active/progressive form shown when item is in_progress."},"status":{"type":"string","description":"Optional status: pending, in_progress, or completed. Defaults to pending."}}"#,
        required_json: r#"["content"]"#,
    },
    StaticToolSpec {
        name: "checklist_add",
        description: "DeepSeek-TUI-compatible alias for todo_add; add one checklist item.",
        properties_json: r#"{"content":{"type":"string","description":"Checklist item text."},"activeForm":{"type":"string","description":"Optional active/progressive form shown when item is in_progress."},"status":{"type":"string","description":"Optional status: pending, in_progress, or completed. Defaults to pending."}}"#,
        required_json: r#"["content"]"#,
    },
    StaticToolSpec {
        name: "todo_update",
        description: "Update one todo/checklist item by 1-based id.",
        properties_json: r#"{"id":{"type":"string","description":"1-based todo item id."},"status":{"type":"string","description":"New status: pending, in_progress, or completed."}}"#,
        required_json: r#"["id","status"]"#,
    },
    StaticToolSpec {
        name: "checklist_update",
        description: "DeepSeek-TUI-compatible alias for todo_update; update one checklist item by 1-based id.",
        properties_json: r#"{"id":{"type":"string","description":"1-based checklist item id."},"status":{"type":"string","description":"New status: pending, in_progress, or completed."}}"#,
        required_json: r#"["id","status"]"#,
    },
    StaticToolSpec {
        name: "todo_list",
        description: "List the current todo/checklist progress.",
        properties_json: r#"{}"#,
        required_json: r#"[]"#,
    },
    StaticToolSpec {
        name: "checklist_list",
        description: "DeepSeek-TUI-compatible alias for todo_list; list current checklist progress.",
        properties_json: r#"{}"#,
        required_json: r#"[]"#,
    },
    StaticToolSpec {
        name: "dispatch_subagent",
        description: "Delegate an independent subtask to a child agent with its own budget and todo list.",
        properties_json: r#"{"task":{"type":"string","description":"Concrete self-contained subtask for the child agent."},"agent":{"type":"string","description":"Optional custom subagent name from `.dscode/agents` or `~/.config/dscode/agents`."},"skill":{"type":"string","description":"Optional skill name for the child agent."},"steps":{"type":"string","description":"Optional step budget for the child agent, as a positive integer up to 12."}}"#,
        required_json: r#"["task"]"#,
    },
    StaticToolSpec {
        name: "dispatch_subagents",
        description: "Delegate multiple independent subtasks to child agents in parallel and return consolidated thread summaries.",
        properties_json: r#"{"tasks":{"type":"string","description":"JSON array of child task objects. Each object requires task and may include agent, skill, and steps, for example [{\"task\":\"review src/api.rs\",\"agent\":\"reviewer\",\"steps\":\"4\"}]. Maximum 4 child tasks."}}"#,
        required_json: r#"["tasks"]"#,
    },
    StaticToolSpec {
        name: "rlm",
        description: "Run bounded RLM-style analysis. Supports context+question for lightweight synthesis, or DeepSeek-TUI-style task plus file_path/content or existing session_id continuation for long-input processing.",
        properties_json: r#"{"context":{"type":"string","description":"The long text, extracted data, or notes for lightweight child analysis."},"question":{"type":"string","description":"Specific question the child analysis should answer when using context mode."},"task":{"type":"string","description":"DeepSeek-TUI-style objective for long-input RLM processing."},"file_path":{"type":"string","description":"Workspace-relative file to load as long input for task mode. Mutually exclusive with content; optional when session_id continues an existing session."},"content":{"type":"string","description":"Inline long input for task mode, capped at 200k chars. Mutually exclusive with file_path; optional when session_id continues an existing session."},"strategy":{"type":"string","description":"Optional strategy label such as synthesize, classify, compare, critique, or extract."},"steps":{"type":"string","description":"Optional child step budget as a positive integer up to 12."},"max_depth":{"type":"string","description":"Compatibility field for DeepSeek-TUI RLM; used as child step budget when steps is omitted."},"session_id":{"type":"string","description":"Optional durable RLM model session id. Prior summaries are stored under .dscode/rlm-model; with task and no file_path/content, an existing non-empty session is continued."},"reset":{"type":"string","description":"Set true/1/yes/on with session_id to clear the durable RLM model session before this process call."},"live":{"type":"string","description":"Set true/1/yes/on with session_id to enqueue a live RLM daemon turn instead of running the bounded child-agent call immediately."}}"#,
        required_json: r#"[]"#,
    },
    StaticToolSpec {
        name: "rlm_query",
        description: "DeepSeek-TUI-compatible alias for rlm: run context+question analysis or task plus file_path/content or existing session_id continuation.",
        properties_json: r#"{"context":{"type":"string","description":"The long text, extracted data, or notes for lightweight child analysis."},"question":{"type":"string","description":"Specific question the child analysis should answer when using context mode."},"task":{"type":"string","description":"DeepSeek-TUI-style objective for long-input RLM processing."},"file_path":{"type":"string","description":"Workspace-relative file to load as long input for task mode. Mutually exclusive with content; optional when session_id continues an existing session."},"content":{"type":"string","description":"Inline long input for task mode, capped at 200k chars. Mutually exclusive with file_path; optional when session_id continues an existing session."},"strategy":{"type":"string","description":"Optional strategy label such as synthesize, classify, compare, critique, or extract."},"steps":{"type":"string","description":"Optional child step budget as a positive integer up to 12."},"max_depth":{"type":"string","description":"Compatibility field for DeepSeek-TUI RLM; used as child step budget when steps is omitted."},"session_id":{"type":"string","description":"Optional durable RLM model session id. Prior summaries are stored under .dscode/rlm-model; with task and no file_path/content, an existing non-empty session is continued."},"reset":{"type":"string","description":"Set true/1/yes/on with session_id to clear the durable RLM model session before this process call."},"live":{"type":"string","description":"Set true/1/yes/on with session_id to enqueue a live RLM daemon turn instead of running the bounded child-agent call immediately."}}"#,
        required_json: r#"[]"#,
    },
    StaticToolSpec {
        name: "llm_query",
        description: "DeepSeek-TUI-compatible alias for rlm: run one bounded child analysis, task plus file_path/content, or existing session_id continuation.",
        properties_json: r#"{"context":{"type":"string","description":"The long text, extracted data, or notes for lightweight child analysis."},"question":{"type":"string","description":"Specific question the child analysis should answer when using context mode."},"task":{"type":"string","description":"DeepSeek-TUI-style objective for long-input RLM processing."},"file_path":{"type":"string","description":"Workspace-relative file to load as long input for task mode. Mutually exclusive with content; optional when session_id continues an existing session."},"content":{"type":"string","description":"Inline long input for task mode, capped at 200k chars. Mutually exclusive with file_path; optional when session_id continues an existing session."},"strategy":{"type":"string","description":"Optional strategy label such as synthesize, classify, compare, critique, or extract."},"steps":{"type":"string","description":"Optional child step budget as a positive integer up to 12."},"max_depth":{"type":"string","description":"Compatibility field for DeepSeek-TUI RLM; used as child step budget when steps is omitted."},"session_id":{"type":"string","description":"Optional durable RLM model session id. Prior summaries are stored under .dscode/rlm-model; with task and no file_path/content, an existing non-empty session is continued."},"reset":{"type":"string","description":"Set true/1/yes/on with session_id to clear the durable RLM model session before this process call."},"live":{"type":"string","description":"Set true/1/yes/on with session_id to enqueue a live RLM daemon turn instead of running the bounded child-agent call immediately."}}"#,
        required_json: r#"[]"#,
    },
    StaticToolSpec {
        name: "rlm_process",
        description: "DeepSeek-TUI-compatible RLM process entrypoint for long-input work. Uses the bounded child-agent adapter; optional session_id persists prior summaries and can continue an existing non-empty session without new file_path/content.",
        properties_json: r#"{"task":{"type":"string","description":"Objective for long-input RLM processing."},"file_path":{"type":"string","description":"Workspace-relative file to load as long input. Mutually exclusive with content; optional when session_id continues an existing non-empty session."},"content":{"type":"string","description":"Inline long input, capped at 200k chars. Mutually exclusive with file_path; optional when session_id continues an existing non-empty session."},"steps":{"type":"string","description":"Optional child step budget as a positive integer up to 12."},"max_depth":{"type":"string","description":"DeepSeek-TUI compatibility field; used as child step budget when steps is omitted."},"session_id":{"type":"string","description":"Optional durable RLM model session id. Prior summaries are stored under .dscode/rlm-model and injected into later calls; with no file_path/content, an existing non-empty session is continued."},"reset":{"type":"string","description":"Set true/1/yes/on with session_id to clear the durable RLM model session before this process call."},"live":{"type":"string","description":"Set true/1/yes/on with session_id to enqueue a live RLM daemon turn instead of running the bounded child-agent call immediately."}}"#,
        required_json: r#"["task"]"#,
    },
    StaticToolSpec {
        name: "rlm_chunk_plan",
        description: "Plan DeepSeek-TUI-style RLM chunks for a workspace file or inline content without running Python or a child model. Returns chunk start/end offsets, coverage metadata, and optionally chunk text for map-reduce setup.",
        properties_json: r#"{"file_path":{"type":"string","description":"Workspace-relative file to chunk. Mutually exclusive with content."},"content":{"type":"string","description":"Inline long input to chunk, capped at 200k chars. Mutually exclusive with file_path."},"max_chars":{"type":"string","description":"Maximum characters per chunk. Defaults to 20000 and clamps to 1-50000."},"overlap":{"type":"string","description":"Characters to overlap between adjacent chunks. Must be smaller than max_chars."},"include_text":{"type":"string","description":"Set false/0/no/off to omit chunk text and return offsets only. Defaults to true."}}"#,
        required_json: r#"[]"#,
    },
    StaticToolSpec {
        name: "rlm_map_reduce_plan",
        description: "Plan a DeepSeek-TUI-style RLM map-reduce workflow for a workspace file or inline content without immediately running child agents. Returns chunks, ready-to-dispatch map task JSON, omitted map count when map_limit is exceeded, and a reduce prompt.",
        properties_json: r#"{"task":{"type":"string","description":"Overall objective or question for the map-reduce workflow."},"question":{"type":"string","description":"Alias for task."},"file_path":{"type":"string","description":"Workspace-relative file to chunk. Mutually exclusive with content."},"content":{"type":"string","description":"Inline long input to chunk, capped at 200k chars. Mutually exclusive with file_path."},"max_chars":{"type":"string","description":"Maximum characters per chunk. Defaults to 20000 and clamps to 1-50000."},"overlap":{"type":"string","description":"Characters to overlap between adjacent chunks. Must be smaller than max_chars."},"include_text":{"type":"string","description":"Set false/0/no/off to omit chunk text from chunks and map tasks. Defaults to true."},"map_limit":{"type":"string","description":"Maximum map tasks to emit in this plan, clamped to 1-16. Defaults to 16."},"steps":{"type":"string","description":"Suggested child-agent step budget for each map task. Defaults to 4."}}"#,
        required_json: r#"["task"]"#,
    },
    StaticToolSpec {
        name: "rlm_recursive_plan",
        description: "Plan a DeepSeek-TUI-style recursive RLM map/reduce workflow for long input without running child agents. Returns chunks, initial map tasks, and multi-round fan-in reduce groups with stable input/output refs.",
        properties_json: r#"{"task":{"type":"string","description":"Overall objective or question for the recursive workflow."},"question":{"type":"string","description":"Alias for task."},"file_path":{"type":"string","description":"Workspace-relative file to chunk. Mutually exclusive with content."},"content":{"type":"string","description":"Inline long input to chunk, capped at 200k chars. Mutually exclusive with file_path."},"max_chars":{"type":"string","description":"Maximum characters per chunk. Defaults to 20000 and clamps to 1-50000."},"overlap":{"type":"string","description":"Characters to overlap between adjacent chunks. Must be smaller than max_chars."},"include_text":{"type":"string","description":"Set false/0/no/off to omit chunk text from chunks and map tasks. Defaults to true."},"map_limit":{"type":"string","description":"Maximum initial map tasks to emit in this plan, clamped to 1-16. Defaults to 16."},"fan_in":{"type":"string","description":"Maximum input summaries per recursive reduce group, clamped to 2-16. Defaults to 8."},"steps":{"type":"string","description":"Suggested child-agent step budget for each map or reduce task. Defaults to 4."}}"#,
        required_json: r#"["task"]"#,
    },
    StaticToolSpec {
        name: "rlm_python",
        description: "Run a short restricted Python helper script for RLM-style pure computation, text splitting, counting, classification setup, or aggregation. Includes chunk_context, chunk_coverage, SHOW_VARS, repl_get/repl_set, FINAL, and FINAL_VAR helpers. No imports, file, network, subprocess, or OS access.",
        properties_json: r#"{"code":{"type":"string","description":"Short Python code to execute in the restricted helper. It can read context (alias ctx) and question variables, print, and assign JSON-serializable variables."},"context":{"type":"string","description":"Optional text or extracted data exposed to Python as context and ctx."},"question":{"type":"string","description":"Optional question exposed to Python as question."},"timeout_ms":{"type":"string","description":"Optional timeout in milliseconds, clamped to 100-5000."}}"#,
        required_json: r#"["code"]"#,
    },
    StaticToolSpec {
        name: "rlm_python_session",
        description: "Run a short restricted Python helper script with REPL-like persisted JSON locals keyed by session_id. Safe JSON state keys are preloaded as locals and JSON-serializable locals are saved back after each call. Set persistent=true to reuse a long-lived Python process for the same session_id within this DeepSeekCode process; reset=true clears state and rebuilds that process. Includes chunk_context, chunk_coverage, SHOW_VARS, repl_get/repl_set, FINAL, and FINAL_VAR helpers for incremental RLM-style counting, chunk indexes, classification caches, and aggregation across calls.",
        properties_json: r#"{"session_id":{"type":"string","description":"Safe state session id using letters, numbers, underscore, dash, or dot."},"code":{"type":"string","description":"Short Python code to execute in the restricted helper. It can read/write the state dict and read context/ctx/question."},"context":{"type":"string","description":"Optional text or extracted data exposed to Python as context and ctx."},"question":{"type":"string","description":"Optional question exposed to Python as question."},"timeout_ms":{"type":"string","description":"Optional timeout in milliseconds, clamped to 100-5000."},"reset":{"type":"string","description":"Set to true/1/yes/on to clear this session state before running code. With persistent=true, also closes and rebuilds the cached Python process."},"persistent":{"type":"string","description":"Set to true/1/yes/on to keep and reuse a Python REPL process for this session_id while DeepSeekCode is running."}}"#,
        required_json: r#"["session_id","code"]"#,
    },
    StaticToolSpec {
        name: "rlm_python_sessions",
        description: "List or inspect persisted rlm_python_session JSON state files without running Python. Use to discover existing RLM helper sessions, inspect caches, verify whether a session exists, and see whether a persistent Python process is currently active for that session.",
        properties_json: r#"{"session_id":{"type":"string","description":"Optional safe state session id to inspect. Omit to list sessions."},"limit":{"type":"string","description":"Optional list limit, clamped to 1-100 and defaulting to 20."}}"#,
        required_json: r#"[]"#,
    },
    StaticToolSpec {
        name: "rlm_process_sessions",
        description: "List or inspect persisted rlm_process durable model-session summaries, optionally including live RLM daemon manifests, daemon owner liveness/stale status, and live turn inventory, without running a child model. Use to discover existing long-input RLM process sessions before continuing or resetting them.",
        properties_json: r#"{"session_id":{"type":"string","description":"Optional durable RLM model session id to inspect. Omit to list sessions."},"limit":{"type":"string","description":"Optional list limit, clamped to 1-100 and defaulting to 20."},"include_live":{"type":"string","description":"Set true/1/yes/on to include .dscode/rlm-daemon live-session manifests alongside legacy .dscode/rlm-model summaries, including daemon_alive, daemon_stale, and daemon_owner."},"include_turns":{"type":"string","description":"Set true/1/yes/on to include live turn payload inventory with runtime status, input metadata, result preview, and error preview. Implies include_live."}}"#,
        required_json: r#"[]"#,
    },
    StaticToolSpec {
        name: "rlm_process_status",
        description: "Summarize live RLM daemon lifecycle status without running a model. Reports owner liveness/stale state, queue counts, active turn status, and recommended next commands for one session or all live sessions.",
        properties_json: r#"{"session_id":{"type":"string","description":"Optional live RLM session id to inspect. Omit to summarize all live sessions."},"limit":{"type":"string","description":"Optional list limit for all-session status, clamped to 1-100 and defaulting to 20."}}"#,
        required_json: r#"[]"#,
    },
    StaticToolSpec {
        name: "rlm_process_events",
        description: "Replay live RLM daemon event-log records from .dscode/rlm-daemon without running a model. Use cursor to continue from the last seen event seq.",
        properties_json: r#"{"session_id":{"type":"string","description":"Live RLM session id."},"cursor":{"type":"string","description":"Return events with seq greater than this cursor. Defaults to 0."},"after_seq":{"type":"string","description":"Alias for cursor."},"limit":{"type":"string","description":"Optional event limit, clamped to 1-500 and defaulting to 50."}}"#,
        required_json: r#"["session_id"]"#,
    },
    StaticToolSpec {
        name: "rlm_process_wait",
        description: "Wait for live RLM daemon event-log records after a cursor without running a model. Returns immediately when events are available or when timeout_ms elapses.",
        properties_json: r#"{"session_id":{"type":"string","description":"Live RLM session id."},"cursor":{"type":"string","description":"Return events with seq greater than this cursor. Defaults to 0."},"after_seq":{"type":"string","description":"Alias for cursor."},"limit":{"type":"string","description":"Optional event limit, clamped to 1-500 and defaulting to 50."},"timeout_ms":{"type":"string","description":"Maximum wait in milliseconds, clamped to 30000 and defaulting to 1000."},"poll_interval_ms":{"type":"string","description":"Polling interval in milliseconds, clamped to 25-1000 and defaulting to 100."}}"#,
        required_json: r#"["session_id"]"#,
    },
    StaticToolSpec {
        name: "rlm_process_cancel",
        description: "Cancel queued pending or active running live RLM daemon turns for a session. Active worker cancellation is cooperative through the runtime task cancel path.",
        properties_json: r#"{"session_id":{"type":"string","description":"Live RLM session id."},"task_id":{"type":"string","description":"Runtime task id for the queued or active turn to cancel."},"turn_id":{"type":"string","description":"Alias for task_id."},"id":{"type":"string","description":"Alias for task_id."},"all":{"type":"string","description":"Set true/1/yes/on to cancel all queued pending or active running turns in the live session."},"reason":{"type":"string","description":"Optional cancellation reason stored on the runtime task and live event log."}}"#,
        required_json: r#"["session_id"]"#,
    },
    StaticToolSpec {
        name: "rlm_process_recover",
        description: "Recover interrupted live RLM daemon turns for a session, or scan all live sessions with all=true. Use mode=requeue to make stale running turns pending again, or mode=fail to mark them failed. Use dry_run=true to preview actions. Running turns owned by a live daemon pid are skipped unless force=true is supplied.",
        properties_json: r#"{"session_id":{"type":"string","description":"Live RLM session id. Optional when all=true."},"all":{"type":"string","description":"Set true/1/yes/on to scan all live RLM sessions."},"limit":{"type":"string","description":"Maximum live sessions to scan when all=true, clamped to 1-100 and defaulting to 20."},"mode":{"type":"string","description":"Recovery mode: requeue or fail. Defaults to requeue."},"dry_run":{"type":"string","description":"Set true/1/yes/on to preview recovery actions without mutating state."},"force":{"type":"string","description":"Set true/1/yes/on to recover even when the live manifest daemon pid is still alive."},"reason":{"type":"string","description":"Optional recovery reason stored in turn_recovered events."}}"#,
        required_json: r#"[]"#,
    },
    StaticToolSpec {
        name: "rlm_process_stop",
        description: "Stop an idle live RLM daemon session. Cancels queued pending turns, writes a session_stopped event, and prevents accidental reuse until reset=true is supplied to rlm_process live=true.",
        properties_json: r#"{"session_id":{"type":"string","description":"Live RLM session id."},"reason":{"type":"string","description":"Optional stop reason stored in the live event log."}}"#,
        required_json: r#"["session_id"]"#,
    },
    StaticToolSpec {
        name: "rlm_process_run_next",
        description: "Claim and run the next queued live RLM daemon turn from its persisted payload, stamping daemon pid/epoch while the turn is running. Use dry_run=true to inspect the selected payload and rendered task without claiming it.",
        properties_json: r#"{"session_id":{"type":"string","description":"Live RLM session id."},"task_id":{"type":"string","description":"Optional runtime task id for a specific queued turn."},"turn_id":{"type":"string","description":"Alias for task_id."},"id":{"type":"string","description":"Alias for task_id."},"dry_run":{"type":"string","description":"Set true/1/yes/on to load the payload and render the child task without claiming or running it."}}"#,
        required_json: r#"["session_id"]"#,
    },
    StaticToolSpec {
        name: "rlm_process_drain",
        description: "Run up to max_turns queued live RLM daemon turns for a session by repeatedly claiming persisted payloads in FIFO order. Use dry_run=true to preview selected turns without claiming them.",
        properties_json: r#"{"session_id":{"type":"string","description":"Live RLM session id."},"max_turns":{"type":"string","description":"Maximum queued turns to run, clamped to 1-100 and defaulting to 10."},"dry_run":{"type":"string","description":"Set true/1/yes/on to preview selected queued turns without claiming or running them."}}"#,
        required_json: r#"["session_id"]"#,
    },
    StaticToolSpec {
        name: "rlm_batch",
        description: "Run batched bounded RLM-style child analyses over shared context. Use for several independent classification, extraction, comparison, or critique questions.",
        properties_json: r#"{"context":{"type":"string","description":"Shared long text, extracted data, or notes for all child analyses."},"questions":{"type":"string","description":"JSON array of up to 16 question strings, or objects with question plus optional context and strategy."},"strategy":{"type":"string","description":"Optional default strategy label such as synthesize, classify, compare, critique, or extract."},"steps":{"type":"string","description":"Optional child step budget per question as a positive integer up to 12."}}"#,
        required_json: r#"["context","questions"]"#,
    },
    StaticToolSpec {
        name: "rlm_query_batched",
        description: "DeepSeek-TUI-compatible alias for rlm_batch: run batched bounded RLM-style child analyses over shared context.",
        properties_json: r#"{"context":{"type":"string","description":"Shared long text, extracted data, or notes for all child analyses."},"questions":{"type":"string","description":"JSON array of up to 16 question strings, or objects with question plus optional context and strategy."},"strategy":{"type":"string","description":"Optional default strategy label such as synthesize, classify, compare, critique, or extract."},"steps":{"type":"string","description":"Optional child step budget per question as a positive integer up to 12."}}"#,
        required_json: r#"["context","questions"]"#,
    },
    StaticToolSpec {
        name: "llm_query_batched",
        description: "DeepSeek-TUI-compatible alias for rlm_batch: run up to 16 bounded child analyses over shared context.",
        properties_json: r#"{"context":{"type":"string","description":"Shared long text, extracted data, or notes for all child analyses."},"questions":{"type":"string","description":"JSON array of up to 16 question strings, or objects with question plus optional context and strategy."},"strategy":{"type":"string","description":"Optional default strategy label such as synthesize, classify, compare, critique, or extract."},"steps":{"type":"string","description":"Optional child step budget per question as a positive integer up to 12."}}"#,
        required_json: r#"["context","questions"]"#,
    },
    StaticToolSpec {
        name: "mcp_list_tools",
        description: "List tools exposed by configured stdio, HTTP, or SSE MCP servers. Use before mcp_call or dynamic mcp__server__tool calls when you need the remote tool schema.",
        properties_json: r#"{"server":{"type":"string","description":"Optional MCP server name. Omit to list enabled stdio, HTTP, or SSE MCP servers."}}"#,
        required_json: r#"[]"#,
    },
    StaticToolSpec {
        name: "mcp_call",
        description: "Call a configured stdio, HTTP, or SSE MCP server tool with JSON object arguments.",
        properties_json: r#"{"server":{"type":"string","description":"MCP server name from the project or user MCP config."},"tool":{"type":"string","description":"Remote MCP tool name to call."},"arguments":{"type":"string","description":"JSON object string containing tool arguments, for example {\"path\":\"README.md\"}."}}"#,
        required_json: r#"["server","tool"]"#,
    },
    StaticToolSpec {
        name: "mcp_list_prompts",
        description: "List prompts exposed by configured stdio, HTTP, or SSE MCP servers.",
        properties_json: r#"{"server":{"type":"string","description":"Optional MCP server name. Omit to list enabled stdio, HTTP, or SSE MCP servers."}}"#,
        required_json: r#"[]"#,
    },
    StaticToolSpec {
        name: "mcp_get_prompt",
        description: "Get a prompt from a configured stdio, HTTP, or SSE MCP server with optional JSON object arguments.",
        properties_json: r#"{"server":{"type":"string","description":"MCP server name from the project or user MCP config."},"prompt":{"type":"string","description":"Prompt name returned by mcp_list_prompts."},"arguments":{"type":"string","description":"Optional JSON object string containing prompt arguments, for example {\"number\":42}."}}"#,
        required_json: r#"["server","prompt"]"#,
    },
    StaticToolSpec {
        name: "mcp_list_resources",
        description: "List read-only resources exposed by configured stdio, HTTP, or SSE MCP servers.",
        properties_json: r#"{"server":{"type":"string","description":"Optional MCP server name. Omit to list enabled stdio, HTTP, or SSE MCP servers."}}"#,
        required_json: r#"[]"#,
    },
    StaticToolSpec {
        name: "mcp_read_resource",
        description: "Read a resource from a configured stdio, HTTP, or SSE MCP server by URI.",
        properties_json: r#"{"server":{"type":"string","description":"MCP server name from the project or user MCP config."},"uri":{"type":"string","description":"Resource URI returned by mcp_list_resources."}}"#,
        required_json: r#"["server","uri"]"#,
    },
    StaticToolSpec {
        name: "mcp_list_resource_templates",
        description: "List resource templates exposed by configured stdio, HTTP, or SSE MCP servers.",
        properties_json: r#"{"server":{"type":"string","description":"Optional MCP server name. Omit to list enabled stdio, HTTP, or SSE MCP servers."}}"#,
        required_json: r#"[]"#,
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

fn read_cancelable_frame<R: BufRead>(
    reader: &mut R,
    cancel_check: &mut Option<&mut dyn CancellationCheck>,
) -> AppResult<Option<SseFrame>> {
    poll_model_cancel(cancel_check)?;
    let frame = read_frame(reader).map_err(|e| app_error(format!("sse read failed: {e}")))?;
    poll_model_cancel(cancel_check)?;
    Ok(frame)
}

fn poll_model_cancel(cancel_check: &mut Option<&mut dyn CancellationCheck>) -> AppResult<()> {
    if let Some(check) = cancel_check.as_mut() {
        if check.is_cancelled()? {
            return Err(app_error("agent run cancelled"));
        }
    }
    Ok(())
}

fn parse_openai_process_stream(
    process: &mut StreamingProcess,
    events: &mut dyn StreamEvents,
    cancel_check: Option<&mut dyn CancellationCheck>,
) -> AppResult<(ModelResponse, Option<TokenUsage>)> {
    if let Some(cancel_check) = cancel_check {
        let stdout = process.take_stdout()?;
        let mut reader = CancelAwarePipeReader::spawn(stdout, cancel_check);
        parse_openai_stream_with_cancel(&mut reader, events, None)
    } else {
        parse_openai_stream_with_cancel(process.stdout_mut()?, events, None)
    }
}

fn parse_anthropic_process_stream(
    process: &mut StreamingProcess,
    events: &mut dyn StreamEvents,
    cancel_check: Option<&mut dyn CancellationCheck>,
) -> AppResult<(ModelResponse, Option<TokenUsage>)> {
    if let Some(cancel_check) = cancel_check {
        let stdout = process.take_stdout()?;
        let mut reader = CancelAwarePipeReader::spawn(stdout, cancel_check);
        parse_anthropic_stream_with_cancel(&mut reader, events, None)
    } else {
        parse_anthropic_stream_with_cancel(process.stdout_mut()?, events, None)
    }
}

enum PipeChunk {
    Data(Vec<u8>),
    Eof,
    Error(String),
}

struct CancelAwarePipeReader<'a> {
    receiver: Receiver<PipeChunk>,
    buffer: Vec<u8>,
    position: usize,
    done: bool,
    cancel_check: &'a mut dyn CancellationCheck,
}

impl<'a> CancelAwarePipeReader<'a> {
    fn spawn<R>(mut reader: R, cancel_check: &'a mut dyn CancellationCheck) -> Self
    where
        R: Read + Send + 'static,
    {
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            let mut chunk = [0_u8; 8192];
            loop {
                match reader.read(&mut chunk) {
                    Ok(0) => {
                        let _ = sender.send(PipeChunk::Eof);
                        break;
                    }
                    Ok(bytes) => {
                        if sender
                            .send(PipeChunk::Data(chunk[..bytes].to_vec()))
                            .is_err()
                        {
                            break;
                        }
                    }
                    Err(error) => {
                        let _ = sender.send(PipeChunk::Error(error.to_string()));
                        break;
                    }
                }
            }
        });
        Self {
            receiver,
            buffer: Vec::new(),
            position: 0,
            done: false,
            cancel_check,
        }
    }

    fn poll_cancel(&mut self) -> io::Result<()> {
        if self
            .cancel_check
            .is_cancelled()
            .map_err(|error| io::Error::new(io::ErrorKind::Other, error.to_string()))?
        {
            return Err(io::Error::new(io::ErrorKind::Other, "agent run cancelled"));
        }
        Ok(())
    }
}

impl Read for CancelAwarePipeReader<'_> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if output.is_empty() {
            return Ok(0);
        }
        let available = self.fill_buf()?;
        if available.is_empty() {
            return Ok(0);
        }
        let bytes = available.len().min(output.len());
        output[..bytes].copy_from_slice(&available[..bytes]);
        self.consume(bytes);
        Ok(bytes)
    }
}

impl BufRead for CancelAwarePipeReader<'_> {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        loop {
            if self.position < self.buffer.len() {
                return Ok(&self.buffer[self.position..]);
            }
            self.buffer.clear();
            self.position = 0;
            if self.done {
                return Ok(&[]);
            }
            self.poll_cancel()?;
            match self.receiver.recv_timeout(Duration::from_millis(100)) {
                Ok(PipeChunk::Data(bytes)) => {
                    self.buffer = bytes;
                }
                Ok(PipeChunk::Eof) | Err(RecvTimeoutError::Disconnected) => {
                    self.done = true;
                    return Ok(&[]);
                }
                Ok(PipeChunk::Error(error)) => {
                    return Err(io::Error::new(io::ErrorKind::Other, error));
                }
                Err(RecvTimeoutError::Timeout) => {}
            }
        }
    }

    fn consume(&mut self, amount: usize) {
        self.position = self.position.saturating_add(amount).min(self.buffer.len());
    }
}

#[cfg(test)]
pub(crate) fn parse_openai_stream<R: BufRead>(
    reader: &mut R,
    events: &mut dyn StreamEvents,
) -> AppResult<(ModelResponse, Option<TokenUsage>)> {
    parse_openai_stream_with_cancel(reader, events, None)
}

fn parse_openai_stream_with_cancel<R: BufRead>(
    reader: &mut R,
    events: &mut dyn StreamEvents,
    cancel_check: Option<&mut dyn CancellationCheck>,
) -> AppResult<(ModelResponse, Option<TokenUsage>)> {
    let mut full_text = String::new();
    let result = parse_openai_stream_inner(reader, events, &mut full_text, cancel_check);
    events.on_assistant_done(&full_text);
    result
}

fn parse_openai_stream_inner<R: BufRead>(
    reader: &mut R,
    events: &mut dyn StreamEvents,
    full_text: &mut String,
    mut cancel_check: Option<&mut dyn CancellationCheck>,
) -> AppResult<(ModelResponse, Option<TokenUsage>)> {
    let mut usage: Option<TokenUsage> = None;
    let mut tool_assembly: Option<OpenAiToolAssembly> = None;
    let mut done_seen = false;

    while let Some(frame) = read_cancelable_frame(reader, &mut cancel_check)? {
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
            if let Some(parsed_usage) = parse_openai_usage_object(usage_obj) {
                usage = Some(parsed_usage);
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
                if let Some(reasoning) = delta.get("reasoning_content").and_then(json_as_string) {
                    if !reasoning.is_empty() {
                        events.on_reasoning_delta(reasoning);
                    }
                }
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

#[cfg(test)]
pub(crate) fn parse_anthropic_stream<R: BufRead>(
    reader: &mut R,
    events: &mut dyn StreamEvents,
) -> AppResult<(ModelResponse, Option<TokenUsage>)> {
    parse_anthropic_stream_with_cancel(reader, events, None)
}

fn parse_anthropic_stream_with_cancel<R: BufRead>(
    reader: &mut R,
    events: &mut dyn StreamEvents,
    cancel_check: Option<&mut dyn CancellationCheck>,
) -> AppResult<(ModelResponse, Option<TokenUsage>)> {
    let mut full_text = String::new();
    let result = parse_anthropic_stream_inner(reader, events, &mut full_text, cancel_check);
    events.on_assistant_done(&full_text);
    result
}

fn parse_anthropic_stream_inner<R: BufRead>(
    reader: &mut R,
    events: &mut dyn StreamEvents,
    full_text: &mut String,
    mut cancel_check: Option<&mut dyn CancellationCheck>,
) -> AppResult<(ModelResponse, Option<TokenUsage>)> {
    let mut tool_assembly: Option<AnthropicToolAssembly> = None;
    let mut usage_prompt: Option<u64> = None;
    let mut usage_completion: Option<u64> = None;
    let mut usage_prompt_cache_hit: Option<u64> = None;
    let mut usage_prompt_cache_miss: Option<u64> = None;

    while let Some(frame) = read_cancelable_frame(reader, &mut cancel_check)? {
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
                        apply_anthropic_usage_delta(
                            usage_obj,
                            &mut usage_prompt,
                            &mut usage_completion,
                            &mut usage_prompt_cache_hit,
                            &mut usage_prompt_cache_miss,
                        );
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
                        "thinking_delta" | "reasoning_delta" => {
                            if let Some(reasoning) = delta
                                .get("thinking")
                                .or_else(|| delta.get("reasoning_content"))
                                .or_else(|| delta.get("text"))
                                .and_then(json_as_string)
                            {
                                if !reasoning.is_empty() {
                                    events.on_reasoning_delta(reasoning);
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
                    apply_anthropic_usage_delta(
                        usage_obj,
                        &mut usage_prompt,
                        &mut usage_completion,
                        &mut usage_prompt_cache_hit,
                        &mut usage_prompt_cache_miss,
                    );
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
        (Some(p), Some(c)) => Some(TokenUsage::with_prompt_cache(
            p,
            c,
            usage_prompt_cache_hit.unwrap_or(0),
            usage_prompt_cache_miss
                .unwrap_or_else(|| p.saturating_sub(usage_prompt_cache_hit.unwrap_or(0))),
        )),
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
    parse_openai_usage_object(usage)
}

fn parse_openai_usage_object(usage: &BTreeMap<String, JsonValue>) -> Option<TokenUsage> {
    let prompt = json_as_u64(usage.get("prompt_tokens")?)?;
    let completion = json_as_u64(usage.get("completion_tokens")?)?;
    let (cache_hit, cache_miss) = openai_prompt_cache_tokens(usage, prompt);
    Some(TokenUsage::with_prompt_cache(
        prompt, completion, cache_hit, cache_miss,
    ))
}

fn openai_prompt_cache_tokens(usage: &BTreeMap<String, JsonValue>, prompt: u64) -> (u64, u64) {
    let direct_hit = usage.get("prompt_cache_hit_tokens").and_then(json_as_u64);
    let direct_miss = usage.get("prompt_cache_miss_tokens").and_then(json_as_u64);
    if direct_hit.is_some() || direct_miss.is_some() {
        let hit = direct_hit.unwrap_or_else(|| prompt.saturating_sub(direct_miss.unwrap_or(0)));
        let miss = direct_miss.unwrap_or_else(|| prompt.saturating_sub(hit));
        return (hit, miss);
    }

    let details_hit = usage
        .get("prompt_tokens_details")
        .and_then(json_as_object)
        .and_then(|details| {
            details
                .get("cached_tokens")
                .or_else(|| details.get("cache_read_tokens"))
                .or_else(|| details.get("prompt_cache_hit_tokens"))
                .and_then(json_as_u64)
        })
        .unwrap_or(0);
    (details_hit, prompt.saturating_sub(details_hit))
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
    parse_anthropic_usage_object(usage)
}

fn parse_anthropic_usage_object(usage: &BTreeMap<String, JsonValue>) -> Option<TokenUsage> {
    let prompt = json_as_u64(usage.get("input_tokens")?)?;
    let completion = json_as_u64(usage.get("output_tokens")?)?;
    let (cache_hit, cache_miss) = anthropic_prompt_cache_tokens(usage, prompt);
    Some(TokenUsage::with_prompt_cache(
        prompt, completion, cache_hit, cache_miss,
    ))
}

fn apply_anthropic_usage_delta(
    usage: &BTreeMap<String, JsonValue>,
    prompt: &mut Option<u64>,
    completion: &mut Option<u64>,
    cache_hit: &mut Option<u64>,
    cache_miss: &mut Option<u64>,
) {
    if let Some(p) = usage.get("input_tokens").and_then(json_as_u64) {
        *prompt = Some(p);
    }
    if let Some(c) = usage.get("output_tokens").and_then(json_as_u64) {
        *completion = Some(c);
    }
    if let Some(hit) = usage
        .get("prompt_cache_hit_tokens")
        .or_else(|| usage.get("cache_read_input_tokens"))
        .and_then(json_as_u64)
    {
        *cache_hit = Some(hit);
    }
    if let Some(miss) = usage
        .get("prompt_cache_miss_tokens")
        .or_else(|| usage.get("cache_creation_input_tokens"))
        .and_then(json_as_u64)
    {
        *cache_miss = Some(miss);
    }
}

fn anthropic_prompt_cache_tokens(usage: &BTreeMap<String, JsonValue>, prompt: u64) -> (u64, u64) {
    let hit = usage
        .get("prompt_cache_hit_tokens")
        .or_else(|| usage.get("cache_read_input_tokens"))
        .and_then(json_as_u64);
    let miss = usage
        .get("prompt_cache_miss_tokens")
        .or_else(|| usage.get("cache_creation_input_tokens"))
        .and_then(json_as_u64);
    if hit.is_some() || miss.is_some() {
        let hit = hit.unwrap_or_else(|| prompt.saturating_sub(miss.unwrap_or(0)));
        let miss = miss.unwrap_or_else(|| prompt.saturating_sub(hit));
        (hit, miss)
    } else {
        (0, prompt)
    }
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
                && !matches!(*word, "DeepSeek" | "DeepSeekCode" | "Cargo" | "README")
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
        anthropic_tool_fields, api_flavor, build_anthropic_tools, build_openai_tools,
        child_files_from_summary, derive_edit_request, derive_github_pr_context_request,
        derive_search_query, last_patched_file_path, openai_tool_fields, parse_anthropic_messages,
        parse_anthropic_usage, parse_openai_chat_completion, parse_openai_usage, ApiFlavor,
        DeepSeekClient, GithubPrContextRequest, ReasoningTier,
    };
    use crate::config::types::ModelConfig;
    use crate::model::client::ModelClient;
    use crate::model::protocol::{ImageInput, ModelAction, ModelRequest, Observation};
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
        let tools = build_openai_tools(&[
            "read_file".to_string(),
            "git_diff".to_string(),
            "git_log".to_string(),
            "git_show".to_string(),
            "git_blame".to_string(),
            "diagnostics".to_string(),
        ]);
        assert!(tools.contains("\"name\":\"read_file\""));
        assert!(tools.contains("\"name\":\"git_diff\""));
        assert!(tools.contains("\"cached\""));
        assert!(tools.contains("\"unified\""));
        assert!(tools.contains("\"name\":\"git_log\""));
        assert!(tools.contains("\"name\":\"git_show\""));
        assert!(tools.contains("\"name\":\"git_blame\""));
        assert!(tools.contains("\"name\":\"diagnostics\""));
        assert!(tools.contains("\"path\""));
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
    fn tool_fields_are_omitted_for_no_tool_requests() {
        assert_eq!(openai_tool_fields(&[], ReasoningTier::Off), "");
        assert_eq!(anthropic_tool_fields(&[], ReasoningTier::Off), "");

        let openai = openai_tool_fields(&["read_file".to_string()], ReasoningTier::Off);
        assert!(openai.contains("\"tool_choice\":\"auto\""));
        assert!(openai.contains("\"parallel_tool_calls\":false"));
        assert!(openai.contains("\"tools\":["));

        let anthropic = anthropic_tool_fields(&["read_file".to_string()], ReasoningTier::Off);
        assert!(anthropic.contains("\"tool_choice\":{\"type\":\"auto\"}"));
        assert!(anthropic.contains("\"tools\":["));
    }

    #[test]
    fn build_openai_tools_includes_todo_checklist_tools() {
        let tools = build_openai_tools(&[
            "todo_write".to_string(),
            "update_plan".to_string(),
            "checklist_write".to_string(),
            "todo_add".to_string(),
            "checklist_add".to_string(),
            "todo_update".to_string(),
            "checklist_update".to_string(),
            "todo_list".to_string(),
            "checklist_list".to_string(),
        ]);
        assert!(tools.contains("\"name\":\"todo_write\""));
        assert!(tools.contains("\"name\":\"update_plan\""));
        assert!(tools.contains("\"name\":\"checklist_write\""));
        assert!(tools.contains("\"name\":\"todo_add\""));
        assert!(tools.contains("\"name\":\"checklist_add\""));
        assert!(tools.contains("\"name\":\"todo_update\""));
        assert!(tools.contains("\"name\":\"checklist_update\""));
        assert!(tools.contains("\"name\":\"todo_list\""));
        assert!(tools.contains("\"name\":\"checklist_list\""));
        assert!(tools.contains("\"items\""));
        assert!(tools.contains("\"plan\""));
    }

    #[test]
    fn build_anthropic_tools_includes_todo_checklist_tools() {
        let tools = build_anthropic_tools(&[
            "todo_write".to_string(),
            "update_plan".to_string(),
            "checklist_write".to_string(),
            "todo_add".to_string(),
            "checklist_add".to_string(),
            "todo_update".to_string(),
            "checklist_update".to_string(),
            "todo_list".to_string(),
            "checklist_list".to_string(),
        ]);
        assert!(tools.contains("\"name\":\"todo_write\""));
        assert!(tools.contains("\"name\":\"update_plan\""));
        assert!(tools.contains("\"name\":\"checklist_write\""));
        assert!(tools.contains("\"name\":\"todo_add\""));
        assert!(tools.contains("\"name\":\"checklist_add\""));
        assert!(tools.contains("\"name\":\"todo_update\""));
        assert!(tools.contains("\"name\":\"checklist_update\""));
        assert!(tools.contains("\"name\":\"todo_list\""));
        assert!(tools.contains("\"name\":\"checklist_list\""));
        assert!(tools.contains("\"items\""));
        assert!(tools.contains("\"plan\""));
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

    #[test]
    fn build_tool_specs_include_readonly_search_aliases() {
        let openai = build_openai_tools(&[
            "list_dir".to_string(),
            "grep_files".to_string(),
            "file_search".to_string(),
        ]);
        assert!(openai.contains("\"name\":\"list_dir\""));
        assert!(openai.contains("\"name\":\"grep_files\""));
        assert!(openai.contains("\"name\":\"file_search\""));
        assert!(openai.contains("\"pattern\""));
        assert!(openai.contains("\"extensions\""));

        let anthropic = build_anthropic_tools(&[
            "list_dir".to_string(),
            "grep_files".to_string(),
            "file_search".to_string(),
        ]);
        assert!(anthropic.contains("\"name\":\"list_dir\""));
        assert!(anthropic.contains("\"name\":\"grep_files\""));
        assert!(anthropic.contains("\"name\":\"file_search\""));
        assert!(anthropic.contains("\"input_schema\""));
    }

    #[test]
    fn build_tool_specs_include_file_write_tools() {
        let openai = build_openai_tools(&[
            "write_file".to_string(),
            "edit_file".to_string(),
            "fim_edit".to_string(),
        ]);
        assert!(openai.contains("\"name\":\"write_file\""));
        assert!(openai.contains("\"name\":\"edit_file\""));
        assert!(openai.contains("\"name\":\"fim_edit\""));
        assert!(openai.contains("\"content\""));
        assert!(openai.contains("\"search\""));
        assert!(openai.contains("\"replace\""));
        assert!(openai.contains("\"prefix_anchor\""));
        assert!(openai.contains("\"suffix_anchor\""));

        let anthropic = build_anthropic_tools(&[
            "write_file".to_string(),
            "edit_file".to_string(),
            "fim_edit".to_string(),
        ]);
        assert!(anthropic.contains("\"name\":\"write_file\""));
        assert!(anthropic.contains("\"name\":\"edit_file\""));
        assert!(anthropic.contains("\"name\":\"fim_edit\""));
        assert!(anthropic.contains("\"input_schema\""));
    }

    #[test]
    fn build_tool_specs_include_retrieve_tool_result() {
        let openai = build_openai_tools(&["retrieve_tool_result".to_string()]);
        assert!(openai.contains("\"name\":\"retrieve_tool_result\""));
        assert!(openai.contains("\"mode\""));
        assert!(openai.contains("\"query\""));

        let anthropic = build_anthropic_tools(&["retrieve_tool_result".to_string()]);
        assert!(anthropic.contains("\"name\":\"retrieve_tool_result\""));
        assert!(anthropic.contains("\"input_schema\""));
    }

    #[test]
    fn build_tool_specs_include_web_search_and_fetch_url() {
        let openai = build_openai_tools(&[
            "web_run".to_string(),
            "web_search".to_string(),
            "fetch_url".to_string(),
        ]);
        assert!(openai.contains("\"name\":\"web_run\""));
        assert!(openai.contains("\"search_query\""));
        assert!(openai.contains("\"image_query\""));
        assert!(openai.contains("\"open\""));
        assert!(openai.contains("\"click\""));
        assert!(openai.contains("\"find\""));
        assert!(openai.contains("\"screenshot\""));
        assert!(openai.contains("\"name\":\"web_search\""));
        assert!(openai.contains("\"name\":\"fetch_url\""));
        assert!(openai.contains("\"search_query\""));
        assert!(openai.contains("\"max_bytes\""));

        let anthropic = build_anthropic_tools(&[
            "web_run".to_string(),
            "web_search".to_string(),
            "fetch_url".to_string(),
        ]);
        assert!(anthropic.contains("\"name\":\"web_run\""));
        assert!(anthropic.contains("\"name\":\"web_search\""));
        assert!(anthropic.contains("\"name\":\"fetch_url\""));
        assert!(anthropic.contains("\"input_schema\""));
    }

    #[test]
    fn build_tool_specs_include_finance() {
        let openai = build_openai_tools(&["finance".to_string()]);
        assert!(openai.contains("\"name\":\"finance\""));
        assert!(openai.contains("\"ticker\""));
        assert!(openai.contains("\"symbol\""));

        let anthropic = build_anthropic_tools(&["finance".to_string()]);
        assert!(anthropic.contains("\"name\":\"finance\""));
        assert!(anthropic.contains("\"input_schema\""));
    }

    #[test]
    fn build_tool_specs_include_document_tools() {
        let openai = build_openai_tools(&[
            "pandoc_convert".to_string(),
            "image_ocr".to_string(),
            "image_analyze".to_string(),
        ]);
        assert!(openai.contains("\"name\":\"pandoc_convert\""));
        assert!(openai.contains("\"source_path\""));
        assert!(openai.contains("\"target_format\""));
        assert!(openai.contains("\"name\":\"image_ocr\""));
        assert!(openai.contains("\"path\""));
        assert!(openai.contains("\"name\":\"image_analyze\""));
        assert!(openai.contains("\"image_path\""));

        let anthropic = build_anthropic_tools(&[
            "pandoc_convert".to_string(),
            "image_ocr".to_string(),
            "image_analyze".to_string(),
        ]);
        assert!(anthropic.contains("\"name\":\"pandoc_convert\""));
        assert!(anthropic.contains("\"name\":\"image_ocr\""));
        assert!(anthropic.contains("\"name\":\"image_analyze\""));
        assert!(anthropic.contains("\"input_schema\""));
    }

    #[test]
    fn build_tool_specs_include_review() {
        let openai = build_openai_tools(&["review".to_string()]);
        assert!(openai.contains("\"name\":\"review\""));
        assert!(openai.contains("\"target\""));
        assert!(openai.contains("\"staged\""));
        assert!(openai.contains("\"github_context\""));
        assert!(openai.contains("\"pr_context\""));
        assert!(openai.contains("\"semantic\""));
        assert!(openai.contains("\"steps\""));

        let anthropic = build_anthropic_tools(&["review".to_string()]);
        assert!(anthropic.contains("\"name\":\"review\""));
        assert!(anthropic.contains("\"input_schema\""));
    }

    #[test]
    fn build_tool_specs_include_pr_review_comment_plan() {
        let openai = build_openai_tools(&["pr_review_comment_plan".to_string()]);
        assert!(openai.contains("\"name\":\"pr_review_comment_plan\""));
        assert!(openai.contains("\"review_output\""));
        assert!(openai.contains("\"github_context\""));
        assert!(openai.contains("\"max_issues\""));

        let anthropic = build_anthropic_tools(&["pr_review_comment_plan".to_string()]);
        assert!(anthropic.contains("\"name\":\"pr_review_comment_plan\""));
        assert!(anthropic.contains("\"input_schema\""));
    }

    #[test]
    fn build_tool_specs_include_request_user_input() {
        let openai = build_openai_tools(&["request_user_input".to_string()]);
        assert!(openai.contains("\"name\":\"request_user_input\""));
        assert!(openai.contains("\"questions\""));
        assert!(openai.contains("\"options\""));
        assert!(openai.contains("\"minItems\":1"));
        assert!(openai.contains("\"maxItems\":3"));

        let anthropic = build_anthropic_tools(&["request_user_input".to_string()]);
        assert!(anthropic.contains("\"name\":\"request_user_input\""));
        assert!(anthropic.contains("\"input_schema\""));
    }

    #[test]
    fn build_tool_specs_include_tool_search_tools() {
        let names = [
            "tool_search_tool_regex".to_string(),
            "tool_search_tool_bm25".to_string(),
        ];
        let openai = build_openai_tools(&names);
        assert!(openai.contains("\"name\":\"tool_search_tool_regex\""));
        assert!(openai.contains("\"name\":\"tool_search_tool_bm25\""));
        assert!(openai.contains("\"query\""));
        assert!(openai.contains("\"limit\""));

        let anthropic = build_anthropic_tools(&names);
        assert!(anthropic.contains("\"name\":\"tool_search_tool_regex\""));
        assert!(anthropic.contains("\"name\":\"tool_search_tool_bm25\""));
        assert!(anthropic.contains("\"input_schema\""));
    }

    #[test]
    fn build_tool_specs_include_git_status_and_project_map() {
        let openai = build_openai_tools(&["git_status".to_string(), "project_map".to_string()]);
        assert!(openai.contains("\"name\":\"git_status\""));
        assert!(openai.contains("\"name\":\"project_map\""));
        assert!(openai.contains("\"max_depth\""));
        assert!(openai.contains("\"path\""));

        let anthropic =
            build_anthropic_tools(&["git_status".to_string(), "project_map".to_string()]);
        assert!(anthropic.contains("\"name\":\"git_status\""));
        assert!(anthropic.contains("\"name\":\"project_map\""));
        assert!(anthropic.contains("\"input_schema\""));
    }

    #[test]
    fn build_tool_specs_include_validate_data() {
        let openai = build_openai_tools(&["validate_data".to_string()]);
        assert!(openai.contains("\"name\":\"validate_data\""));
        assert!(openai.contains("\"content\""));
        assert!(openai.contains("\"format\""));

        let anthropic = build_anthropic_tools(&["validate_data".to_string()]);
        assert!(anthropic.contains("\"name\":\"validate_data\""));
        assert!(anthropic.contains("\"input_schema\""));
    }

    #[test]
    fn build_tool_specs_include_recall_archive() {
        let openai = build_openai_tools(&["recall_archive".to_string()]);
        assert!(openai.contains("\"name\":\"recall_archive\""));
        assert!(openai.contains("\"query\""));
        assert!(openai.contains("\"max_results\""));

        let anthropic = build_anthropic_tools(&["recall_archive".to_string()]);
        assert!(anthropic.contains("\"name\":\"recall_archive\""));
        assert!(anthropic.contains("\"input_schema\""));
    }

    #[test]
    fn build_tool_specs_include_github_context_tools() {
        let openai = build_openai_tools(&[
            "github_issue_context".to_string(),
            "github_pr_context".to_string(),
        ]);
        assert!(openai.contains("\"name\":\"github_issue_context\""));
        assert!(openai.contains("\"name\":\"github_pr_context\""));
        assert!(openai.contains("\"include_comments\""));
        assert!(openai.contains("\"include_diff\""));

        let anthropic = build_anthropic_tools(&[
            "github_issue_context".to_string(),
            "github_pr_context".to_string(),
        ]);
        assert!(anthropic.contains("\"name\":\"github_issue_context\""));
        assert!(anthropic.contains("\"name\":\"github_pr_context\""));
        assert!(anthropic.contains("\"input_schema\""));
    }

    #[test]
    fn build_tool_specs_include_load_skill() {
        let openai = build_openai_tools(&["load_skill".to_string()]);
        assert!(openai.contains("\"name\":\"load_skill\""));
        assert!(openai.contains("\"name\""));

        let anthropic = build_anthropic_tools(&["load_skill".to_string()]);
        assert!(anthropic.contains("\"name\":\"load_skill\""));
        assert!(anthropic.contains("\"input_schema\""));
    }

    #[test]
    fn build_tool_specs_include_note_and_remember() {
        let openai = build_openai_tools(&["note".to_string(), "remember".to_string()]);
        assert!(openai.contains("\"name\":\"note\""));
        assert!(openai.contains("\"name\":\"remember\""));
        assert!(openai.contains("\"content\""));
        assert!(openai.contains("\"note\""));

        let anthropic = build_anthropic_tools(&["note".to_string(), "remember".to_string()]);
        assert!(anthropic.contains("\"name\":\"note\""));
        assert!(anthropic.contains("\"name\":\"remember\""));
        assert!(anthropic.contains("\"input_schema\""));
    }

    #[test]
    fn build_tool_specs_include_notify() {
        let openai = build_openai_tools(&["notify".to_string()]);
        assert!(openai.contains("\"name\":\"notify\""));
        assert!(openai.contains("\"title\""));
        assert!(openai.contains("\"body\""));

        let anthropic = build_anthropic_tools(&["notify".to_string()]);
        assert!(anthropic.contains("\"name\":\"notify\""));
        assert!(anthropic.contains("\"input_schema\""));
    }

    #[test]
    fn build_tool_specs_include_github_write_tools() {
        let openai = build_openai_tools(&[
            "github_comment".to_string(),
            "github_pr_review_comment".to_string(),
            "github_close_issue".to_string(),
        ]);
        assert!(openai.contains("\"name\":\"github_comment\""));
        assert!(openai.contains("\"name\":\"github_pr_review_comment\""));
        assert!(openai.contains("\"name\":\"github_close_issue\""));
        assert!(openai.contains("\"evidence\""));
        assert!(openai.contains("\"comments\""));
        assert!(openai.contains("\"acceptance_criteria\""));

        let anthropic = build_anthropic_tools(&[
            "github_comment".to_string(),
            "github_pr_review_comment".to_string(),
            "github_close_issue".to_string(),
        ]);
        assert!(anthropic.contains("\"name\":\"github_comment\""));
        assert!(anthropic.contains("\"name\":\"github_pr_review_comment\""));
        assert!(anthropic.contains("\"name\":\"github_close_issue\""));
        assert!(anthropic.contains("\"input_schema\""));
    }

    #[test]
    fn build_tool_specs_include_runtime_task_and_automation_tools() {
        let names = [
            "task_create".to_string(),
            "task_list".to_string(),
            "task_read".to_string(),
            "task_cancel".to_string(),
            "task_gate_run".to_string(),
            "pr_attempt_record".to_string(),
            "pr_attempt_list".to_string(),
            "pr_attempt_read".to_string(),
            "pr_attempt_preflight".to_string(),
            "automation_create".to_string(),
            "automation_list".to_string(),
            "automation_read".to_string(),
            "automation_update".to_string(),
            "automation_pause".to_string(),
            "automation_resume".to_string(),
            "automation_delete".to_string(),
            "automation_run".to_string(),
        ];
        let openai = build_openai_tools(&names);
        for name in [
            "task_create",
            "task_list",
            "task_read",
            "task_cancel",
            "task_gate_run",
            "pr_attempt_record",
            "pr_attempt_list",
            "pr_attempt_read",
            "pr_attempt_preflight",
            "automation_create",
            "automation_list",
            "automation_read",
            "automation_update",
            "automation_pause",
            "automation_resume",
            "automation_delete",
            "automation_run",
        ] {
            assert!(openai.contains(&format!("\"name\":\"{name}\"")));
        }
        assert!(openai.contains("\"prompt\""));
        assert!(openai.contains("\"task_id\""));
        assert!(openai.contains("\"gate\""));
        assert!(openai.contains("\"attempt_id\""));
        assert!(openai.contains("\"verification\""));
        assert!(openai.contains("\"automation_id\""));
        assert!(openai.contains("\"rrule\""));

        let anthropic = build_anthropic_tools(&names);
        assert!(anthropic.contains("\"name\":\"task_create\""));
        assert!(anthropic.contains("\"name\":\"pr_attempt_preflight\""));
        assert!(anthropic.contains("\"name\":\"automation_read\""));
        assert!(anthropic.contains("\"name\":\"automation_update\""));
        assert!(anthropic.contains("\"name\":\"automation_run\""));
        assert!(anthropic.contains("\"input_schema\""));
    }

    #[test]
    fn build_tool_specs_include_agent_lifecycle_tools() {
        let names = [
            "agent_spawn".to_string(),
            "agent_result".to_string(),
            "agent_list".to_string(),
            "agent_cancel".to_string(),
            "close_agent".to_string(),
            "resume_agent".to_string(),
            "send_input".to_string(),
        ];
        let openai = build_openai_tools(&names);
        for name in [
            "agent_spawn",
            "agent_result",
            "agent_list",
            "agent_cancel",
            "close_agent",
            "resume_agent",
            "send_input",
        ] {
            assert!(openai.contains(&format!("\"name\":\"{name}\"")));
        }
        assert!(openai.contains("\"agent_id\""));
        assert!(openai.contains("\"fork_context\""));

        let anthropic = build_anthropic_tools(&names);
        assert!(anthropic.contains("\"name\":\"agent_spawn\""));
        assert!(anthropic.contains("\"name\":\"send_input\""));
        assert!(anthropic.contains("\"input_schema\""));
    }

    #[test]
    fn build_tool_specs_include_run_tests() {
        let openai = build_openai_tools(&["run_tests".to_string()]);
        assert!(openai.contains("\"name\":\"run_tests\""));
        assert!(openai.contains("\"all_features\""));
        assert!(openai.contains("\"command\""));

        let anthropic = build_anthropic_tools(&["run_tests".to_string()]);
        assert!(anthropic.contains("\"name\":\"run_tests\""));
        assert!(anthropic.contains("\"input_schema\""));
    }

    #[test]
    fn build_tool_specs_include_revert_turn() {
        let openai = build_openai_tools(&["revert_turn".to_string()]);
        assert!(openai.contains("\"name\":\"revert_turn\""));
        assert!(openai.contains("\"turn_offset\""));
        assert!(openai.contains("\"snapshot_id\""));

        let anthropic = build_anthropic_tools(&["revert_turn".to_string()]);
        assert!(anthropic.contains("\"name\":\"revert_turn\""));
        assert!(anthropic.contains("\"input_schema\""));
    }

    #[test]
    fn build_tool_specs_include_exec_shell_background_tools() {
        let openai = build_openai_tools(&[
            "exec_shell".to_string(),
            "task_shell_start".to_string(),
            "task_shell_wait".to_string(),
            "exec_shell_wait".to_string(),
            "exec_shell_replay".to_string(),
            "exec_shell_interact".to_string(),
            "exec_shell_cancel".to_string(),
            "exec_wait".to_string(),
            "exec_interact".to_string(),
        ]);
        assert!(openai.contains("\"name\":\"exec_shell\""));
        assert!(openai.contains("\"background\""));
        assert!(openai.contains("\"tty\""));
        assert!(openai.contains("\"tty_rows\""));
        assert!(openai.contains("\"tty_cols\""));
        assert!(openai.contains("script PTY backend"));
        assert!(openai.contains("\"name\":\"task_shell_start\""));
        assert!(openai.contains("\"name\":\"task_shell_wait\""));
        assert!(openai.contains("\"gate\""));
        assert!(openai.contains("\"name\":\"exec_shell_wait\""));
        assert!(openai.contains("\"name\":\"exec_shell_replay\""));
        assert!(openai.contains("\"limit_bytes\""));
        assert!(openai.contains("\"tail\""));
        assert!(openai.contains("\"name\":\"exec_shell_interact\""));
        assert!(openai.contains("\"name\":\"exec_shell_cancel\""));
        assert!(openai.contains("detached durable shell records"));

        let anthropic = build_anthropic_tools(&["exec_shell".to_string()]);
        assert!(anthropic.contains("\"name\":\"exec_shell\""));
        assert!(anthropic.contains("\"input_schema\""));
    }

    #[test]
    fn build_tool_specs_include_rlm() {
        let openai = build_openai_tools(&[
            "rlm".to_string(),
            "rlm_query".to_string(),
            "llm_query".to_string(),
            "rlm_process".to_string(),
            "rlm_chunk_plan".to_string(),
            "rlm_map_reduce_plan".to_string(),
            "rlm_recursive_plan".to_string(),
            "rlm_python".to_string(),
            "rlm_python_session".to_string(),
            "rlm_python_sessions".to_string(),
            "rlm_process_sessions".to_string(),
            "rlm_process_status".to_string(),
            "rlm_process_events".to_string(),
            "rlm_process_wait".to_string(),
            "rlm_process_cancel".to_string(),
            "rlm_process_recover".to_string(),
            "rlm_process_stop".to_string(),
            "rlm_process_run_next".to_string(),
            "rlm_process_drain".to_string(),
            "rlm_batch".to_string(),
            "rlm_query_batched".to_string(),
            "llm_query_batched".to_string(),
        ]);
        assert!(openai.contains("\"name\":\"rlm\""));
        assert!(openai.contains("\"name\":\"rlm_query\""));
        assert!(openai.contains("\"name\":\"llm_query\""));
        assert!(openai.contains("\"name\":\"rlm_process\""));
        assert!(openai.contains("\"name\":\"rlm_chunk_plan\""));
        assert!(openai.contains("\"name\":\"rlm_map_reduce_plan\""));
        assert!(openai.contains("\"name\":\"rlm_recursive_plan\""));
        assert!(openai.contains("\"name\":\"rlm_python\""));
        assert!(openai.contains("\"name\":\"rlm_python_session\""));
        assert!(openai.contains("\"name\":\"rlm_python_sessions\""));
        assert!(openai.contains("\"name\":\"rlm_process_sessions\""));
        assert!(openai.contains("\"name\":\"rlm_process_status\""));
        assert!(openai.contains("\"name\":\"rlm_process_events\""));
        assert!(openai.contains("\"name\":\"rlm_process_wait\""));
        assert!(openai.contains("\"name\":\"rlm_process_cancel\""));
        assert!(openai.contains("\"name\":\"rlm_process_recover\""));
        assert!(openai.contains("\"name\":\"rlm_process_stop\""));
        assert!(openai.contains("\"name\":\"rlm_process_run_next\""));
        assert!(openai.contains("\"name\":\"rlm_process_drain\""));
        assert!(openai.contains("\"name\":\"rlm_batch\""));
        assert!(openai.contains("\"name\":\"rlm_query_batched\""));
        assert!(openai.contains("\"name\":\"llm_query_batched\""));
        assert!(openai.contains("up to 16"));
        assert!(openai.contains("restricted Python"));
        assert!(openai.contains("\"context\""));
        assert!(openai.contains("\"question\""));
        assert!(openai.contains("\"file_path\""));
        assert!(openai.contains("\"max_depth\""));
        assert!(openai.contains("\"session_id\""));
        assert!(openai.contains("\"reset\""));
        assert!(openai.contains("\"include_live\""));
        assert!(openai.contains("\"include_turns\""));
        assert!(openai.contains("\"after_seq\""));
        assert!(openai.contains("\"timeout_ms\""));
        assert!(openai.contains("\"turn_id\""));
        assert!(openai.contains("\"dry_run\""));
        assert!(openai.contains("\"max_turns\""));
        assert!(openai.contains("\"mode\""));
        assert!(openai.contains("\"all\""));
        assert!(openai.contains("enqueue a live RLM daemon turn"));
        assert!(openai.contains("active running live RLM daemon turns"));
        assert!(openai.contains("interrupted live RLM daemon turns"));
        assert!(openai.contains("session_stopped"));
        assert!(openai.contains("persisted payload"));
        assert!(openai.contains("\"overlap\""));
        assert!(openai.contains("\"include_text\""));
        assert!(openai.contains("\"map_limit\""));
        assert!(openai.contains("\"fan_in\""));
        assert!(openai.contains("\"questions\""));

        let anthropic = build_anthropic_tools(&[
            "rlm".to_string(),
            "rlm_query".to_string(),
            "llm_query".to_string(),
            "rlm_process".to_string(),
            "rlm_chunk_plan".to_string(),
            "rlm_map_reduce_plan".to_string(),
            "rlm_recursive_plan".to_string(),
            "rlm_python".to_string(),
            "rlm_python_session".to_string(),
            "rlm_python_sessions".to_string(),
            "rlm_process_sessions".to_string(),
            "rlm_process_status".to_string(),
            "rlm_process_events".to_string(),
            "rlm_process_wait".to_string(),
            "rlm_process_cancel".to_string(),
            "rlm_process_recover".to_string(),
            "rlm_process_stop".to_string(),
            "rlm_process_run_next".to_string(),
            "rlm_process_drain".to_string(),
            "rlm_batch".to_string(),
            "rlm_query_batched".to_string(),
            "llm_query_batched".to_string(),
        ]);
        assert!(anthropic.contains("\"name\":\"rlm\""));
        assert!(anthropic.contains("\"name\":\"rlm_query\""));
        assert!(anthropic.contains("\"name\":\"llm_query\""));
        assert!(anthropic.contains("\"name\":\"rlm_process\""));
        assert!(anthropic.contains("\"name\":\"rlm_chunk_plan\""));
        assert!(anthropic.contains("\"name\":\"rlm_map_reduce_plan\""));
        assert!(anthropic.contains("\"name\":\"rlm_recursive_plan\""));
        assert!(anthropic.contains("\"name\":\"rlm_python\""));
        assert!(anthropic.contains("\"name\":\"rlm_python_session\""));
        assert!(anthropic.contains("\"name\":\"rlm_python_sessions\""));
        assert!(anthropic.contains("\"name\":\"rlm_process_sessions\""));
        assert!(anthropic.contains("\"name\":\"rlm_process_status\""));
        assert!(anthropic.contains("\"name\":\"rlm_process_events\""));
        assert!(anthropic.contains("\"name\":\"rlm_process_wait\""));
        assert!(anthropic.contains("\"name\":\"rlm_process_cancel\""));
        assert!(anthropic.contains("\"name\":\"rlm_process_recover\""));
        assert!(anthropic.contains("\"name\":\"rlm_process_stop\""));
        assert!(anthropic.contains("\"name\":\"rlm_process_run_next\""));
        assert!(anthropic.contains("\"name\":\"rlm_process_drain\""));
        assert!(anthropic.contains("\"name\":\"rlm_batch\""));
        assert!(anthropic.contains("\"name\":\"rlm_query_batched\""));
        assert!(anthropic.contains("\"name\":\"llm_query_batched\""));
        assert!(anthropic.contains("\"input_schema\""));
    }

    #[test]
    fn build_tools_include_dispatch_subagents() {
        let openai = build_openai_tools(&["dispatch_subagents".to_string()]);
        assert!(openai.contains("\"name\":\"dispatch_subagents\""));
        assert!(openai.contains("\"tasks\""));
        assert!(openai.contains("Maximum 4 child tasks"));

        let anthropic = build_anthropic_tools(&["dispatch_subagents".to_string()]);
        assert!(anthropic.contains("\"name\":\"dispatch_subagents\""));
        assert!(anthropic.contains("\"tasks\""));
    }

    #[test]
    fn build_tool_specs_include_mcp_bridge_tools() {
        let openai = build_openai_tools(&[
            "mcp_list_tools".to_string(),
            "mcp_call".to_string(),
            "mcp_list_prompts".to_string(),
            "mcp_get_prompt".to_string(),
            "mcp_list_resources".to_string(),
            "mcp_read_resource".to_string(),
            "mcp_list_resource_templates".to_string(),
        ]);
        assert!(openai.contains("\"name\":\"mcp_list_tools\""));
        assert!(openai.contains("\"name\":\"mcp_call\""));
        assert!(openai.contains("\"name\":\"mcp_list_prompts\""));
        assert!(openai.contains("\"name\":\"mcp_get_prompt\""));
        assert!(openai.contains("\"name\":\"mcp_list_resources\""));
        assert!(openai.contains("\"name\":\"mcp_read_resource\""));
        assert!(openai.contains("\"name\":\"mcp_list_resource_templates\""));
        assert!(openai.contains("\"server\""));
        assert!(openai.contains("\"arguments\""));
        assert!(openai.contains("\"prompt\""));
        assert!(openai.contains("\"uri\""));
        assert!(openai.contains("stdio, HTTP, or SSE MCP servers"));

        let anthropic = build_anthropic_tools(&[
            "mcp_list_tools".to_string(),
            "mcp_call".to_string(),
            "mcp_list_prompts".to_string(),
            "mcp_get_prompt".to_string(),
            "mcp_list_resources".to_string(),
            "mcp_read_resource".to_string(),
            "mcp_list_resource_templates".to_string(),
        ]);
        assert!(anthropic.contains("\"name\":\"mcp_list_tools\""));
        assert!(anthropic.contains("\"name\":\"mcp_call\""));
        assert!(anthropic.contains("\"name\":\"mcp_list_prompts\""));
        assert!(anthropic.contains("\"name\":\"mcp_get_prompt\""));
        assert!(anthropic.contains("\"name\":\"mcp_list_resources\""));
        assert!(anthropic.contains("\"name\":\"mcp_read_resource\""));
        assert!(anthropic.contains("\"name\":\"mcp_list_resource_templates\""));
        assert!(anthropic.contains("\"input_schema\""));
        assert!(anthropic.contains("stdio, HTTP, or SSE MCP server tool"));
    }

    #[test]
    fn build_tool_specs_include_dynamic_mcp_tools() {
        let tools = build_openai_tools(&["mcp__fallback__echo".to_string()]);
        assert!(tools.contains("\"name\":\"mcp__fallback__echo\""));
        assert!(tools.contains("\"arguments\""));
        assert!(tools.contains("Call the configured MCP remote tool"));
    }

    #[test]
    fn build_tool_specs_use_cached_dynamic_mcp_schema() {
        crate::tools::mcp::cache_dynamic_tool_schema(
            "mcp__schema__read_file",
            Some("Read a file through MCP".to_string()),
            Some(
                r#"{"type":"object","properties":{"path":{"type":"string","description":"File path"}},"required":["path"]}"#
                    .to_string(),
            ),
        );

        let tools = build_openai_tools(&["mcp__schema__read_file".to_string()]);

        assert!(tools.contains("\"name\":\"mcp__schema__read_file\""));
        assert!(tools.contains("Read a file through MCP"));
        assert!(tools.contains("\"path\""));
        assert!(tools.contains("\"required\":[\"path\"]"));
        assert!(!tools.contains("\"arguments\""));
    }

    fn empty_request_with_todos(todos: Vec<crate::core::todos::Todo>) -> ModelRequest {
        ModelRequest {
            system_prompt: String::new(),
            task: "test".to_string(),
            image_inputs: Vec::new(),
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

    fn one_pixel_png_input() -> ImageInput {
        ImageInput {
            path: "screenshot.png".to_string(),
            media_type: "image/png".to_string(),
            data_base64: "iVBORw0KGgo=".to_string(),
        }
    }

    fn openai_vision_config() -> ModelConfig {
        ModelConfig {
            base_url: "https://api.openai.com/v1".to_string(),
            model: "gpt-5.3-codex".to_string(),
            api_key_env: "OPENAI_API_KEY".to_string(),
            reasoning_effort: "off".to_string(),
        }
    }

    fn anthropic_vision_config() -> ModelConfig {
        ModelConfig {
            base_url: "https://api.anthropic.com/v1/anthropic".to_string(),
            model: "claude-sonnet-4-5".to_string(),
            api_key_env: "ANTHROPIC_API_KEY".to_string(),
            reasoning_effort: "off".to_string(),
        }
    }

    #[test]
    fn openai_user_message_uses_native_image_parts_for_supported_models() {
        let mut request = empty_request_with_todos(Vec::new());
        request.image_inputs = vec![one_pixel_png_input()];

        let message =
            super::build_openai_user_message(&request, "Inspect this", &openai_vision_config())
                .unwrap();

        assert!(message.contains(r#""role":"user""#));
        assert!(message.contains(r#""type":"text""#));
        assert!(message.contains(r#""type":"image_url""#));
        assert!(message.contains("data:image/png;base64,iVBORw0KGgo="));
    }

    #[test]
    fn openai_user_message_keeps_text_for_non_vision_profile() {
        let mut request = empty_request_with_todos(Vec::new());
        request.image_inputs = vec![one_pixel_png_input()];
        let config = ModelConfig::default();

        let message = super::build_openai_user_message(&request, "Inspect this", &config).unwrap();

        assert_eq!(message, r#"{"role":"user","content":"Inspect this"}"#);
    }

    #[test]
    fn anthropic_user_content_uses_native_image_blocks_for_supported_models() {
        let mut request = empty_request_with_todos(Vec::new());
        request.image_inputs = vec![one_pixel_png_input()];

        let content = super::build_anthropic_user_content(
            &request,
            "Inspect this",
            &anthropic_vision_config(),
        )
        .unwrap();

        assert!(content.contains(r#""type":"image""#));
        assert!(content.contains(r#""media_type":"image/png""#));
        assert!(content.contains(r#""data":"iVBORw0KGgo=""#));
        assert!(content.contains(r#""type":"text""#));
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
    fn reasoning_tier_maps_off_high_and_max_to_api_fields() {
        let off = super::ReasoningTier::from_config("off");
        assert!(!off.thinking_enabled());
        assert!(off.openai_fields().contains("\"disabled\""));
        assert_eq!(off.anthropic_fields(), "");

        let high = super::ReasoningTier::from_config("high");
        assert!(high.thinking_enabled());
        assert!(high
            .openai_fields()
            .contains("\"reasoning_effort\":\"high\""));
        assert!(high.anthropic_fields().contains("\"effort\":\"high\""));

        let max = super::ReasoningTier::from_config("xhigh");
        assert!(max.thinking_enabled());
        assert!(max.openai_fields().contains("\"reasoning_effort\":\"max\""));
        assert!(max.anthropic_fields().contains("\"effort\":\"max\""));
    }

    #[test]
    fn auto_route_uses_flash_without_reasoning_for_simple_tasks() {
        let config = ModelConfig {
            base_url: "https://api.deepseek.com".to_string(),
            model: "auto".to_string(),
            api_key_env: "DSCODE_TEST_NO_KEY".to_string(),
            reasoning_effort: "auto".to_string(),
        };
        let mut request = empty_request_with_todos(Vec::new());
        request.task = "quick read-only lookup".to_string();

        let route = super::ModelRoute::resolve(&config, &request);

        assert_eq!(route.model, "deepseek-v4-flash");
        assert_eq!(route.reasoning, super::ReasoningTier::Off);
    }

    #[test]
    fn auto_route_uses_pro_and_max_reasoning_for_deep_tasks() {
        let config = ModelConfig {
            base_url: "https://api.deepseek.com".to_string(),
            model: "deepseek-auto".to_string(),
            api_key_env: "DSCODE_TEST_NO_KEY".to_string(),
            reasoning_effort: "auto".to_string(),
        };
        let mut request = empty_request_with_todos(Vec::new());
        request.task =
            "audit the architecture and close the DeepSeek-TUI parity gap with a multi-step plan"
                .to_string();
        request.planning_mode = true;

        let route = super::ModelRoute::resolve(&config, &request);

        assert_eq!(route.model, "deepseek-v4-pro");
        assert_eq!(route.reasoning, super::ReasoningTier::Max);
    }

    #[test]
    fn auto_route_respects_explicit_model_and_reasoning() {
        let config = ModelConfig {
            base_url: "https://api.deepseek.com".to_string(),
            model: "deepseek-chat".to_string(),
            api_key_env: "DSCODE_TEST_NO_KEY".to_string(),
            reasoning_effort: "off".to_string(),
        };
        let mut request = empty_request_with_todos(Vec::new());
        request.task = "complex architecture refactor with failed diagnostics".to_string();
        request.observations = vec![Observation::failed("diagnostics", "type errors")];

        let route = super::ModelRoute::resolve(&config, &request);

        assert_eq!(route.model, "deepseek-chat");
        assert_eq!(route.reasoning, super::ReasoningTier::Off);
    }

    #[test]
    fn remote_usage_records_resolved_model() {
        let parsed = Ok((
            crate::model::protocol::ModelResponse {
                message: "ok".to_string(),
                action: ModelAction::Finish,
            },
            Some(crate::model::protocol::TokenUsage::new(5, 2)),
        ));

        let (_, usage) = super::attach_usage_model(parsed, "deepseek-v4-flash").unwrap();

        assert_eq!(usage.unwrap().model.as_deref(), Some("deepseek-v4-flash"));
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
                reasoning_effort: "off".to_string(),
            },
        }
    }

    #[test]
    fn offline_planner_routes_recent_commits_to_git_log() {
        let mut request = empty_request_with_todos(Vec::new());
        request.task = "show recent git commits".to_string();
        request.available_tools = vec!["git_log".to_string(), "list_files".to_string()];

        let response = planner()
            .respond(request, &mut crate::ui::stream::NoopStreamEvents)
            .unwrap()
            .0;
        match response.action {
            ModelAction::CallTool { tool_name, input } => {
                assert_eq!(tool_name, "git_log");
                assert_eq!(input.get("limit"), Some("10"));
            }
            ModelAction::Finish => panic!("expected git_log"),
        }
    }

    #[test]
    fn offline_planner_routes_git_blame_with_path_and_line() {
        let mut request = empty_request_with_todos(Vec::new());
        request.task = "git blame src/tools/registry.rs line 42".to_string();
        request.available_tools = vec!["git_blame".to_string(), "list_files".to_string()];

        let response = planner()
            .respond(request, &mut crate::ui::stream::NoopStreamEvents)
            .unwrap()
            .0;
        match response.action {
            ModelAction::CallTool { tool_name, input } => {
                assert_eq!(tool_name, "git_blame");
                assert_eq!(input.get("path"), Some("src/tools/registry.rs"));
                assert_eq!(input.get("line_start"), Some("42"));
                assert_eq!(input.get("line_end"), Some("42"));
            }
            ModelAction::Finish => panic!("expected git_blame"),
        }
    }

    #[test]
    fn offline_planner_finishes_after_git_history_tool() {
        let mut request = empty_request_with_todos(Vec::new());
        request.task = "show recent git commits".to_string();
        request.available_tools = vec!["git_log".to_string(), "list_files".to_string()];
        request.observations = vec![Observation::ok(
            "git_log",
            "meta.git_command=log\nmeta.result=ok\nabc123 initial commit",
        )];

        let response = planner()
            .respond(request, &mut crate::ui::stream::NoopStreamEvents)
            .unwrap()
            .0;
        assert!(matches!(response.action, ModelAction::Finish));
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
            image_inputs: Vec::new(),
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
            image_inputs: Vec::new(),
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
            image_inputs: Vec::new(),
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
            image_inputs: Vec::new(),
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
            image_inputs: Vec::new(),
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
            image_inputs: Vec::new(),
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
            image_inputs: Vec::new(),
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
    fn derive_github_pr_context_request_reads_number_repo_and_urls() {
        assert_eq!(
            derive_github_pr_context_request(
                "Review pull request #42 'Route benchmark command' on owner/repo."
            ),
            Some(GithubPrContextRequest {
                number: "42".to_string(),
                repo: Some("owner/repo".to_string()),
            })
        );
        assert_eq!(
            derive_github_pr_context_request(
                "Review https://github.com/acme/widgets/pull/77 before merge."
            ),
            Some(GithubPrContextRequest {
                number: "77".to_string(),
                repo: Some("acme/widgets".to_string()),
            })
        );
    }

    #[test]
    fn offline_planner_fetches_remote_pr_context_before_review() {
        let request = ModelRequest {
            system_prompt: String::new(),
            task: "Review pull request #42 'Route benchmark command' on owner/repo. Highlight correctness risks.".to_string(),
            image_inputs: Vec::new(),
            profile_name: "rust".to_string(),
            profile_hints: Vec::new(),
            primary_file: None,
            suggested_test_command: None,
            available_tools: vec![
                "github_pr_context".to_string(),
                "review".to_string(),
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
            ModelAction::CallTool { tool_name, input } => {
                assert_eq!(tool_name, "github_pr_context");
                assert_eq!(input.get("number"), Some("42"));
                assert_eq!(input.get("repo"), Some("owner/repo"));
                assert_eq!(input.get("include_diff"), Some("true"));
            }
            ModelAction::Finish => panic!("expected github_pr_context tool call"),
        }
    }

    #[test]
    fn offline_planner_fetches_remote_pr_context_for_comment_draft_task() {
        let request = ModelRequest {
            system_prompt: String::new(),
            task: "Draft a PR review comment for PR #42 on owner/repo.".to_string(),
            image_inputs: Vec::new(),
            profile_name: "rust".to_string(),
            profile_hints: Vec::new(),
            primary_file: None,
            suggested_test_command: None,
            available_tools: vec![
                "github_pr_context".to_string(),
                "review".to_string(),
                "pr_review_comment_plan".to_string(),
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
            ModelAction::CallTool { tool_name, input } => {
                assert_eq!(tool_name, "github_pr_context");
                assert_eq!(input.get("number"), Some("42"));
                assert_eq!(input.get("repo"), Some("owner/repo"));
                assert_eq!(input.get("include_diff"), Some("true"));
            }
            ModelAction::Finish => panic!("expected github_pr_context tool call"),
        }
    }

    #[test]
    fn offline_planner_reviews_gathered_remote_pr_context() {
        let context = "meta.kind=pr\n\
meta.number=42\n\
PR #42: Route benchmark command\n\
json:\n\
{\"number\":42,\"title\":\"Route benchmark command\",\"reviewDecision\":\"CHANGES_REQUESTED\"}\n\
diff:\n\
diff --git a/src/cli/app.rs b/src/cli/app.rs\n\
--- a/src/cli/app.rs\n\
+++ b/src/cli/app.rs\n\
@@ -1,2 +1,3 @@\n\
+pub fn route_benchmark_subcommand() {}\n";
        let request = ModelRequest {
            system_prompt: String::new(),
            task: "Review pull request #42 'Route benchmark command' on owner/repo. Highlight correctness risks.".to_string(),
            image_inputs: Vec::new(),
            profile_name: "rust".to_string(),
            profile_hints: Vec::new(),
            primary_file: None,
            suggested_test_command: None,
            available_tools: vec![
                "github_pr_context".to_string(),
                "review".to_string(),
                "read_file".to_string(),
            ],
            observations: vec![Observation::ok("github_pr_context", context)],
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
                assert_eq!(tool_name, "review");
                assert_eq!(input.get("target"), Some("github_pr_context"));
                assert_eq!(input.get("github_context"), Some(context));
            }
            ModelAction::Finish => panic!("expected review tool call"),
        }
    }

    #[test]
    fn offline_planner_enables_semantic_review_for_remote_pr_when_requested() {
        let context = "meta.kind=pr\n\
meta.number=42\n\
PR #42: Route benchmark command\n\
json:\n\
{\"number\":42,\"title\":\"Route benchmark command\",\"reviewDecision\":\"CHANGES_REQUESTED\"}\n\
diff:\n\
diff --git a/src/cli/app.rs b/src/cli/app.rs\n";
        let request = ModelRequest {
            system_prompt: String::new(),
            task:
                "Run a semantic review of pull request #42 on owner/repo for real behavioral bugs."
                    .to_string(),
            image_inputs: Vec::new(),
            profile_name: "rust".to_string(),
            profile_hints: Vec::new(),
            primary_file: None,
            suggested_test_command: None,
            available_tools: vec![
                "github_pr_context".to_string(),
                "review".to_string(),
                "read_file".to_string(),
            ],
            observations: vec![Observation::ok("github_pr_context", context)],
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
                assert_eq!(tool_name, "review");
                assert_eq!(input.get("target"), Some("github_pr_context"));
                assert_eq!(input.get("github_context"), Some(context));
                assert_eq!(input.get("semantic"), Some("true"));
            }
            ModelAction::Finish => panic!("expected semantic review tool call"),
        }
    }

    #[test]
    fn offline_planner_finishes_after_remote_pr_review_tool() {
        let request = ModelRequest {
            system_prompt: String::new(),
            task: "Review pull request #42 'Route benchmark command' on owner/repo. Highlight correctness risks.".to_string(),
            image_inputs: Vec::new(),
            profile_name: "rust".to_string(),
            profile_hints: Vec::new(),
            primary_file: None,
            suggested_test_command: None,
            available_tools: vec![
                "github_pr_context".to_string(),
                "review".to_string(),
                "read_file".to_string(),
            ],
            observations: vec![
                Observation::ok("github_pr_context", "meta.kind=pr\nmeta.number=42\n"),
                Observation::ok(
                    "review",
                    "{\"issues\":[{\"title\":\"GitHub PR has requested changes\"}]}",
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
        assert!(matches!(response.action, ModelAction::Finish));
    }

    #[test]
    fn offline_planner_drafts_remote_pr_review_comment_plan() {
        let context = "meta.kind=pr\n\
meta.number=42\n\
PR #42: Route benchmark command\n\
json:\n\
{\"number\":42,\"title\":\"Route benchmark command\",\"url\":\"https://github.com/owner/repo/pull/42\"}\n\
diff:\n\
diff --git a/src/cli/app.rs b/src/cli/app.rs\n";
        let review_output =
            "{\"issues\":[{\"severity\":\"warning\",\"title\":\"source change without test change\"}],\"source\":{\"kind\":\"github_pr_diff\",\"target\":\"github_pr_context\",\"truncated\":false}}";
        let request = ModelRequest {
            system_prompt: String::new(),
            task: "Review pull request #42 'Route benchmark command' on owner/repo and draft a PR comment with the findings.".to_string(),
            image_inputs: Vec::new(),
            profile_name: "rust".to_string(),
            profile_hints: Vec::new(),
            primary_file: None,
            suggested_test_command: None,
            available_tools: vec![
                "github_pr_context".to_string(),
                "review".to_string(),
                "pr_review_comment_plan".to_string(),
            ],
            observations: vec![
                Observation::ok("github_pr_context", context),
                Observation::ok("review", review_output),
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
                assert_eq!(tool_name, "pr_review_comment_plan");
                assert_eq!(input.get("review_output"), Some(review_output));
                assert_eq!(input.get("pr_context"), Some(context));
                assert_eq!(input.get("number"), Some("42"));
                assert_eq!(input.get("repo"), Some("owner/repo"));
            }
            ModelAction::Finish => panic!("expected pr_review_comment_plan tool call"),
        }
    }

    #[test]
    fn offline_planner_posts_prepared_remote_pr_comment_when_explicit() {
        let comment_plan = r###"{"comment_body":"## Automated PR Review\n\nFound 1 deterministic issue(s).","evidence":{"tool":"review","issue_count":1},"github_comment_input":{"target":"pr","number":"42","body":"## Automated PR Review\n\nFound 1 deterministic issue(s).","evidence":"{\"tool\":\"review\",\"issue_count\":1}","dry_run":"true","repo":"owner/repo"}}"###;
        let request = ModelRequest {
            system_prompt: String::new(),
            task: "Review pull request #42 on owner/repo and post a PR comment with the findings."
                .to_string(),
            image_inputs: Vec::new(),
            profile_name: "rust".to_string(),
            profile_hints: Vec::new(),
            primary_file: None,
            suggested_test_command: None,
            available_tools: vec![
                "github_pr_context".to_string(),
                "review".to_string(),
                "pr_review_comment_plan".to_string(),
                "github_comment".to_string(),
            ],
            observations: vec![
                Observation::ok("github_pr_context", "meta.kind=pr\nmeta.number=42\n"),
                Observation::ok("review", "{\"issues\":[]}"),
                Observation::ok("pr_review_comment_plan", comment_plan),
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
                assert_eq!(tool_name, "github_comment");
                assert_eq!(input.get("target"), Some("pr"));
                assert_eq!(input.get("number"), Some("42"));
                assert_eq!(input.get("repo"), Some("owner/repo"));
                assert_eq!(input.get("dry_run"), Some("false"));
                assert!(input
                    .get("body")
                    .unwrap_or_default()
                    .contains("Automated PR Review"));
                assert_eq!(
                    input.get("evidence"),
                    Some("{\"tool\":\"review\",\"issue_count\":1}")
                );
            }
            ModelAction::Finish => panic!("expected guarded github_comment tool call"),
        }
    }

    #[test]
    fn offline_planner_posts_prepared_inline_pr_review_comments_when_explicit() {
        let comment_plan = r###"{"evidence":{"tool":"review","issue_count":1},"github_comment_input":{"target":"pr","number":"42","body":"draft","evidence":"{\"tool\":\"review\",\"issue_count\":1}","dry_run":"true","repo":"owner/repo"},"github_pr_review_comment_input":{"number":"42","commit_id":"abc123","comments":[{"path":"src/lib.rs","line":12,"body":"warning: check this","side":"RIGHT"}],"evidence":"{\"tool\":\"review\",\"issue_count\":1}","dry_run":"true","repo":"owner/repo"}}"###;
        let request = ModelRequest {
            system_prompt: String::new(),
            task: "Review pull request #42 on owner/repo and post inline review comments with the findings."
                .to_string(),
            image_inputs: Vec::new(),
            profile_name: "rust".to_string(),
            profile_hints: Vec::new(),
            primary_file: None,
            suggested_test_command: None,
            available_tools: vec![
                "github_pr_context".to_string(),
                "review".to_string(),
                "pr_review_comment_plan".to_string(),
                "github_comment".to_string(),
                "github_pr_review_comment".to_string(),
            ],
            observations: vec![
                Observation::ok("github_pr_context", "meta.kind=pr\nmeta.number=42\n"),
                Observation::ok("review", "{\"issues\":[]}"),
                Observation::ok("pr_review_comment_plan", comment_plan),
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
                assert_eq!(tool_name, "github_pr_review_comment");
                assert_eq!(input.get("number"), Some("42"));
                assert_eq!(input.get("repo"), Some("owner/repo"));
                assert_eq!(input.get("commit_id"), Some("abc123"));
                assert_eq!(input.get("dry_run"), Some("false"));
                assert!(input
                    .get("comments")
                    .unwrap_or_default()
                    .contains("src/lib.rs"));
                assert_eq!(
                    input.get("evidence"),
                    Some("{\"tool\":\"review\",\"issue_count\":1}")
                );
            }
            ModelAction::Finish => panic!("expected guarded github_pr_review_comment tool call"),
        }
    }

    #[test]
    fn offline_planner_replans_after_failed_inline_pr_review_comment_post() {
        let context = "meta.kind=pr\nmeta.number=42\n";
        let review_output = "{\"issues\":[{\"severity\":\"info\",\"title\":\"public API change\",\"path\":\"src/lib.rs\",\"line\":12}],\"source\":{\"kind\":\"github_pr_diff\",\"target\":\"github_pr_context\",\"truncated\":false}}";
        let comment_plan = r###"{"evidence":{"tool":"review","issue_count":1},"github_pr_review_comment_input":{"number":"42","commit_id":"abc123","comments":[{"path":"src/lib.rs","line":12,"body":"warning: check this"}],"evidence":"{\"tool\":\"review\",\"issue_count\":1}","dry_run":"true","repo":"owner/repo"}}"###;
        let request = ModelRequest {
            system_prompt: String::new(),
            task: "Review pull request #42 on owner/repo and post inline review comments with the findings."
                .to_string(),
            image_inputs: Vec::new(),
            profile_name: "rust".to_string(),
            profile_hints: Vec::new(),
            primary_file: None,
            suggested_test_command: None,
            available_tools: vec![
                "github_pr_context".to_string(),
                "review".to_string(),
                "pr_review_comment_plan".to_string(),
                "github_pr_review_comment".to_string(),
            ],
            observations: vec![
                Observation::ok("github_pr_context", context),
                Observation::ok("review", review_output),
                Observation::ok("pr_review_comment_plan", comment_plan),
                Observation::failed("github_pr_review_comment", "line is not part of the diff"),
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
                assert_eq!(tool_name, "pr_review_comment_plan");
                assert_eq!(input.get("review_output"), Some(review_output));
                assert_eq!(
                    input.get("comment_error"),
                    Some("line is not part of the diff")
                );
            }
            ModelAction::Finish => panic!("expected pr_review_comment_plan retry tool call"),
        }
    }

    #[test]
    fn offline_planner_does_not_post_draft_remote_pr_comment_plan() {
        let comment_plan = r###"{"comment_body":"draft","evidence":{"tool":"review"},"github_comment_input":{"target":"pr","number":"42","body":"draft","evidence":"{\"tool\":\"review\"}","dry_run":"true","repo":"owner/repo"}}"###;
        let request = ModelRequest {
            system_prompt: String::new(),
            task: "Review pull request #42 on owner/repo and draft a PR comment with the findings."
                .to_string(),
            image_inputs: Vec::new(),
            profile_name: "rust".to_string(),
            profile_hints: Vec::new(),
            primary_file: None,
            suggested_test_command: None,
            available_tools: vec![
                "github_pr_context".to_string(),
                "review".to_string(),
                "pr_review_comment_plan".to_string(),
                "github_comment".to_string(),
            ],
            observations: vec![
                Observation::ok("github_pr_context", "meta.kind=pr\nmeta.number=42\n"),
                Observation::ok("review", "{\"issues\":[]}"),
                Observation::ok("pr_review_comment_plan", comment_plan),
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
    fn offline_planner_replans_after_failed_pr_comment_post() {
        let context = "meta.kind=pr\nmeta.number=42\n";
        let review_output = "{\"issues\":[],\"source\":{\"kind\":\"github_pr_diff\",\"target\":\"github_pr_context\",\"truncated\":false}}";
        let comment_plan = r###"{"comment_body":"draft","evidence":{"tool":"review"},"github_comment_input":{"target":"pr","number":"42","body":"draft","evidence":"{\"tool\":\"review\"}","dry_run":"true","repo":"owner/repo"}}"###;
        let request = ModelRequest {
            system_prompt: String::new(),
            task: "Review pull request #42 on owner/repo and post a PR comment with the findings."
                .to_string(),
            image_inputs: Vec::new(),
            profile_name: "rust".to_string(),
            profile_hints: Vec::new(),
            primary_file: None,
            suggested_test_command: None,
            available_tools: vec![
                "github_pr_context".to_string(),
                "review".to_string(),
                "pr_review_comment_plan".to_string(),
                "github_comment".to_string(),
            ],
            observations: vec![
                Observation::ok("github_pr_context", context),
                Observation::ok("review", review_output),
                Observation::ok("pr_review_comment_plan", comment_plan),
                Observation::failed("github_comment", "policy denied by reviewer"),
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
                assert_eq!(tool_name, "pr_review_comment_plan");
                assert_eq!(input.get("review_output"), Some(review_output));
                assert_eq!(input.get("pr_context"), Some(context));
                assert_eq!(
                    input.get("comment_error"),
                    Some("policy denied by reviewer")
                );
                assert_eq!(input.get("number"), Some("42"));
                assert_eq!(input.get("repo"), Some("owner/repo"));
            }
            ModelAction::Finish => panic!("expected comment-plan retry after failed post"),
        }
    }

    #[test]
    fn offline_planner_finishes_after_pr_comment_retry_plan() {
        let request = ModelRequest {
            system_prompt: String::new(),
            task: "Review pull request #42 on owner/repo and post a PR comment with the findings."
                .to_string(),
            image_inputs: Vec::new(),
            profile_name: "rust".to_string(),
            profile_hints: Vec::new(),
            primary_file: None,
            suggested_test_command: None,
            available_tools: vec![
                "github_pr_context".to_string(),
                "review".to_string(),
                "pr_review_comment_plan".to_string(),
                "github_comment".to_string(),
            ],
            observations: vec![
                Observation::ok("github_pr_context", "meta.kind=pr\nmeta.number=42\n"),
                Observation::ok("review", "{\"issues\":[]}"),
                Observation::ok("pr_review_comment_plan", "{\"comment_body\":\"first\",\"evidence\":{\"tool\":\"review\"},\"number\":\"42\"}"),
                Observation::failed("github_comment", "gh auth failed"),
                Observation::ok("pr_review_comment_plan", "{\"comment_body\":\"retry\",\"evidence\":{\"tool\":\"review\",\"previous_comment_error\":\"gh auth failed\"},\"number\":\"42\"}"),
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
    fn offline_planner_creates_initial_todo_plan_in_planning_mode() {
        let request = ModelRequest {
            system_prompt: String::new(),
            task: "implement the feature and verify the tests still pass".to_string(),
            image_inputs: Vec::new(),
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
            image_inputs: Vec::new(),
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
            image_inputs: Vec::new(),
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
            image_inputs: Vec::new(),
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
            image_inputs: Vec::new(),
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
            image_inputs: Vec::new(),
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
            image_inputs: Vec::new(),
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
            image_inputs: Vec::new(),
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
            image_inputs: Vec::new(),
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
            image_inputs: Vec::new(),
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
            image_inputs: Vec::new(),
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
            image_inputs: Vec::new(),
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
            image_inputs: Vec::new(),
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
            image_inputs: Vec::new(),
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
            image_inputs: Vec::new(),
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
            image_inputs: Vec::new(),
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
            image_inputs: Vec::new(),
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
            image_inputs: Vec::new(),
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
            image_inputs: Vec::new(),
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
            image_inputs: Vec::new(),
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
            image_inputs: Vec::new(),
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
            image_inputs: Vec::new(),
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
            image_inputs: Vec::new(),
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
            image_inputs: Vec::new(),
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
            image_inputs: Vec::new(),
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
            image_inputs: Vec::new(),
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
            image_inputs: Vec::new(),
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
            image_inputs: Vec::new(),
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
            image_inputs: Vec::new(),
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
            image_inputs: Vec::new(),
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
            image_inputs: Vec::new(),
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
            image_inputs: Vec::new(),
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
            image_inputs: Vec::new(),
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
    fn build_initial_todo_plan_specializes_product_readiness_requests() {
        let request = ModelRequest {
            system_prompt: String::new(),
            task: "productionize DeepSeekCode for daily coding work".to_string(),
            image_inputs: Vec::new(),
            profile_name: "rust".to_string(),
            profile_hints: Vec::new(),
            primary_file: None,
            suggested_test_command: None,
            available_tools: vec![
                "todo_write".to_string(),
                "search_text".to_string(),
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

        let plan = super::build_initial_todo_plan_json(&request);
        assert!(
            plan.contains("Assess current capability gaps against the target product behavior"),
            "plan: {plan}"
        );
        assert!(
            plan.contains("Implement the smallest high-impact product gap closure slice"),
            "plan: {plan}"
        );
        assert!(
            plan.contains("Validate the product gap slice with tests or benchmark"),
            "plan: {plan}"
        );
    }

    #[test]
    fn offline_planner_replans_after_replan_hint() {
        let request = ModelRequest {
            system_prompt: String::new(),
            task: "implement the fix and validate with cargo test".to_string(),
            image_inputs: Vec::new(),
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
            "usage": {
                "prompt_tokens": 12,
                "completion_tokens": 5,
                "prompt_cache_hit_tokens": 7,
                "prompt_cache_miss_tokens": 5
            }
        }"#;
        let usage = parse_openai_usage(body).unwrap();
        assert_eq!(usage.prompt, 12);
        assert_eq!(usage.completion, 5);
        assert_eq!(usage.prompt_cache_hit, 7);
        assert_eq!(usage.prompt_cache_miss, 5);
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
            "usage": {
                "input_tokens": 30,
                "output_tokens": 11,
                "cache_read_input_tokens": 9,
                "cache_creation_input_tokens": 21
            }
        }"#;
        let usage = parse_anthropic_usage(body).unwrap();
        assert_eq!(usage.prompt, 30);
        assert_eq!(usage.completion, 11);
        assert_eq!(usage.prompt_cache_hit, 9);
        assert_eq!(usage.prompt_cache_miss, 21);
    }

    use crate::error::AppResult;
    use crate::ui::stream::{NoopStreamEvents, StreamEvents};
    use crate::util::cancel::CancellationCheck;
    use std::cell::RefCell;
    use std::collections::BTreeMap;
    use std::io::Cursor;

    #[derive(Default)]
    struct CapturingEvents {
        reasoning: RefCell<Vec<String>>,
        chunks: RefCell<Vec<String>>,
        done: RefCell<Vec<String>>,
        tool_calls: RefCell<Vec<(String, BTreeMap<String, String>)>>,
    }

    impl StreamEvents for CapturingEvents {
        fn on_reasoning_delta(&mut self, chunk: &str) {
            self.reasoning.borrow_mut().push(chunk.to_string());
        }
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

    struct CancelAfterPoll {
        calls: usize,
        cancel_after: usize,
    }

    impl CancellationCheck for CancelAfterPoll {
        fn is_cancelled(&mut self) -> AppResult<bool> {
            self.calls += 1;
            Ok(self.calls >= self.cancel_after)
        }
    }

    #[test]
    fn parse_openai_stream_emits_text_deltas_and_finishes() {
        let body = concat!(
            "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"Thinking\"},\"finish_reason\":null}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"Hel\"},\"finish_reason\":null}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"lo\"},\"finish_reason\":null}]}\n\n",
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
            "data: {\"usage\":{\"prompt_tokens\":3,\"completion_tokens\":2,\"prompt_tokens_details\":{\"cached_tokens\":1}}}\n\n",
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
        assert_eq!(usage.prompt_cache_hit, 1);
        assert_eq!(usage.prompt_cache_miss, 2);
        let chunks = events.chunks.borrow();
        assert_eq!(*chunks, vec!["Hel".to_string(), "lo".to_string()]);
        let reasoning = events.reasoning.borrow();
        assert_eq!(*reasoning, vec!["Thinking".to_string()]);
        assert_eq!(events.done.borrow().len(), 1);
        assert!(events.tool_calls.borrow().is_empty());
    }

    #[test]
    fn parse_openai_stream_with_cancel_stops_between_frames() {
        let body = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"Hel\"},\"finish_reason\":null}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"lo\"},\"finish_reason\":null}]}\n\n",
            "data: [DONE]\n\n",
        );
        let mut cur = Cursor::new(body.as_bytes().to_vec());
        let mut events = CapturingEvents::default();
        let mut cancel = CancelAfterPoll {
            calls: 0,
            cancel_after: 3,
        };
        let error =
            super::parse_openai_stream_with_cancel(&mut cur, &mut events, Some(&mut cancel))
                .unwrap_err();

        assert!(error.to_string().contains("agent run cancelled"));
        assert_eq!(*events.chunks.borrow(), vec!["Hel".to_string()]);
        assert_eq!(*events.done.borrow(), vec!["Hel".to_string()]);
    }

    #[cfg(unix)]
    #[test]
    fn parse_openai_process_stream_with_cancel_stops_blocked_read() {
        let mut process = crate::util::process::spawn_streaming(
            "sh",
            &["-c", "sleep 5; printf 'data: [DONE]\\n\\n'"],
        )
        .unwrap();
        let mut events = CapturingEvents::default();
        let mut cancel = CancelAfterPoll {
            calls: 0,
            cancel_after: 2,
        };
        let started = std::time::Instant::now();
        let error =
            super::parse_openai_process_stream(&mut process, &mut events, Some(&mut cancel))
                .unwrap_err();

        assert!(error.to_string().contains("agent run cancelled"));
        assert!(
            started.elapsed() < std::time::Duration::from_secs(1),
            "cancel should not wait for the blocked producer"
        );
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
            "event: message_start\ndata: {\"message\":{\"usage\":{\"input_tokens\":10,\"output_tokens\":0,\"cache_read_input_tokens\":4}}}\n\n",
            "event: content_block_start\ndata: {\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
            "event: content_block_delta\ndata: {\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"Reasoning\"}}\n\n",
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
        assert_eq!(usage.prompt_cache_hit, 4);
        assert_eq!(usage.prompt_cache_miss, 6);
        let chunks = events.chunks.borrow();
        assert_eq!(*chunks, vec!["hi ".to_string(), "there".to_string()]);
        let reasoning = events.reasoning.borrow();
        assert_eq!(*reasoning, vec!["Reasoning".to_string()]);
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
            image_inputs: Vec::new(),
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
