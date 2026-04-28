use std::collections::BTreeMap;
use std::env;
use std::process::Command;

use crate::config::types::ModelConfig;
use crate::error::AppResult;
use crate::error::app_error;
use crate::model::client::ModelClient;
use crate::model::protocol::{ModelAction, ModelRequest, ModelResponse};
use crate::tools::types::ToolInput;
use crate::util::json::{
    json_as_array, json_as_object, json_as_string, parse_root_object, JsonValue,
};

pub struct DeepSeekClient {
    pub config: ModelConfig,
}

impl ModelClient for DeepSeekClient {
    fn respond(&self, input: ModelRequest) -> AppResult<ModelResponse> {
        if let Ok(api_key) = env::var(&self.config.api_key_env) {
            if !api_key.trim().is_empty() {
                if let Ok(response) = self.respond_remote(&input, &api_key) {
                    return Ok(response);
                }
            }
        }

        Ok(self.respond_offline(input))
    }
}

impl DeepSeekClient {
    fn respond_remote(&self, input: &ModelRequest, api_key: &str) -> AppResult<ModelResponse> {
        match api_flavor(&self.config.base_url) {
            ApiFlavor::OpenAi => self.respond_remote_openai(input, api_key),
            ApiFlavor::Anthropic => self.respond_remote_anthropic(input, api_key),
        }
    }

    fn respond_remote_openai(&self, input: &ModelRequest, api_key: &str) -> AppResult<ModelResponse> {
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

        let output = Command::new("curl")
            .args([
                "-sS",
                "-X",
                "POST",
                &endpoint,
                "-H",
                &format!("Authorization: Bearer {api_key}"),
                "-H",
                "Content-Type: application/json",
                "--data-binary",
                &body,
            ])
            .output()?;

        if !output.status.success() {
            return Err(app_error(format!(
                "deepseek openai request failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }

        let body = String::from_utf8_lossy(&output.stdout);
        parse_openai_chat_completion(&body)
    }

    fn respond_remote_anthropic(&self, input: &ModelRequest, api_key: &str) -> AppResult<ModelResponse> {
        let endpoint = format!("{}/messages", self.config.base_url.trim_end_matches('/'));
        let system_prompt = build_anthropic_tool_system_prompt(&input.system_prompt);
        let user_prompt = build_user_prompt(input);
        let tools = build_anthropic_tools(&input.available_tools);
        let body = format!(
            concat!(
                "{{",
                "\"model\":\"{}\",",
                "\"max_tokens\":1024,",
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

        let output = Command::new("curl")
            .args([
                "-sS",
                "-X",
                "POST",
                &endpoint,
                "-H",
                &format!("x-api-key: {api_key}"),
                "-H",
                "anthropic-version: 2023-06-01",
                "-H",
                "Content-Type: application/json",
                "--data-binary",
                &body,
            ])
            .output()?;

        if !output.status.success() {
            return Err(app_error(format!(
                "deepseek anthropic request failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }

        let body = String::from_utf8_lossy(&output.stdout);
        parse_anthropic_messages(&body)
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

        if !used_tools.contains("list_files") && tool_available("list_files") {
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
            if !used_tools.contains("read_file") && tool_available("read_file") {
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

fn json_escape(value: &str) -> String {
    let mut output = String::new();
    for ch in value.chars() {
        match ch {
            '"' => output.push_str("\\\""),
            '\\' => output.push_str("\\\\"),
            '\n' => output.push_str("\\n"),
            '\r' => output.push_str("\\r"),
            '\t' => output.push_str("\\t"),
            _ => output.push(ch),
        }
    }
    output
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
        parse_openai_chat_completion, ApiFlavor, DeepSeekClient,
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
            primary_file: None,
            suggested_test_command: None,
            available_tools: vec!["apply_patch".to_string(), "read_file".to_string(), "list_files".to_string()],
            observations: vec![
                Observation::ok("list_files", "noop"),
                Observation::ok("read_file", "noop"),
            ],
        };

        let response = planner().respond(request).unwrap();
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
            primary_file: None,
            suggested_test_command: None,
            available_tools: vec!["apply_patch".to_string(), "read_file".to_string(), "list_files".to_string()],
            observations: vec![
                Observation::ok("list_files", "noop"),
                Observation::ok("read_file", "noop"),
            ],
        };

        let response = planner().respond(request).unwrap();
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

        let response = planner().respond(request).unwrap();
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

        let response = planner().respond(request).unwrap();
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
}
