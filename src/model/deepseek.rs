use std::collections::BTreeMap;
use std::env;
use std::io::BufRead;

use crate::config::types::ModelConfig;
use crate::error::AppResult;
use crate::error::app_error;
use crate::error::tool_failure;
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
        let endpoint = format!("{}/chat/completions", self.config.base_url.trim_end_matches('/'));
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
        let mut succeeded_tools: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
        for observation in &input.observations {
            used_tools.insert(observation.tool_name.as_str());
            if !observation.is_failure() {
                succeeded_tools.insert(observation.tool_name.as_str());
            }
        }
        let available_tools: std::collections::BTreeSet<&str> = input
            .available_tools
            .iter()
            .map(String::as_str)
            .collect();
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
        let edit_request = derive_edit_request(&task);

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

        if edit_request.is_none() {
            if let Some(query) = search_query {
                if !used_tools.contains("search_text") && tool_available("search_text") {
                    return ModelResponse {
                        message: format!("{} planner is searching for `{query}`.", self.config.model),
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
            let apply_patch_available =
                tool_available("apply_patch");
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
                        message: format!("{} planner is reading the primary file.", self.config.model),
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

        if succeeded_tools.contains("apply_patch")
            && !used_tools.contains("git_diff")
            && tool_available("git_diff")
        {
            return ModelResponse {
                message: format!("{} planner is reviewing the resulting diff.", self.config.model),
                action: ModelAction::CallTool {
                    tool_name: "git_diff".to_string(),
                    input: ToolInput::new(),
                },
            };
        }

        if let Some(test_command) = input.suggested_test_command.as_deref() {
            if wants_validation(&task_lower)
                && !used_tools.contains("run_shell")
                && tool_available("run_shell")
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
            let prompt_preview = input
                .system_prompt
                .lines()
                .next()
                .unwrap_or("")
                .trim();
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
    prompt.push_str(&format!("Available tools: {}\n", input.available_tools.join(", ")));
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

    while let Some(frame) = read_frame(reader).map_err(|e| app_error(format!("sse read failed: {e}")))? {
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
                usage = Some(TokenUsage { prompt: p, completion: c });
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
                        let Some(call_obj) = json_as_object(call) else { continue };
                        // OpenAI streams tool calls indexed by `index`. We support exactly one
                        // tool call per turn; any other distinct index is an error.
                        let observed_index = call_obj.get("index").and_then(json_as_u64);
                        match (tool_assembly.as_mut(), observed_index) {
                            (Some(existing), Some(idx)) if existing.index != idx => {
                                return Err(tool_failure(format!(
                                    "openai stream emitted multiple parallel tool calls (indices {} and {}); only one is supported per turn",
                                    existing.index, idx
                                )));
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

    while let Some(frame) = read_frame(reader).map_err(|e| app_error(format!("sse read failed: {e}")))? {
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
                        let block_index = root
                            .get("index")
                            .and_then(json_as_u64)
                            .unwrap_or(0);
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
                            let id = block
                                .get("id")
                                .and_then(json_as_string)
                                .map(str::to_string);
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
                            if let Some(partial) = delta.get("partial_json").and_then(json_as_string) {
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
        (Some(p), Some(c)) => Some(TokenUsage { prompt: p, completion: c }),
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
                result.insert(key.clone(), if *value { "true" } else { "false" }.to_string());
            }
            JsonValue::Null => {
                result.insert(key.clone(), "null".to_string());
            }
            JsonValue::Object(_) | JsonValue::Array(_) => {
                return Err(app_error(format!(
                    "tool argument `{key}` must be a scalar json value"
                )));
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

fn first_quoted_segment(task: &str) -> Option<String> {
    quoted_segments(task).into_iter().next()
}

fn wants_validation(task: &str) -> bool {
    ["test", "fix", "validate", "check", "lint"].iter().any(|word| task.contains(word))
}

#[derive(Debug, Clone)]
struct EditRequest {
    path: String,
    find: String,
    replace: String,
}

fn derive_edit_request(task: &str) -> Option<EditRequest> {
    let task_lower = task.to_lowercase();
    if !task_lower.contains("replace ") || !task_lower.contains(" with ") || !task_lower.contains(" in ") {
        return None;
    }

    let quoted = quoted_segments(task);
    if quoted.len() < 2 {
        return None;
    }

    let in_index = task_lower.rfind(" in ")?;
    let path = task[in_index + 4..].trim().trim_matches('`').trim().to_string();
    if path.is_empty() {
        return None;
    }

    Some(EditRequest {
        path,
        find: quoted[0].clone(),
        replace: quoted[1].clone(),
    })
}

fn quoted_segments(task: &str) -> Vec<String> {
    let bytes = task.as_bytes();
    let mut start = None;
    let mut values = Vec::new();

    for (index, byte) in bytes.iter().enumerate() {
        if *byte == b'"' {
            if let Some(begin) = start {
                let segment = task[begin + 1..index].trim();
                if !segment.is_empty() {
                    values.push(segment.to_string());
                }
                start = None;
            } else {
                start = Some(index);
            }
        }
    }

    values
}

#[cfg(test)]
mod tests {
    use super::{
        api_flavor, build_anthropic_tools, build_openai_tools, parse_anthropic_messages,
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
            available_tools: vec!["apply_patch".to_string(), "read_file".to_string(), "list_files".to_string()],
            observations: vec![
                Observation::ok("list_files", "noop"),
                Observation::ok("read_file", "noop"),
            ],
        };

        let response = planner().respond(request, &mut crate::ui::stream::NoopStreamEvents).unwrap().0;
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
            available_tools: vec!["apply_patch".to_string(), "read_file".to_string(), "list_files".to_string()],
            observations: vec![],
        };

        let response = planner().respond(request, &mut crate::ui::stream::NoopStreamEvents).unwrap().0;
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
            available_tools: vec!["apply_patch".to_string(), "read_file".to_string(), "list_files".to_string()],
            observations: vec![
                Observation::ok("list_files", "noop"),
                Observation::ok("read_file", "noop"),
            ],
        };

        let response = planner().respond(request, &mut crate::ui::stream::NoopStreamEvents).unwrap().0;
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
        };

        let response = planner().respond(request, &mut crate::ui::stream::NoopStreamEvents).unwrap().0;
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
                Observation::failed(
                    "apply_patch",
                    "apply_patch requires a path",
                ),
            ],
        };

        let response = planner().respond(request, &mut crate::ui::stream::NoopStreamEvents).unwrap().0;
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
        assert_eq!(events.done.borrow().len(), 1, "on_assistant_done not called on error");
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
    fn parse_openai_stream_errors_on_parallel_tool_calls() {
        // Two tool calls at distinct indices in the same stream — must error loudly.
        let body = concat!(
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"a\",\"type\":\"function\",\"function\":{\"name\":\"read_file\",\"arguments\":\"{\\\"path\\\":\\\"a.rs\\\"}\"}}]},\"finish_reason\":null}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":1,\"id\":\"b\",\"type\":\"function\",\"function\":{\"name\":\"git_diff\",\"arguments\":\"{}\"}}]},\"finish_reason\":null}]}\n\n",
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n",
            "data: [DONE]\n\n",
        );
        let mut cur = Cursor::new(body.as_bytes().to_vec());
        let mut events = NoopStreamEvents;
        let result = super::parse_openai_stream(&mut cur, &mut events);
        let err = result.unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("multiple parallel tool calls"),
            "unexpected error: {msg}"
        );
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
            err.to_string().contains("multiple parallel tool_use blocks"),
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
}
