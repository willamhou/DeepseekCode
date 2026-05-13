use std::collections::BTreeMap;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crate::cli::app::{ServeAcpArgs, ServeAction, ServeArgs, ServeHttpArgs, ServeMcpArgs};
use crate::config::load::load_or_default;
use crate::config::types::{AppConfig, ApprovalConfig, DiagnosticsConfig};
use crate::core::rollback::{snapshot_to_json, RestorePlan, RollbackStore};
use crate::core::runtime::{
    automation_to_json, event_to_json, item_to_json, json_array, json_object, json_string_field,
    parse_json_object_body, session_to_json, task_to_json, thread_compaction_to_json,
    thread_to_json, turn_to_json, usage_to_json, validate_record_id, RuntimeEvent, RuntimeStore,
    TaskRecord,
};
use crate::error::{app_error, AppResult};
use crate::model::client::ModelClient;
use crate::model::deepseek::DeepSeekClient;
use crate::model::protocol::{ModelRequest, TokenUsage};
use crate::tools::apply_patch::ApplyPatchTool;
use crate::tools::diagnostics::DiagnosticsTool;
use crate::tools::document::{ImageOcrTool, PandocConvertTool};
use crate::tools::exec_shell::{
    ExecShellCancelTool, ExecShellInteractTool, ExecShellListTool, ExecShellShowTool,
    ExecShellTool, ExecShellWaitTool, TaskShellStartTool, TaskShellWaitTool,
};
use crate::tools::file_search::FileSearchTool;
use crate::tools::file_write::EditFileTool;
use crate::tools::git_diff::{GitDiffTool, GitStatusTool};
use crate::tools::git_history::{GitBlameTool, GitLogTool, GitShowTool};
use crate::tools::github::{
    GithubCloseIssueTool, GithubCommentTool, GithubIssueContextTool, GithubPrContextTool,
};
use crate::tools::list_files::{ListDirTool, ListFilesTool};
use crate::tools::notes::{NoteTool, RememberTool};
use crate::tools::notify::NotifyTool;
use crate::tools::project_map::ProjectMapTool;
use crate::tools::read_file::ReadFileTool;
use crate::tools::recall_archive::RecallArchiveTool;
use crate::tools::revert_turn::RevertTurnTool;
use crate::tools::review::{PrReviewCommentPlanTool, ReviewTool};
use crate::tools::rlm::{
    RlmBatchTool, RlmChunkPlanTool, RlmMapReducePlanTool, RlmPythonSessionTool,
    RlmPythonSessionsTool, RlmPythonTool, RlmRecursivePlanTool, RlmTool,
};
use crate::tools::run_shell::{is_safe_shell_command, RunShellTool};
use crate::tools::run_tests::{render_run_tests_command, RunTestsTool};
use crate::tools::search_text::{GrepFilesTool, SearchTextTool};
use crate::tools::skill::LoadSkillTool;
use crate::tools::tool_output::RetrieveToolResultTool;
use crate::tools::tool_search::{ToolSearchMode, ToolSearchTool};
use crate::tools::types::{Tool, ToolInput};
use crate::tools::user_input::RequestUserInputTool;
use crate::tools::validate_data::ValidateDataTool;
use crate::tools::vision::ImageAnalyzeTool;
use crate::tools::web::{FetchUrlTool, FinanceTool, WebRunTool, WebSearchTool};
use crate::ui::stream::NoopStreamEvents;
use crate::util::cwd::CwdGuard;
use crate::util::json::{
    json_as_array, json_as_object, json_as_string, json_as_u64, json_value_to_string,
    parse_root_object, JsonValue,
};

const MCP_PROTOCOL_VERSION: &str = "2025-11-25";
const ACP_PROTOCOL_VERSION: u64 = 1;
const ACP_TOOL_PROGRESS_MIN_CHARS: usize = 4096;
const ACP_TOOL_PROGRESS_CHUNK_CHARS: usize = 2048;
const ACP_TOOL_PROGRESS_MAX_CHUNKS: usize = 4;
static ACP_TOOL_CALL_COUNTER: AtomicU64 = AtomicU64::new(0);

fn env_flag(name: &str) -> bool {
    matches!(
        std::env::var(name).ok().as_deref(),
        Some("1") | Some("true") | Some("TRUE")
    )
}

pub fn run(args: ServeArgs) -> AppResult<()> {
    match args.action {
        ServeAction::Http(http) => run_http(http),
        ServeAction::Mcp(mcp) => run_mcp_stdio(mcp),
        ServeAction::Acp(acp) => run_acp_stdio(acp),
    }
}

fn run_http(args: ServeHttpArgs) -> AppResult<()> {
    let config = load_or_default()?;
    let store = RuntimeStore::new(PathBuf::from(&config.workspace.config_dir).join("runtime"));
    let listener = TcpListener::bind(&args.addr).map_err(|error| {
        app_error(format!(
            "failed to bind HTTP runtime at {}: {error}",
            args.addr
        ))
    })?;
    let addr = listener.local_addr()?;
    println!("DeepSeekCode HTTP runtime listening on http://{addr}");
    println!("  health: http://{addr}/health");
    println!("  runtime: http://{addr}/runtime");
    println!("  threads: http://{addr}/v1/threads");
    serve_http_listener(listener, args.once, &store)
}

#[derive(Clone)]
struct McpStdioState {
    store: RuntimeStore,
    rollback: RollbackStore,
    config: AppConfig,
    workspace: PathBuf,
    approval: ApprovalConfig,
    diagnostics: DiagnosticsConfig,
    approval_thread_id: Option<String>,
    approval_turn_id: Option<String>,
    approval_poll_interval: Duration,
    approval_max_polls: Option<usize>,
    allow_side_effect_tools: bool,
}

fn run_mcp_stdio(args: ServeMcpArgs) -> AppResult<()> {
    let _cwd_guard = match args.workspace {
        Some(workspace) => Some(CwdGuard::enter(Path::new(&workspace))?),
        None => None,
    };
    let config = load_or_default()?;
    let workspace = std::env::current_dir()?;
    let store = RuntimeStore::new(PathBuf::from(&config.workspace.config_dir).join("runtime"));
    let rollback = RollbackStore::new(PathBuf::from(&config.workspace.config_dir).join("rollback"));
    let approval_thread_id = mcp_approval_thread_from_env(&store, &workspace)?;
    let state = McpStdioState {
        store,
        rollback,
        config: config.clone(),
        workspace,
        approval: config.approval.clone(),
        diagnostics: config.diagnostics.clone(),
        approval_thread_id,
        approval_turn_id: None,
        approval_poll_interval: Duration::from_millis(250),
        approval_max_polls: None,
        allow_side_effect_tools: env_flag("DSCODE_MCP_ENABLE_SIDE_EFFECTS"),
    };
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();

    for line in BufReader::new(stdin.lock()).lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        if let Some(response) = mcp_response_for_message(&line, &state) {
            stdout.write_all(json_value_to_string(&response).as_bytes())?;
            stdout.write_all(b"\n")?;
            stdout.flush()?;
        }
    }

    Ok(())
}

fn mcp_approval_thread_from_env(
    store: &RuntimeStore,
    workspace: &Path,
) -> AppResult<Option<String>> {
    if let Ok(thread_id) = std::env::var("DSCODE_MCP_APPROVAL_THREAD_ID") {
        let thread_id = thread_id.trim();
        if !thread_id.is_empty() {
            store.load_thread(thread_id)?;
            return Ok(Some(thread_id.to_string()));
        }
    }

    if !env_flag("DSCODE_MCP_ENABLE_DURABLE_APPROVALS") {
        return Ok(None);
    }

    let workspace = workspace.display().to_string();
    let session = store.create_session("MCP approvals".to_string(), workspace.clone())?;
    let thread = store.create_thread_for_session(
        &session.id,
        "MCP side-effect approvals".to_string(),
        workspace,
        "deepseek-coder".to_string(),
        "mcp".to_string(),
    )?;
    eprintln!(
        "DeepSeekCode MCP durable approvals enabled on runtime thread {}",
        thread.id
    );
    Ok(Some(thread.id))
}

fn mcp_response_for_message(message: &str, state: &McpStdioState) -> Option<JsonValue> {
    let root = match parse_root_object(message) {
        Ok(root) => root,
        Err(error) => {
            return Some(mcp_error_response(
                JsonValue::Null,
                -32700,
                "Parse error",
                &error.to_string(),
            ))
        }
    };
    let id = root.get("id").cloned();
    let Some(method) = root.get("method").and_then(json_as_string) else {
        return Some(mcp_error_response(
            id.unwrap_or(JsonValue::Null),
            -32600,
            "Invalid Request",
            "request field `method` must be a string",
        ));
    };
    if id.is_none() && method.starts_with("notifications/") {
        return None;
    }
    let response_id = id.unwrap_or(JsonValue::Null);

    match method {
        "initialize" => Some(mcp_success_response(response_id, mcp_initialize_result())),
        "tools/list" => Some(mcp_success_response(
            response_id,
            object([("tools", JsonValue::Array(mcp_tool_definitions(state)))]),
        )),
        "tools/call" => Some(mcp_tools_call_response(response_id, &root, state)),
        "prompts/list" => Some(mcp_success_response(
            response_id,
            object([
                ("prompts", JsonValue::Array(mcp_prompt_definitions())),
                ("nextCursor", JsonValue::Null),
            ]),
        )),
        "prompts/get" => Some(mcp_prompts_get_response(response_id, &root)),
        "resources/list" => Some(mcp_success_response(
            response_id,
            object([
                (
                    "resources",
                    JsonValue::Array(mcp_resource_definitions(state)),
                ),
                ("nextCursor", JsonValue::Null),
            ]),
        )),
        "resources/templates/list" => Some(mcp_success_response(
            response_id,
            object([
                (
                    "resourceTemplates",
                    JsonValue::Array(mcp_resource_template_definitions()),
                ),
                ("nextCursor", JsonValue::Null),
            ]),
        )),
        "resources/read" => Some(mcp_resources_read_response(response_id, &root, state)),
        "notifications/initialized" => None,
        other => Some(mcp_error_response(
            response_id,
            -32601,
            "Method not found",
            &format!("unsupported MCP method `{other}`"),
        )),
    }
}

fn mcp_initialize_result() -> JsonValue {
    object([
        (
            "protocolVersion",
            JsonValue::String(MCP_PROTOCOL_VERSION.to_string()),
        ),
        (
            "capabilities",
            object([
                ("tools", JsonValue::Object(BTreeMap::new())),
                ("prompts", JsonValue::Object(BTreeMap::new())),
                ("resources", JsonValue::Object(BTreeMap::new())),
            ]),
        ),
        (
            "serverInfo",
            object([
                ("name", JsonValue::String("DeepSeekCode".to_string())),
                (
                    "version",
                    JsonValue::String(env!("CARGO_PKG_VERSION").to_string()),
                ),
            ]),
        ),
    ])
}

fn mcp_tools_call_response(
    response_id: JsonValue,
    root: &BTreeMap<String, JsonValue>,
    state: &McpStdioState,
) -> JsonValue {
    let params = match root.get("params").and_then(json_as_object) {
        Some(params) => params,
        None => {
            return mcp_error_response(
                response_id,
                -32602,
                "Invalid params",
                "tools/call requires object params",
            )
        }
    };
    let Some(name) = params.get("name").and_then(json_as_string) else {
        return mcp_error_response(
            response_id,
            -32602,
            "Invalid params",
            "tools/call requires string params.name",
        );
    };
    let arguments = match params.get("arguments") {
        Some(value) => match json_as_object(value) {
            Some(arguments) => arguments,
            None => {
                return mcp_error_response(
                    response_id,
                    -32602,
                    "Invalid params",
                    "tools/call params.arguments must be an object",
                )
            }
        },
        None => {
            static EMPTY: std::sync::OnceLock<BTreeMap<String, JsonValue>> =
                std::sync::OnceLock::new();
            EMPTY.get_or_init(BTreeMap::new)
        }
    };

    let result = execute_mcp_tool(name, arguments, state);
    match result {
        Ok(text) => mcp_success_response(response_id, mcp_tool_text_result(text, false)),
        Err(error) => {
            mcp_success_response(response_id, mcp_tool_text_result(error.to_string(), true))
        }
    }
}

fn mcp_prompts_get_response(
    response_id: JsonValue,
    root: &BTreeMap<String, JsonValue>,
) -> JsonValue {
    let params = match root.get("params").and_then(json_as_object) {
        Some(params) => params,
        None => {
            return mcp_error_response(
                response_id,
                -32602,
                "Invalid params",
                "prompts/get requires object params",
            )
        }
    };
    let Some(name) = params.get("name").and_then(json_as_string) else {
        return mcp_error_response(
            response_id,
            -32602,
            "Invalid params",
            "prompts/get requires string params.name",
        );
    };
    let arguments = match params.get("arguments") {
        Some(value) => match json_as_object(value) {
            Some(arguments) => arguments,
            None => {
                return mcp_error_response(
                    response_id,
                    -32602,
                    "Invalid params",
                    "prompts/get params.arguments must be an object",
                )
            }
        },
        None => {
            static EMPTY: std::sync::OnceLock<BTreeMap<String, JsonValue>> =
                std::sync::OnceLock::new();
            EMPTY.get_or_init(BTreeMap::new)
        }
    };

    match mcp_prompt_result(name, arguments) {
        Ok(result) => mcp_success_response(response_id, result),
        Err(error) => mcp_error_response(response_id, -32602, "Invalid params", &error.to_string()),
    }
}

fn mcp_resources_read_response(
    response_id: JsonValue,
    root: &BTreeMap<String, JsonValue>,
    state: &McpStdioState,
) -> JsonValue {
    let params = match root.get("params").and_then(json_as_object) {
        Some(params) => params,
        None => {
            return mcp_error_response(
                response_id,
                -32602,
                "Invalid params",
                "resources/read requires object params",
            )
        }
    };
    let Some(uri) = params.get("uri").and_then(json_as_string) else {
        return mcp_error_response(
            response_id,
            -32602,
            "Invalid params",
            "resources/read requires string params.uri",
        );
    };

    match mcp_read_resource(uri, state) {
        Ok(content) => mcp_success_response(
            response_id,
            object([("contents", JsonValue::Array(vec![content]))]),
        ),
        Err(error) => mcp_error_response(response_id, -32602, "Invalid params", &error.to_string()),
    }
}

fn mcp_resource_definitions(state: &McpStdioState) -> Vec<JsonValue> {
    let mut resources = vec![mcp_resource_definition(
        &workspace_resource_uri(&state.workspace),
        "workspace",
        "Workspace root",
        "inode/directory",
    )];

    if let Ok(sessions) = state.store.list_sessions(50) {
        for session in sessions {
            resources.push(mcp_resource_definition(
                &format!("deepseekcode://runtime/sessions/{}", session.id),
                &session.title,
                &format!(
                    "{} session, {} thread(s)",
                    session.status, session.thread_count
                ),
                "application/json",
            ));
        }
    }
    if let Ok(threads) = state.store.list_threads(50) {
        for thread in threads {
            resources.push(mcp_resource_definition(
                &format!("deepseekcode://runtime/threads/{}", thread.id),
                &thread.title,
                &format!(
                    "{} thread, model {}, mode {}",
                    thread.status, thread.model, thread.mode
                ),
                "application/json",
            ));
        }
    }
    if let Ok(tasks) = state.store.list_tasks(None, None, 50) {
        for task in tasks {
            resources.push(mcp_resource_definition(
                &format!("deepseekcode://runtime/tasks/{}", task.id),
                &task.summary,
                &format!("{} {} task", task.status, task.kind),
                "application/json",
            ));
        }
    }

    resources
}

fn mcp_prompt_definitions() -> Vec<JsonValue> {
    vec![
        mcp_prompt_definition(
            "review_code",
            "Review a file or code area for correctness, maintainability, and test gaps.",
            vec![
                mcp_prompt_argument("path", "File path or code area to review.", true),
                mcp_prompt_argument(
                    "focus",
                    "Optional review focus, such as bugs, tests, or performance.",
                    false,
                ),
            ],
        ),
        mcp_prompt_definition(
            "explain_code",
            "Explain how a file, module, or symbol works.",
            vec![
                mcp_prompt_argument("path", "File path or module to explain.", true),
                mcp_prompt_argument(
                    "symbol",
                    "Optional symbol, function, or type name to focus on.",
                    false,
                ),
            ],
        ),
        mcp_prompt_definition(
            "plan_task",
            "Create an implementation plan for a coding task in the current workspace.",
            vec![
                mcp_prompt_argument("task", "Task or feature to plan.", true),
                mcp_prompt_argument(
                    "constraints",
                    "Optional constraints, risks, or verification requirements.",
                    false,
                ),
            ],
        ),
    ]
}

fn mcp_prompt_definition(name: &str, description: &str, arguments: Vec<JsonValue>) -> JsonValue {
    object([
        ("name", JsonValue::String(name.to_string())),
        ("description", JsonValue::String(description.to_string())),
        ("arguments", JsonValue::Array(arguments)),
    ])
}

fn mcp_prompt_argument(name: &str, description: &str, required: bool) -> JsonValue {
    object([
        ("name", JsonValue::String(name.to_string())),
        ("description", JsonValue::String(description.to_string())),
        ("required", JsonValue::Bool(required)),
    ])
}

fn mcp_prompt_result(name: &str, arguments: &BTreeMap<String, JsonValue>) -> AppResult<JsonValue> {
    match name {
        "review_code" => {
            let path = prompt_argument(arguments, "path")?;
            let focus = optional_prompt_argument(arguments, "focus")
                .map(|value| format!("\nFocus: {value}"))
                .unwrap_or_default();
            Ok(mcp_prompt_messages(
                "Review a file or code area for correctness, maintainability, and test gaps.",
                format!(
                    "Review `{path}` in this workspace. Identify concrete bugs, behavioral regressions, missing tests, and maintainability risks. Ground findings in file paths and line references where possible.{focus}"
                ),
            ))
        }
        "explain_code" => {
            let path = prompt_argument(arguments, "path")?;
            let symbol = optional_prompt_argument(arguments, "symbol")
                .map(|value| format!(" Focus on `{value}`."))
                .unwrap_or_default();
            Ok(mcp_prompt_messages(
                "Explain how a file, module, or symbol works.",
                format!(
                    "Explain how `{path}` works in this workspace.{symbol} Cover the main responsibilities, important data flow, and any non-obvious edge cases."
                ),
            ))
        }
        "plan_task" => {
            let task = prompt_argument(arguments, "task")?;
            let constraints = optional_prompt_argument(arguments, "constraints")
                .map(|value| format!("\nConstraints: {value}"))
                .unwrap_or_default();
            Ok(mcp_prompt_messages(
                "Create an implementation plan for a coding task in the current workspace.",
                format!(
                    "Plan this coding task for the current workspace:\n\n{task}\n\nInclude concrete files or modules to inspect, implementation steps, test strategy, and residual risks.{constraints}"
                ),
            ))
        }
        other => Err(app_error(format!("unknown MCP prompt `{other}`"))),
    }
}

fn mcp_prompt_messages(description: &str, text: String) -> JsonValue {
    object([
        ("description", JsonValue::String(description.to_string())),
        (
            "messages",
            JsonValue::Array(vec![object([
                ("role", JsonValue::String("user".to_string())),
                (
                    "content",
                    object([
                        ("type", JsonValue::String("text".to_string())),
                        ("text", JsonValue::String(text)),
                    ]),
                ),
            ])]),
        ),
    ])
}

fn prompt_argument<'a>(
    arguments: &'a BTreeMap<String, JsonValue>,
    key: &str,
) -> AppResult<&'a str> {
    arguments
        .get(key)
        .and_then(json_as_string)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| app_error(format!("MCP prompt requires string argument `{key}`")))
}

fn optional_prompt_argument<'a>(
    arguments: &'a BTreeMap<String, JsonValue>,
    key: &str,
) -> Option<&'a str> {
    arguments
        .get(key)
        .and_then(json_as_string)
        .filter(|value| !value.trim().is_empty())
}

fn mcp_resource_definition(uri: &str, name: &str, description: &str, mime_type: &str) -> JsonValue {
    object([
        ("uri", JsonValue::String(uri.to_string())),
        ("name", JsonValue::String(name.to_string())),
        ("description", JsonValue::String(description.to_string())),
        ("mimeType", JsonValue::String(mime_type.to_string())),
    ])
}

fn mcp_resource_template_definitions() -> Vec<JsonValue> {
    vec![
        mcp_resource_template_definition(
            "deepseekcode://runtime/sessions/{id}",
            "runtime-session",
            "Durable runtime session JSON by session id",
            "application/json",
        ),
        mcp_resource_template_definition(
            "deepseekcode://runtime/threads/{id}",
            "runtime-thread",
            "Durable runtime thread JSON with turns and items by thread id",
            "application/json",
        ),
        mcp_resource_template_definition(
            "deepseekcode://runtime/tasks/{id}",
            "runtime-task",
            "Durable runtime task JSON by task id",
            "application/json",
        ),
    ]
}

fn mcp_resource_template_definition(
    uri_template: &str,
    name: &str,
    description: &str,
    mime_type: &str,
) -> JsonValue {
    object([
        ("uriTemplate", JsonValue::String(uri_template.to_string())),
        ("name", JsonValue::String(name.to_string())),
        ("description", JsonValue::String(description.to_string())),
        ("mimeType", JsonValue::String(mime_type.to_string())),
    ])
}

fn mcp_read_resource(uri: &str, state: &McpStdioState) -> AppResult<JsonValue> {
    if uri == workspace_resource_uri(&state.workspace) {
        return Ok(mcp_resource_text_content(
            uri,
            "application/json",
            json_value_to_string(&object([(
                "workspace",
                JsonValue::String(state.workspace.display().to_string()),
            )])),
        ));
    }
    if let Some(id) = uri.strip_prefix("deepseekcode://runtime/sessions/") {
        let session = state.store.load_session(id)?;
        return Ok(mcp_resource_text_content(
            uri,
            "application/json",
            json_value_to_string(&session_to_json(&session)),
        ));
    }
    if let Some(id) = uri.strip_prefix("deepseekcode://runtime/threads/") {
        let thread = state.store.load_thread(id)?;
        let turns = state
            .store
            .list_turns(id)?
            .iter()
            .map(turn_to_json)
            .collect::<Vec<_>>();
        let items = state
            .store
            .list_items(id, None)?
            .iter()
            .map(item_to_json)
            .collect::<Vec<_>>();
        return Ok(mcp_resource_text_content(
            uri,
            "application/json",
            json_value_to_string(&object([
                ("thread", thread_to_json(&thread)),
                ("turns", JsonValue::Array(turns)),
                ("items", JsonValue::Array(items)),
            ])),
        ));
    }
    if let Some(id) = uri.strip_prefix("deepseekcode://runtime/tasks/") {
        let task = state.store.load_task(id)?;
        return Ok(mcp_resource_text_content(
            uri,
            "application/json",
            json_value_to_string(&task_to_json(&task)),
        ));
    }
    Err(app_error(format!("unknown MCP resource uri: {uri}")))
}

fn workspace_resource_uri(workspace: &Path) -> String {
    format!("file://{}", workspace.display())
}

fn mcp_resource_text_content(uri: &str, mime_type: &str, text: String) -> JsonValue {
    object([
        ("uri", JsonValue::String(uri.to_string())),
        ("mimeType", JsonValue::String(mime_type.to_string())),
        ("text", JsonValue::String(text)),
    ])
}

fn execute_mcp_tool(
    name: &str,
    arguments: &BTreeMap<String, JsonValue>,
    state: &McpStdioState,
) -> AppResult<String> {
    let input = mcp_input_with_workspace_defaults(name, tool_input_from_json(arguments), state);
    let output = match name {
        "list_files" => ListFilesTool.execute(input)?,
        "list_dir" => ListDirTool.execute(input)?,
        "read_file" => ReadFileTool.execute(input)?,
        "retrieve_tool_result" => RetrieveToolResultTool.execute(input)?,
        "search_text" => SearchTextTool.execute(input)?,
        "grep_files" => GrepFilesTool.execute(input)?,
        "file_search" => FileSearchTool.execute(input)?,
        "web_search" => WebSearchTool.execute(input)?,
        "web_run" => WebRunTool.execute(input)?,
        "fetch_url" => FetchUrlTool.execute(input)?,
        "finance" => FinanceTool.execute(input)?,
        "pandoc_convert" => {
            return execute_mcp_pandoc_convert(input, state);
        }
        "image_ocr" => ImageOcrTool.execute(input)?,
        "image_analyze" => {
            return execute_mcp_image_analyze(input, state);
        }
        "git_status" => GitStatusTool.execute(input)?,
        "git_diff" => GitDiffTool.execute(input)?,
        "project_map" => ProjectMapTool.execute(input)?,
        "validate_data" => ValidateDataTool.execute(input)?,
        "git_log" => GitLogTool.execute(input)?,
        "git_show" => GitShowTool.execute(input)?,
        "git_blame" => GitBlameTool.execute(input)?,
        "github_issue_context" => GithubIssueContextTool.execute(input)?,
        "github_pr_context" => GithubPrContextTool.execute(input)?,
        "review" => ReviewTool::default().execute(input)?,
        "pr_review_comment_plan" => PrReviewCommentPlanTool.execute(input)?,
        "recall_archive" => RecallArchiveTool::new(&state.config).execute(input)?,
        "tool_search_tool_regex" => ToolSearchTool {
            tool_name: "tool_search_tool_regex",
            mode: ToolSearchMode::Regex,
        }
        .execute(input)?,
        "tool_search_tool_bm25" => ToolSearchTool {
            tool_name: "tool_search_tool_bm25",
            mode: ToolSearchMode::Bm25,
        }
        .execute(input)?,
        "load_skill" => LoadSkillTool::new(state.config.clone()).execute(input)?,
        "request_user_input" => RequestUserInputTool.execute(input)?,
        "notify" => NotifyTool.execute(input)?,
        "note" => {
            if !mcp_write_tools_enabled(state) {
                return Err(app_error(
                    "MCP write tool `note` is disabled; set DSCODE_MCP_ENABLE_DURABLE_APPROVALS=1 or DSCODE_MCP_APPROVAL_THREAD_ID=<thread-id> to route note writes through runtime approvals",
                ));
            }
            return execute_mcp_note(input, state);
        }
        "remember" => {
            if !mcp_write_tools_enabled(state) || !state.config.memory.enabled {
                return Err(app_error(
                    "MCP write tool `remember` is disabled; enable memory and set DSCODE_MCP_ENABLE_DURABLE_APPROVALS=1 or DSCODE_MCP_APPROVAL_THREAD_ID=<thread-id> to route memory writes through runtime approvals",
                ));
            }
            return execute_mcp_remember(input, state);
        }
        "exec_shell_list" => ExecShellListTool.execute(input)?,
        "exec_shell_show" => ExecShellShowTool.execute(input)?,
        "exec_shell_wait" => ExecShellWaitTool {
            tool_name: "exec_shell_wait",
        }
        .execute(input)?,
        "exec_wait" => ExecShellWaitTool {
            tool_name: "exec_wait",
        }
        .execute(input)?,
        "task_shell_wait" => TaskShellWaitTool.execute(input)?,
        "rlm_chunk_plan" => RlmChunkPlanTool.execute(input)?,
        "rlm_map_reduce_plan" => RlmMapReducePlanTool.execute(input)?,
        "rlm_recursive_plan" => RlmRecursivePlanTool.execute(input)?,
        "rlm_python" => RlmPythonTool.execute(input)?,
        "rlm_python_sessions" => RlmPythonSessionsTool {
            config: state.config.clone(),
        }
        .execute(input)?,
        "rlm_python_session" => {
            if !mcp_side_effect_tools_enabled(state) {
                return Err(app_error(
                    "MCP RLM state tool `rlm_python_session` is disabled; set DSCODE_MCP_ENABLE_SIDE_EFFECTS=1 for trusted direct execution or DSCODE_MCP_ENABLE_DURABLE_APPROVALS=1 to route through runtime approvals",
                ));
            }
            return execute_mcp_rlm_python_session(input, state);
        }
        "rlm" => {
            return execute_mcp_model_rlm_tool("rlm", input, state);
        }
        "rlm_query" => {
            return execute_mcp_model_rlm_tool("rlm_query", input, state);
        }
        "llm_query" => {
            return execute_mcp_model_rlm_tool("llm_query", input, state);
        }
        "rlm_process" => {
            return execute_mcp_model_rlm_tool("rlm_process", input, state);
        }
        "rlm_batch" => {
            return execute_mcp_model_rlm_tool("rlm_batch", input, state);
        }
        "rlm_query_batched" => {
            return execute_mcp_model_rlm_tool("rlm_query_batched", input, state);
        }
        "llm_query_batched" => {
            return execute_mcp_model_rlm_tool("llm_query_batched", input, state);
        }
        "github_comment" => {
            if !mcp_write_tools_enabled(state) {
                return Err(app_error(
                    "MCP write tool `github_comment` is disabled; set DSCODE_MCP_ENABLE_DURABLE_APPROVALS=1 or DSCODE_MCP_APPROVAL_THREAD_ID=<thread-id> to route writes through runtime approvals",
                ));
            }
            return execute_mcp_github_comment(input, state);
        }
        "github_close_issue" => {
            if !mcp_write_tools_enabled(state) {
                return Err(app_error(
                    "MCP write tool `github_close_issue` is disabled; set DSCODE_MCP_ENABLE_DURABLE_APPROVALS=1 or DSCODE_MCP_APPROVAL_THREAD_ID=<thread-id> to route writes through runtime approvals",
                ));
            }
            return execute_mcp_github_close_issue(input, state);
        }
        "diagnostics" => DiagnosticsTool.execute(input)?,
        "run_tests" => {
            if !mcp_side_effect_tools_enabled(state) {
                return Err(app_error(
                    "MCP side-effect tool `run_tests` is disabled; set DSCODE_MCP_ENABLE_SIDE_EFFECTS=1 for trusted direct execution or DSCODE_MCP_ENABLE_DURABLE_APPROVALS=1 to route through runtime approvals",
                ));
            }
            return execute_mcp_run_tests(input, state);
        }
        "exec_shell" => {
            if !mcp_side_effect_tools_enabled(state) {
                return Err(app_error(
                    "MCP shell-session tool `exec_shell` is disabled; set DSCODE_MCP_ENABLE_SIDE_EFFECTS=1 for trusted direct execution or DSCODE_MCP_ENABLE_DURABLE_APPROVALS=1 to route through runtime approvals",
                ));
            }
            return execute_mcp_shell_tool("exec_shell", input, state);
        }
        "task_shell_start" => {
            if !mcp_side_effect_tools_enabled(state) {
                return Err(app_error(
                    "MCP shell-session tool `task_shell_start` is disabled; set DSCODE_MCP_ENABLE_SIDE_EFFECTS=1 for trusted direct execution or DSCODE_MCP_ENABLE_DURABLE_APPROVALS=1 to route through runtime approvals",
                ));
            }
            return execute_mcp_shell_tool("task_shell_start", input, state);
        }
        "exec_shell_interact" | "exec_interact" => {
            if !mcp_side_effect_tools_enabled(state) {
                return Err(app_error(
                    "MCP shell-session stdin tool is disabled; set DSCODE_MCP_ENABLE_SIDE_EFFECTS=1 for trusted direct execution or DSCODE_MCP_ENABLE_DURABLE_APPROVALS=1 to route through runtime approvals",
                ));
            }
            return execute_mcp_shell_tool(name, input, state);
        }
        "exec_shell_cancel" => {
            if !mcp_side_effect_tools_enabled(state) {
                return Err(app_error(
                    "MCP shell-session cancel tool `exec_shell_cancel` is disabled; set DSCODE_MCP_ENABLE_SIDE_EFFECTS=1 for trusted direct execution or DSCODE_MCP_ENABLE_DURABLE_APPROVALS=1 to route through runtime approvals",
                ));
            }
            return execute_mcp_shell_tool("exec_shell_cancel", input, state);
        }
        "apply_patch" => {
            if !mcp_write_tools_enabled(state) {
                return Err(app_error(
                    "MCP write tool `apply_patch` is disabled; set DSCODE_MCP_ENABLE_DURABLE_APPROVALS=1 or DSCODE_MCP_APPROVAL_THREAD_ID=<thread-id> to route writes through runtime approvals",
                ));
            }
            return execute_mcp_apply_patch(input, state);
        }
        "write_file" => {
            if !mcp_write_tools_enabled(state) {
                return Err(app_error(
                    "MCP write tool `write_file` is disabled; set DSCODE_MCP_ENABLE_DURABLE_APPROVALS=1 or DSCODE_MCP_APPROVAL_THREAD_ID=<thread-id> to route writes through runtime approvals",
                ));
            }
            return execute_mcp_write_file(input, state);
        }
        "edit_file" => {
            if !mcp_write_tools_enabled(state) {
                return Err(app_error(
                    "MCP write tool `edit_file` is disabled; set DSCODE_MCP_ENABLE_DURABLE_APPROVALS=1 or DSCODE_MCP_APPROVAL_THREAD_ID=<thread-id> to route writes through runtime approvals",
                ));
            }
            return execute_mcp_edit_file(input, state);
        }
        "delete_file" => {
            if !mcp_write_tools_enabled(state) {
                return Err(app_error(
                    "MCP write tool `delete_file` is disabled; set DSCODE_MCP_ENABLE_DURABLE_APPROVALS=1 or DSCODE_MCP_APPROVAL_THREAD_ID=<thread-id> to route writes through runtime approvals",
                ));
            }
            return execute_mcp_delete_file(input, state);
        }
        "copy_file" => {
            if !mcp_write_tools_enabled(state) {
                return Err(app_error(
                    "MCP write tool `copy_file` is disabled; set DSCODE_MCP_ENABLE_DURABLE_APPROVALS=1 or DSCODE_MCP_APPROVAL_THREAD_ID=<thread-id> to route writes through runtime approvals",
                ));
            }
            return execute_mcp_copy_file(input, state);
        }
        "move_file" => {
            if !mcp_write_tools_enabled(state) {
                return Err(app_error(
                    "MCP write tool `move_file` is disabled; set DSCODE_MCP_ENABLE_DURABLE_APPROVALS=1 or DSCODE_MCP_APPROVAL_THREAD_ID=<thread-id> to route writes through runtime approvals",
                ));
            }
            return execute_mcp_move_file(input, state);
        }
        "revert_turn" => {
            if !mcp_write_tools_enabled(state) {
                return Err(app_error(
                    "MCP write tool `revert_turn` is disabled; set DSCODE_MCP_ENABLE_DURABLE_APPROVALS=1 or DSCODE_MCP_APPROVAL_THREAD_ID=<thread-id> to route rollback restores through runtime approvals",
                ));
            }
            return execute_mcp_revert_turn(input, state);
        }
        "run_shell" => {
            if !mcp_side_effect_tools_enabled(state) {
                return Err(app_error(
                    "MCP side-effect tool `run_shell` is disabled; set DSCODE_MCP_ENABLE_SIDE_EFFECTS=1 for trusted direct execution or DSCODE_MCP_ENABLE_DURABLE_APPROVALS=1 to route through runtime approvals",
                ));
            }
            return execute_mcp_run_shell(input, state);
        }
        "runtime_health" => {
            return Ok(json_value_to_string(&object([
                ("status", JsonValue::String("ok".to_string())),
                ("service", JsonValue::String("DeepSeekCode".to_string())),
                (
                    "version",
                    JsonValue::String(env!("CARGO_PKG_VERSION").to_string()),
                ),
                ("runtime", JsonValue::String("mcp".to_string())),
            ])))
        }
        "runtime_list_sessions" => {
            let limit = mcp_limit(arguments, 20, 100);
            let sessions = state
                .store
                .list_sessions(limit)?
                .iter()
                .map(session_to_json)
                .collect::<Vec<_>>();
            return Ok(json_value_to_string(&JsonValue::Array(sessions)));
        }
        "runtime_list_threads" => {
            let limit = mcp_limit(arguments, 20, 100);
            let threads = state
                .store
                .list_threads(limit)?
                .iter()
                .map(thread_to_json)
                .collect::<Vec<_>>();
            return Ok(json_value_to_string(&JsonValue::Array(threads)));
        }
        "runtime_read_thread" => {
            let thread_id = mcp_required_string(arguments, "thread_id")?;
            let thread = state.store.load_thread(thread_id)?;
            let turns = state
                .store
                .list_turns(thread_id)?
                .iter()
                .map(turn_to_json)
                .collect::<Vec<_>>();
            let items = state
                .store
                .list_items(thread_id, None)?
                .iter()
                .map(item_to_json)
                .collect::<Vec<_>>();
            return Ok(json_value_to_string(&object([
                ("thread", thread_to_json(&thread)),
                ("turns", JsonValue::Array(turns)),
                ("items", JsonValue::Array(items)),
            ])));
        }
        "runtime_list_tasks" => {
            let limit = mcp_limit(arguments, 20, 100);
            let tasks = state
                .store
                .list_tasks(None, None, limit)?
                .iter()
                .map(task_to_json)
                .collect::<Vec<_>>();
            return Ok(json_value_to_string(&JsonValue::Array(tasks)));
        }
        "runtime_read_task" => {
            let task_id = mcp_required_string(arguments, "task_id")?;
            let task = state.store.load_task(task_id)?;
            return Ok(json_value_to_string(&task_to_json(&task)));
        }
        "runtime_list_agents" => {
            let limit = mcp_limit(arguments, 20, 100);
            let agents = state
                .store
                .list_tasks(None, None, limit)?
                .into_iter()
                .filter(mcp_is_agent_task)
                .map(|task| mcp_agent_snapshot_json(&state.store, &task))
                .collect::<AppResult<Vec<_>>>()?;
            return Ok(json_value_to_string(&json_object([
                (
                    "summary",
                    JsonValue::String(format!("{} sub-agent(s)", agents.len())),
                ),
                ("agents", json_array(agents)),
            ])));
        }
        "runtime_agent_result" => {
            let agent_id = mcp_required_any(arguments, &["agent_id", "id"])?;
            let task = state.store.load_task(&agent_id)?;
            mcp_ensure_agent_task(&task, "runtime_agent_result")?;
            return Ok(json_value_to_string(&mcp_agent_snapshot_json(
                &state.store,
                &task,
            )?));
        }
        "runtime_create_task" => {
            if !mcp_write_tools_enabled(state) {
                return Err(app_error(
                    "MCP write tool `runtime_create_task` is disabled; set DSCODE_MCP_ENABLE_DURABLE_APPROVALS=1 or DSCODE_MCP_APPROVAL_THREAD_ID=<thread-id> to route task writes through runtime approvals",
                ));
            }
            return execute_mcp_runtime_create_task(input, state);
        }
        "runtime_cancel_task" => {
            if !mcp_write_tools_enabled(state) {
                return Err(app_error(
                    "MCP write tool `runtime_cancel_task` is disabled; set DSCODE_MCP_ENABLE_DURABLE_APPROVALS=1 or DSCODE_MCP_APPROVAL_THREAD_ID=<thread-id> to route task writes through runtime approvals",
                ));
            }
            return execute_mcp_runtime_cancel_task(input, state);
        }
        "runtime_create_automation" => {
            if !mcp_write_tools_enabled(state) {
                return Err(app_error(
                    "MCP write tool `runtime_create_automation` is disabled; set DSCODE_MCP_ENABLE_DURABLE_APPROVALS=1 or DSCODE_MCP_APPROVAL_THREAD_ID=<thread-id> to route automation writes through runtime approvals",
                ));
            }
            return execute_mcp_runtime_create_automation(input, state);
        }
        "runtime_update_automation" => {
            if !mcp_write_tools_enabled(state) {
                return Err(app_error(
                    "MCP write tool `runtime_update_automation` is disabled; set DSCODE_MCP_ENABLE_DURABLE_APPROVALS=1 or DSCODE_MCP_APPROVAL_THREAD_ID=<thread-id> to route automation writes through runtime approvals",
                ));
            }
            return execute_mcp_runtime_update_automation(input, state);
        }
        "runtime_pause_automation" => {
            if !mcp_write_tools_enabled(state) {
                return Err(app_error(
                    "MCP write tool `runtime_pause_automation` is disabled; set DSCODE_MCP_ENABLE_DURABLE_APPROVALS=1 or DSCODE_MCP_APPROVAL_THREAD_ID=<thread-id> to route automation writes through runtime approvals",
                ));
            }
            return execute_mcp_runtime_pause_automation(input, state);
        }
        "runtime_resume_automation" => {
            if !mcp_write_tools_enabled(state) {
                return Err(app_error(
                    "MCP write tool `runtime_resume_automation` is disabled; set DSCODE_MCP_ENABLE_DURABLE_APPROVALS=1 or DSCODE_MCP_APPROVAL_THREAD_ID=<thread-id> to route automation writes through runtime approvals",
                ));
            }
            return execute_mcp_runtime_resume_automation(input, state);
        }
        "runtime_delete_automation" => {
            if !mcp_write_tools_enabled(state) {
                return Err(app_error(
                    "MCP write tool `runtime_delete_automation` is disabled; set DSCODE_MCP_ENABLE_DURABLE_APPROVALS=1 or DSCODE_MCP_APPROVAL_THREAD_ID=<thread-id> to route automation writes through runtime approvals",
                ));
            }
            return execute_mcp_runtime_delete_automation(input, state);
        }
        "runtime_trigger_automation" => {
            if !mcp_write_tools_enabled(state) {
                return Err(app_error(
                    "MCP write tool `runtime_trigger_automation` is disabled; set DSCODE_MCP_ENABLE_DURABLE_APPROVALS=1 or DSCODE_MCP_APPROVAL_THREAD_ID=<thread-id> to route automation writes through runtime approvals",
                ));
            }
            return execute_mcp_runtime_trigger_automation(input, state);
        }
        "runtime_spawn_agent" => {
            if !mcp_write_tools_enabled(state) {
                return Err(app_error(
                    "MCP write tool `runtime_spawn_agent` is disabled; set DSCODE_MCP_ENABLE_DURABLE_APPROVALS=1 or DSCODE_MCP_APPROVAL_THREAD_ID=<thread-id> to route sub-agent writes through runtime approvals",
                ));
            }
            return execute_mcp_runtime_spawn_agent(input, state);
        }
        "runtime_cancel_agent" => {
            if !mcp_write_tools_enabled(state) {
                return Err(app_error(
                    "MCP write tool `runtime_cancel_agent` is disabled; set DSCODE_MCP_ENABLE_DURABLE_APPROVALS=1 or DSCODE_MCP_APPROVAL_THREAD_ID=<thread-id> to route sub-agent writes through runtime approvals",
                ));
            }
            return execute_mcp_runtime_cancel_agent(input, state);
        }
        "runtime_close_agent" => {
            if !mcp_write_tools_enabled(state) {
                return Err(app_error(
                    "MCP write tool `runtime_close_agent` is disabled; set DSCODE_MCP_ENABLE_DURABLE_APPROVALS=1 or DSCODE_MCP_APPROVAL_THREAD_ID=<thread-id> to route sub-agent writes through runtime approvals",
                ));
            }
            return execute_mcp_runtime_close_agent(input, state);
        }
        "runtime_resume_agent" => {
            if !mcp_write_tools_enabled(state) {
                return Err(app_error(
                    "MCP write tool `runtime_resume_agent` is disabled; set DSCODE_MCP_ENABLE_DURABLE_APPROVALS=1 or DSCODE_MCP_APPROVAL_THREAD_ID=<thread-id> to route sub-agent writes through runtime approvals",
                ));
            }
            return execute_mcp_runtime_resume_agent(input, state);
        }
        "runtime_send_agent_input" => {
            if !mcp_write_tools_enabled(state) {
                return Err(app_error(
                    "MCP write tool `runtime_send_agent_input` is disabled; set DSCODE_MCP_ENABLE_DURABLE_APPROVALS=1 or DSCODE_MCP_APPROVAL_THREAD_ID=<thread-id> to route sub-agent writes through runtime approvals",
                ));
            }
            return execute_mcp_runtime_send_agent_input(input, state);
        }
        _ => return Err(app_error(format!("unknown MCP tool: {name}"))),
    };
    Ok(output.summary)
}

fn mcp_input_with_workspace_defaults(
    name: &str,
    mut input: ToolInput,
    state: &McpStdioState,
) -> ToolInput {
    input
        .args
        .entry("cwd".to_string())
        .or_insert_with(|| state.workspace.display().to_string());
    match name {
        "read_file" => absolutize_mcp_path_arg(&mut input, "path", &state.workspace),
        "validate_data" => {
            if input.get("path").is_some() {
                absolutize_mcp_path_arg(&mut input, "path", &state.workspace);
            }
        }
        "list_files" | "search_text" => {
            input
                .args
                .entry("root".to_string())
                .or_insert_with(|| state.workspace.display().to_string());
            absolutize_mcp_path_arg(&mut input, "root", &state.workspace);
        }
        "list_dir" | "grep_files" | "file_search" | "project_map" => {
            input
                .args
                .entry("path".to_string())
                .or_insert_with(|| state.workspace.display().to_string());
            absolutize_mcp_path_arg(&mut input, "path", &state.workspace);
        }
        _ => {}
    }
    input
}

fn absolutize_mcp_path_arg(input: &mut ToolInput, key: &str, workspace: &Path) {
    let Some(value) = input.args.get(key).cloned() else {
        return;
    };
    let path = Path::new(&value);
    if path.is_absolute() {
        return;
    }
    input
        .args
        .insert(key.to_string(), workspace.join(path).display().to_string());
}

fn mcp_side_effect_tools_enabled(state: &McpStdioState) -> bool {
    state.allow_side_effect_tools || state.approval_thread_id.is_some()
}

fn mcp_write_tools_enabled(state: &McpStdioState) -> bool {
    state.approval_thread_id.is_some()
}

fn execute_mcp_apply_patch(mut input: ToolInput, state: &McpStdioState) -> AppResult<String> {
    input
        .args
        .entry("cwd".to_string())
        .or_insert_with(|| state.workspace.display().to_string());
    let Some(thread_id) = state.approval_thread_id.as_deref() else {
        return Err(app_error(
            "MCP write tool `apply_patch` is disabled; durable runtime approvals are required",
        ));
    };
    if state.approval.require_write_confirmation && !env_flag("DSCODE_AUTO_APPROVE_WRITES") {
        let approval = state.store.append_permission_request(
            thread_id,
            state.approval_turn_id.as_deref(),
            "apply_patch".to_string(),
            "write".to_string(),
            mcp_apply_patch_target(&input),
            input.args.clone(),
        )?;
        wait_for_mcp_permission_response(state, thread_id, &approval, "apply_patch")?;
    }
    Ok(ApplyPatchTool::new(state.diagnostics.clone())
        .execute(input)?
        .summary)
}

fn mcp_apply_patch_target(input: &ToolInput) -> String {
    if let Some(path) = input.get("path") {
        return path.to_string();
    }
    if let Some(cwd) = input.get("cwd") {
        return format!("{cwd} (unified diff)");
    }
    "current workspace".to_string()
}

fn execute_mcp_write_file(input: ToolInput, state: &McpStdioState) -> AppResult<String> {
    let Some(path) = input.get("path") else {
        return Err(app_error("write_file requires a path"));
    };
    let Some(content) = input.get("content") else {
        return Err(app_error("write_file requires content"));
    };
    let target = safe_mcp_workspace_path(&state.workspace, path, "write_file")?;
    let Some(thread_id) = state.approval_thread_id.as_deref() else {
        return Err(app_error(
            "MCP write tool `write_file` is disabled; durable runtime approvals are required",
        ));
    };
    if state.approval.require_write_confirmation && !env_flag("DSCODE_AUTO_APPROVE_WRITES") {
        let approval = state.store.append_permission_request(
            thread_id,
            state.approval_turn_id.as_deref(),
            "write_file".to_string(),
            "write".to_string(),
            path.to_string(),
            input.args.clone(),
        )?;
        wait_for_mcp_permission_response(state, thread_id, &approval, "write_file")?;
    }
    if let Ok(metadata) = fs::symlink_metadata(&target) {
        if metadata.file_type().is_symlink() {
            return Err(app_error(format!(
                "write_file refuses symlink target: {}",
                target.display()
            )));
        }
        if metadata.is_dir() {
            return Err(app_error(format!(
                "write_file target is a directory: {}",
                target.display()
            )));
        }
    }
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)?;
    }
    let existed = target.exists();
    fs::write(&target, content)?;
    Ok(format!(
        "Wrote {} bytes to {} ({})",
        content.len(),
        path,
        if existed { "overwritten" } else { "created" }
    ))
}

fn execute_mcp_pandoc_convert(input: ToolInput, state: &McpStdioState) -> AppResult<String> {
    let writes_output = input
        .get("output_path")
        .map(|value| !value.trim().is_empty())
        .unwrap_or(false);
    if writes_output {
        let output_path = input.get("output_path").unwrap_or("");
        let _target = safe_mcp_workspace_path(&state.workspace, output_path, "pandoc_convert")?;
        let Some(thread_id) = state.approval_thread_id.as_deref() else {
            return Err(app_error(
                "MCP write mode for `pandoc_convert` is disabled; durable runtime approvals are required when output_path is set",
            ));
        };
        if state.approval.require_write_confirmation && !env_flag("DSCODE_AUTO_APPROVE_WRITES") {
            let approval = state.store.append_permission_request(
                thread_id,
                state.approval_turn_id.as_deref(),
                "pandoc_convert".to_string(),
                "write".to_string(),
                format!("pandoc convert {output_path}"),
                input.args.clone(),
            )?;
            wait_for_mcp_permission_response(state, thread_id, &approval, "pandoc_convert")?;
        }
    }
    Ok(PandocConvertTool.execute(input)?.summary)
}

fn execute_mcp_edit_file(input: ToolInput, state: &McpStdioState) -> AppResult<String> {
    let Some(path) = input.get("path") else {
        return Err(app_error("edit_file requires a path"));
    };
    let Some(thread_id) = state.approval_thread_id.as_deref() else {
        return Err(app_error(
            "MCP write tool `edit_file` is disabled; durable runtime approvals are required",
        ));
    };
    if state.approval.require_write_confirmation && !env_flag("DSCODE_AUTO_APPROVE_WRITES") {
        let approval = state.store.append_permission_request(
            thread_id,
            state.approval_turn_id.as_deref(),
            "edit_file".to_string(),
            "write".to_string(),
            path.to_string(),
            input.args.clone(),
        )?;
        wait_for_mcp_permission_response(state, thread_id, &approval, "edit_file")?;
    }
    Ok(EditFileTool.execute(input)?.summary)
}

fn execute_mcp_delete_file(input: ToolInput, state: &McpStdioState) -> AppResult<String> {
    let Some(path) = input.get("path") else {
        return Err(app_error("delete_file requires a path"));
    };
    let target = safe_mcp_workspace_path(&state.workspace, path, "delete_file")?;
    let Some(thread_id) = state.approval_thread_id.as_deref() else {
        return Err(app_error(
            "MCP write tool `delete_file` is disabled; durable runtime approvals are required",
        ));
    };
    if state.approval.require_write_confirmation && !env_flag("DSCODE_AUTO_APPROVE_WRITES") {
        let approval = state.store.append_permission_request(
            thread_id,
            state.approval_turn_id.as_deref(),
            "delete_file".to_string(),
            "write".to_string(),
            path.to_string(),
            input.args.clone(),
        )?;
        wait_for_mcp_permission_response(state, thread_id, &approval, "delete_file")?;
    }
    let metadata = fs::symlink_metadata(&target).map_err(|error| {
        app_error(format!(
            "delete_file failed to inspect `{}`: {error}",
            target.display()
        ))
    })?;
    if metadata.file_type().is_symlink() {
        return Err(app_error(format!(
            "delete_file refuses symlink target: {}",
            target.display()
        )));
    }
    if metadata.is_dir() {
        return Err(app_error(format!(
            "delete_file target is a directory: {}",
            target.display()
        )));
    }
    fs::remove_file(&target)?;
    Ok(format!("Deleted file {path}"))
}

fn execute_mcp_copy_file(input: ToolInput, state: &McpStdioState) -> AppResult<String> {
    let Some(source_path) = input.get("source_path") else {
        return Err(app_error("copy_file requires source_path"));
    };
    let Some(destination_path) = input.get("destination_path") else {
        return Err(app_error("copy_file requires destination_path"));
    };
    let source = safe_mcp_workspace_path(&state.workspace, source_path, "copy_file source")?;
    let destination =
        safe_mcp_workspace_path(&state.workspace, destination_path, "copy_file destination")?;
    if source == destination {
        return Err(app_error("copy_file source and destination are the same"));
    }
    let Some(thread_id) = state.approval_thread_id.as_deref() else {
        return Err(app_error(
            "MCP write tool `copy_file` is disabled; durable runtime approvals are required",
        ));
    };
    if state.approval.require_write_confirmation && !env_flag("DSCODE_AUTO_APPROVE_WRITES") {
        let approval = state.store.append_permission_request(
            thread_id,
            state.approval_turn_id.as_deref(),
            "copy_file".to_string(),
            "write".to_string(),
            format!("{source_path} -> {destination_path}"),
            input.args.clone(),
        )?;
        wait_for_mcp_permission_response(state, thread_id, &approval, "copy_file")?;
    }
    let source_metadata = fs::symlink_metadata(&source).map_err(|error| {
        app_error(format!(
            "copy_file failed to inspect source `{}`: {error}",
            source.display()
        ))
    })?;
    if source_metadata.file_type().is_symlink() {
        return Err(app_error(format!(
            "copy_file refuses symlink source: {}",
            source.display()
        )));
    }
    if source_metadata.is_dir() {
        return Err(app_error(format!(
            "copy_file source is a directory: {}",
            source.display()
        )));
    }
    if let Ok(destination_metadata) = fs::symlink_metadata(&destination) {
        if destination_metadata.file_type().is_symlink() {
            return Err(app_error(format!(
                "copy_file refuses symlink destination: {}",
                destination.display()
            )));
        }
        return Err(app_error(format!(
            "copy_file destination already exists: {}",
            destination.display()
        )));
    }
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::copy(&source, &destination)?;
    Ok(format!("Copied file {source_path} to {destination_path}"))
}

fn execute_mcp_move_file(input: ToolInput, state: &McpStdioState) -> AppResult<String> {
    let Some(source_path) = input.get("source_path") else {
        return Err(app_error("move_file requires source_path"));
    };
    let Some(destination_path) = input.get("destination_path") else {
        return Err(app_error("move_file requires destination_path"));
    };
    let source = safe_mcp_workspace_path(&state.workspace, source_path, "move_file source")?;
    let destination =
        safe_mcp_workspace_path(&state.workspace, destination_path, "move_file destination")?;
    if source == destination {
        return Err(app_error("move_file source and destination are the same"));
    }
    let Some(thread_id) = state.approval_thread_id.as_deref() else {
        return Err(app_error(
            "MCP write tool `move_file` is disabled; durable runtime approvals are required",
        ));
    };
    if state.approval.require_write_confirmation && !env_flag("DSCODE_AUTO_APPROVE_WRITES") {
        let approval = state.store.append_permission_request(
            thread_id,
            state.approval_turn_id.as_deref(),
            "move_file".to_string(),
            "write".to_string(),
            format!("{source_path} -> {destination_path}"),
            input.args.clone(),
        )?;
        wait_for_mcp_permission_response(state, thread_id, &approval, "move_file")?;
    }
    let source_metadata = fs::symlink_metadata(&source).map_err(|error| {
        app_error(format!(
            "move_file failed to inspect source `{}`: {error}",
            source.display()
        ))
    })?;
    if source_metadata.file_type().is_symlink() {
        return Err(app_error(format!(
            "move_file refuses symlink source: {}",
            source.display()
        )));
    }
    if source_metadata.is_dir() {
        return Err(app_error(format!(
            "move_file source is a directory: {}",
            source.display()
        )));
    }
    if let Ok(destination_metadata) = fs::symlink_metadata(&destination) {
        if destination_metadata.file_type().is_symlink() {
            return Err(app_error(format!(
                "move_file refuses symlink destination: {}",
                destination.display()
            )));
        }
        return Err(app_error(format!(
            "move_file destination already exists: {}",
            destination.display()
        )));
    }
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::rename(&source, &destination)?;
    Ok(format!("Moved file {source_path} to {destination_path}"))
}

fn execute_mcp_revert_turn(input: ToolInput, state: &McpStdioState) -> AppResult<String> {
    let Some(thread_id) = state.approval_thread_id.as_deref() else {
        return Err(app_error(
            "MCP write tool `revert_turn` is disabled; durable runtime approvals are required",
        ));
    };
    if state.approval.require_write_confirmation && !env_flag("DSCODE_AUTO_APPROVE_WRITES") {
        let approval = state.store.append_permission_request(
            thread_id,
            state.approval_turn_id.as_deref(),
            "revert_turn".to_string(),
            "write".to_string(),
            mcp_revert_turn_target(&input),
            input.args.clone(),
        )?;
        wait_for_mcp_permission_response(state, thread_id, &approval, "revert_turn")?;
    }
    Ok(RevertTurnTool::from_store(state.rollback.clone())
        .execute(input)?
        .summary)
}

fn execute_mcp_github_comment(input: ToolInput, state: &McpStdioState) -> AppResult<String> {
    let Some(thread_id) = state.approval_thread_id.as_deref() else {
        return Err(app_error(
            "MCP write tool `github_comment` is disabled; durable runtime approvals are required",
        ));
    };
    if state.approval.require_write_confirmation && !env_flag("DSCODE_AUTO_APPROVE_WRITES") {
        let approval = state.store.append_permission_request(
            thread_id,
            state.approval_turn_id.as_deref(),
            "github_comment".to_string(),
            "write".to_string(),
            mcp_github_comment_target(&input),
            input.args.clone(),
        )?;
        wait_for_mcp_permission_response(state, thread_id, &approval, "github_comment")?;
    }
    Ok(GithubCommentTool.execute(input)?.summary)
}

fn execute_mcp_github_close_issue(input: ToolInput, state: &McpStdioState) -> AppResult<String> {
    let Some(thread_id) = state.approval_thread_id.as_deref() else {
        return Err(app_error(
            "MCP write tool `github_close_issue` is disabled; durable runtime approvals are required",
        ));
    };
    if state.approval.require_write_confirmation && !env_flag("DSCODE_AUTO_APPROVE_WRITES") {
        let approval = state.store.append_permission_request(
            thread_id,
            state.approval_turn_id.as_deref(),
            "github_close_issue".to_string(),
            "write".to_string(),
            mcp_github_close_issue_target(&input),
            input.args.clone(),
        )?;
        wait_for_mcp_permission_response(state, thread_id, &approval, "github_close_issue")?;
    }
    Ok(GithubCloseIssueTool.execute(input)?.summary)
}

fn execute_mcp_runtime_create_task(input: ToolInput, state: &McpStdioState) -> AppResult<String> {
    let summary = mcp_input_required_any(&input, &["summary", "prompt"], "runtime_create_task")?;
    let Some(approval_thread_id) = state.approval_thread_id.as_deref() else {
        return Err(app_error(
            "MCP write tool `runtime_create_task` is disabled; durable runtime approvals are required",
        ));
    };
    if state.approval.require_write_confirmation && !env_flag("DSCODE_AUTO_APPROVE_WRITES") {
        let approval = state.store.append_permission_request(
            approval_thread_id,
            state.approval_turn_id.as_deref(),
            "runtime_create_task".to_string(),
            "write".to_string(),
            format!("create runtime task: {summary}"),
            input.args.clone(),
        )?;
        wait_for_mcp_permission_response(
            state,
            approval_thread_id,
            &approval,
            "runtime_create_task",
        )?;
    }
    let kind = mcp_input_optional(&input, "kind").unwrap_or_else(|| "agent".to_string());
    let status = mcp_input_optional(&input, "status").unwrap_or_else(|| "pending".to_string());
    let task = state.store.create_task(
        mcp_input_optional(&input, "session_id").as_deref(),
        mcp_input_optional(&input, "thread_id").as_deref(),
        mcp_input_optional(&input, "parent_task_id").as_deref(),
        kind,
        status,
        summary,
    )?;
    Ok(json_value_to_string(&task_to_json(&task)))
}

fn execute_mcp_runtime_cancel_task(input: ToolInput, state: &McpStdioState) -> AppResult<String> {
    let task_id = mcp_input_required_any(&input, &["task_id", "id"], "runtime_cancel_task")?;
    let Some(approval_thread_id) = state.approval_thread_id.as_deref() else {
        return Err(app_error(
            "MCP write tool `runtime_cancel_task` is disabled; durable runtime approvals are required",
        ));
    };
    if state.approval.require_write_confirmation && !env_flag("DSCODE_AUTO_APPROVE_WRITES") {
        let approval = state.store.append_permission_request(
            approval_thread_id,
            state.approval_turn_id.as_deref(),
            "runtime_cancel_task".to_string(),
            "write".to_string(),
            format!("cancel runtime task: {task_id}"),
            input.args.clone(),
        )?;
        wait_for_mcp_permission_response(
            state,
            approval_thread_id,
            &approval,
            "runtime_cancel_task",
        )?;
    }
    let reason = mcp_input_optional(&input, "reason")
        .unwrap_or_else(|| "cancelled by runtime_cancel_task".to_string());
    let (task, event) = state.store.cancel_task(&task_id, reason)?;
    Ok(json_value_to_string(&json_object([
        ("task", task_to_json(&task)),
        (
            "event",
            event.as_ref().map(event_to_json).unwrap_or(JsonValue::Null),
        ),
    ])))
}

fn request_mcp_runtime_write_approval(
    state: &McpStdioState,
    input: &ToolInput,
    tool_name: &str,
    target: String,
) -> AppResult<()> {
    let Some(approval_thread_id) = state.approval_thread_id.as_deref() else {
        return Err(app_error(format!(
            "MCP write tool `{tool_name}` is disabled; durable runtime approvals are required"
        )));
    };
    if state.approval.require_write_confirmation && !env_flag("DSCODE_AUTO_APPROVE_WRITES") {
        let approval = state.store.append_permission_request(
            approval_thread_id,
            state.approval_turn_id.as_deref(),
            tool_name.to_string(),
            "write".to_string(),
            target,
            input.args.clone(),
        )?;
        wait_for_mcp_permission_response(state, approval_thread_id, &approval, tool_name)?;
    }
    Ok(())
}

fn execute_mcp_runtime_create_automation(
    input: ToolInput,
    state: &McpStdioState,
) -> AppResult<String> {
    let name = mcp_input_required_any(&input, &["name"], "runtime_create_automation")?;
    let prompt = mcp_input_required_any(&input, &["prompt"], "runtime_create_automation")?;
    let schedule =
        mcp_input_required_any(&input, &["rrule", "schedule"], "runtime_create_automation")?;
    request_mcp_runtime_write_approval(
        state,
        &input,
        "runtime_create_automation",
        format!("create runtime automation: {name}"),
    )?;
    let status = mcp_input_optional(&input, "status")
        .or_else(|| {
            mcp_input_optional_bool(&input, "paused")
                .filter(|paused| *paused)
                .map(|_| "paused".to_string())
        })
        .unwrap_or_else(|| "active".to_string());
    let automation = state.store.create_automation(
        mcp_input_optional(&input, "session_id").as_deref(),
        mcp_input_optional(&input, "thread_id").as_deref(),
        name,
        status,
        schedule,
        prompt,
        mcp_input_optional(&input, "last_run_at"),
        mcp_input_optional(&input, "next_run_at"),
    )?;
    Ok(json_value_to_string(&automation_to_json(&automation)))
}

fn execute_mcp_runtime_update_automation(
    input: ToolInput,
    state: &McpStdioState,
) -> AppResult<String> {
    let automation_id = mcp_input_required_any(
        &input,
        &["automation_id", "id"],
        "runtime_update_automation",
    )?;
    let name = mcp_input_optional(&input, "name");
    let status = mcp_input_optional(&input, "status").or_else(|| {
        mcp_input_optional_bool(&input, "paused").map(|paused| {
            if paused {
                "paused".to_string()
            } else {
                "active".to_string()
            }
        })
    });
    let schedule =
        mcp_input_optional(&input, "rrule").or_else(|| mcp_input_optional(&input, "schedule"));
    let prompt = mcp_input_optional(&input, "prompt");
    let next_run_at = mcp_input_optional(&input, "next_run_at");
    if name.is_none()
        && status.is_none()
        && schedule.is_none()
        && prompt.is_none()
        && next_run_at.is_none()
    {
        return Err(app_error(
            "runtime_update_automation requires at least one updated field",
        ));
    }
    request_mcp_runtime_write_approval(
        state,
        &input,
        "runtime_update_automation",
        format!("update runtime automation: {automation_id}"),
    )?;
    let automation = state.store.update_automation(
        &automation_id,
        name,
        status,
        schedule,
        prompt,
        next_run_at,
    )?;
    Ok(json_value_to_string(&automation_to_json(&automation)))
}

fn execute_mcp_runtime_pause_automation(
    input: ToolInput,
    state: &McpStdioState,
) -> AppResult<String> {
    let automation_id =
        mcp_input_required_any(&input, &["automation_id", "id"], "runtime_pause_automation")?;
    request_mcp_runtime_write_approval(
        state,
        &input,
        "runtime_pause_automation",
        format!("pause runtime automation: {automation_id}"),
    )?;
    let automation = state.store.pause_automation(&automation_id)?;
    Ok(json_value_to_string(&automation_to_json(&automation)))
}

fn execute_mcp_runtime_resume_automation(
    input: ToolInput,
    state: &McpStdioState,
) -> AppResult<String> {
    let automation_id = mcp_input_required_any(
        &input,
        &["automation_id", "id"],
        "runtime_resume_automation",
    )?;
    request_mcp_runtime_write_approval(
        state,
        &input,
        "runtime_resume_automation",
        format!("resume runtime automation: {automation_id}"),
    )?;
    let automation = state.store.resume_automation(&automation_id)?;
    Ok(json_value_to_string(&automation_to_json(&automation)))
}

fn execute_mcp_runtime_delete_automation(
    input: ToolInput,
    state: &McpStdioState,
) -> AppResult<String> {
    let automation_id = mcp_input_required_any(
        &input,
        &["automation_id", "id"],
        "runtime_delete_automation",
    )?;
    request_mcp_runtime_write_approval(
        state,
        &input,
        "runtime_delete_automation",
        format!("delete runtime automation: {automation_id}"),
    )?;
    let automation = state.store.delete_automation(&automation_id)?;
    Ok(json_value_to_string(&automation_to_json(&automation)))
}

fn execute_mcp_runtime_trigger_automation(
    input: ToolInput,
    state: &McpStdioState,
) -> AppResult<String> {
    let automation_id = mcp_input_required_any(
        &input,
        &["automation_id", "id"],
        "runtime_trigger_automation",
    )?;
    request_mcp_runtime_write_approval(
        state,
        &input,
        "runtime_trigger_automation",
        format!("trigger runtime automation: {automation_id}"),
    )?;
    let prompt_override = mcp_input_optional(&input, "prompt")
        .or_else(|| mcp_input_optional(&input, "prompt_override"));
    let (automation, task) = state
        .store
        .trigger_automation(&automation_id, prompt_override)?;
    Ok(json_value_to_string(&json_object([
        ("automation", automation_to_json(&automation)),
        ("task", task_to_json(&task)),
    ])))
}

fn execute_mcp_runtime_spawn_agent(input: ToolInput, state: &McpStdioState) -> AppResult<String> {
    let prompt = mcp_input_required_any(
        &input,
        &["prompt", "message", "objective", "task"],
        "runtime_spawn_agent",
    )?;
    request_mcp_runtime_write_approval(
        state,
        &input,
        "runtime_spawn_agent",
        format!(
            "spawn runtime sub-agent: {}",
            mcp_compact_target(&prompt, 120)
        ),
    )?;
    let workspace = mcp_input_optional(&input, "cwd")
        .or_else(|| mcp_input_optional(&input, "workspace"))
        .unwrap_or_else(|| state.workspace.display().to_string());
    let model = mcp_input_optional(&input, "model").unwrap_or_else(|| "deepseek-coder".to_string());
    let mode = mcp_input_optional(&input, "mode").unwrap_or_else(|| "agent".to_string());
    let title =
        mcp_input_optional(&input, "title").unwrap_or_else(|| mcp_summarize_agent_prompt(&prompt));
    let thread = match mcp_input_optional(&input, "thread_id") {
        Some(thread_id) => state.store.load_thread(&thread_id)?,
        None => state.store.create_thread(title, workspace, model, mode)?,
    };
    let task = state.store.create_task(
        thread.session_id.as_deref(),
        Some(&thread.id),
        mcp_input_optional(&input, "parent_task_id").as_deref(),
        "subagent".to_string(),
        "pending".to_string(),
        prompt,
    )?;
    Ok(json_value_to_string(&mcp_agent_snapshot_json(
        &state.store,
        &task,
    )?))
}

fn execute_mcp_runtime_cancel_agent(input: ToolInput, state: &McpStdioState) -> AppResult<String> {
    let agent_id = mcp_input_required_any(&input, &["agent_id", "id"], "runtime_cancel_agent")?;
    let task = state.store.load_task(&agent_id)?;
    mcp_ensure_agent_task(&task, "runtime_cancel_agent")?;
    request_mcp_runtime_write_approval(
        state,
        &input,
        "runtime_cancel_agent",
        format!("cancel runtime sub-agent: {agent_id}"),
    )?;
    let (task, event) = state
        .store
        .cancel_task(&agent_id, "cancelled by runtime_cancel_agent".to_string())?;
    Ok(json_value_to_string(&json_object([
        ("agent", mcp_agent_snapshot_json(&state.store, &task)?),
        (
            "event",
            event.as_ref().map(event_to_json).unwrap_or(JsonValue::Null),
        ),
    ])))
}

fn execute_mcp_runtime_close_agent(input: ToolInput, state: &McpStdioState) -> AppResult<String> {
    let agent_id = mcp_input_required_any(&input, &["agent_id", "id"], "runtime_close_agent")?;
    let task = state.store.load_task(&agent_id)?;
    mcp_ensure_agent_task(&task, "runtime_close_agent")?;
    if matches!(task.status.as_str(), "completed" | "failed" | "cancelled") {
        return Ok(json_value_to_string(&mcp_agent_snapshot_json(
            &state.store,
            &task,
        )?));
    }
    request_mcp_runtime_write_approval(
        state,
        &input,
        "runtime_close_agent",
        format!("close runtime sub-agent: {agent_id}"),
    )?;
    let (task, _) = state
        .store
        .cancel_task(&agent_id, "closed by runtime_close_agent".to_string())?;
    Ok(json_value_to_string(&mcp_agent_snapshot_json(
        &state.store,
        &task,
    )?))
}

fn execute_mcp_runtime_resume_agent(input: ToolInput, state: &McpStdioState) -> AppResult<String> {
    let agent_id = mcp_input_required_any(&input, &["agent_id", "id"], "runtime_resume_agent")?;
    let task = state.store.load_task(&agent_id)?;
    mcp_ensure_agent_task(&task, "runtime_resume_agent")?;
    request_mcp_runtime_write_approval(
        state,
        &input,
        "runtime_resume_agent",
        format!("resume runtime sub-agent: {agent_id}"),
    )?;
    let prompt =
        mcp_input_optional(&input, "prompt").or_else(|| mcp_input_optional(&input, "message"));
    let resumed = if task.status == "paused" {
        state.store.resume_task(&task.id, prompt)?
    } else {
        state.store.create_task(
            task.session_id.as_deref(),
            task.thread_id.as_deref(),
            Some(&task.id),
            "subagent".to_string(),
            "pending".to_string(),
            prompt.unwrap_or_else(|| task.summary.clone()),
        )?
    };
    Ok(json_value_to_string(&json_object([
        ("agent_id", JsonValue::String(resumed.id.clone())),
        ("agent", mcp_agent_snapshot_json(&state.store, &resumed)?),
        ("resumed_from", JsonValue::String(task.id)),
    ])))
}

fn execute_mcp_runtime_send_agent_input(
    input: ToolInput,
    state: &McpStdioState,
) -> AppResult<String> {
    let agent_id = mcp_input_required_any(&input, &["agent_id", "id"], "runtime_send_agent_input")?;
    let message = mcp_input_required_any(
        &input,
        &["message", "input", "prompt"],
        "runtime_send_agent_input",
    )?;
    let task = state.store.load_task(&agent_id)?;
    mcp_ensure_agent_task(&task, "runtime_send_agent_input")?;
    let thread_id = task.thread_id.clone().ok_or_else(|| {
        app_error("runtime_send_agent_input requires an agent linked to a runtime thread")
    })?;
    request_mcp_runtime_write_approval(
        state,
        &input,
        "runtime_send_agent_input",
        format!("send input to runtime sub-agent: {agent_id}"),
    )?;
    let turn = state
        .store
        .append_turn(&thread_id, "user".to_string(), message.clone())?;
    let item = state.store.append_item(
        &thread_id,
        Some(&turn.id),
        "message".to_string(),
        Some("user".to_string()),
        message.clone(),
        "completed".to_string(),
    )?;
    let followup = state.store.create_task(
        task.session_id.as_deref(),
        Some(&thread_id),
        Some(&task.id),
        "subagent_input".to_string(),
        "pending".to_string(),
        message,
    )?;
    Ok(json_value_to_string(&json_object([
        ("agent_id", JsonValue::String(task.id)),
        ("queued_agent_id", JsonValue::String(followup.id.clone())),
        (
            "queued_agent",
            mcp_agent_snapshot_json(&state.store, &followup)?,
        ),
        ("input_item", item_to_json(&item)),
    ])))
}

fn mcp_agent_snapshot_json(store: &RuntimeStore, task: &TaskRecord) -> AppResult<JsonValue> {
    let thread = task
        .thread_id
        .as_deref()
        .map(|thread_id| store.load_thread(thread_id))
        .transpose()?;
    let latest_item = match task.thread_id.as_deref() {
        Some(thread_id) => store
            .list_items(thread_id, None)?
            .into_iter()
            .rev()
            .find(|item| item.role.as_deref() == Some("assistant")),
        None => None,
    };
    Ok(json_object([
        ("agent_id", JsonValue::String(task.id.clone())),
        ("status", JsonValue::String(task.status.clone())),
        ("task", task_to_json(task)),
        (
            "thread",
            thread
                .as_ref()
                .map(thread_to_json)
                .unwrap_or(JsonValue::Null),
        ),
        (
            "result",
            latest_item
                .as_ref()
                .map(item_to_json)
                .unwrap_or(JsonValue::Null),
        ),
    ]))
}

fn mcp_ensure_agent_task(task: &TaskRecord, tool_name: &str) -> AppResult<()> {
    if mcp_is_agent_task(task) {
        Ok(())
    } else {
        Err(app_error(format!(
            "{tool_name} expected a sub-agent task id, got task kind `{}`",
            task.kind
        )))
    }
}

fn mcp_is_agent_task(task: &TaskRecord) -> bool {
    task.kind == "subagent" || task.kind == "subagent_input"
}

fn mcp_summarize_agent_prompt(prompt: &str) -> String {
    let mut out = String::new();
    for (index, ch) in prompt.chars().enumerate() {
        if index >= 80 {
            out.push_str("...");
            break;
        }
        out.push(ch);
    }
    if out.trim().is_empty() {
        "Sub-agent task".to_string()
    } else {
        out
    }
}

fn mcp_compact_target(value: &str, max_chars: usize) -> String {
    let compact = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.chars().count() <= max_chars {
        return compact;
    }
    let mut out = compact
        .chars()
        .take(max_chars.saturating_sub(3))
        .collect::<String>();
    out.push_str("...");
    out
}

fn mcp_revert_turn_target(input: &ToolInput) -> String {
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

fn mcp_github_comment_target(input: &ToolInput) -> String {
    let number = input
        .get("number")
        .or_else(|| input.get("issue"))
        .or_else(|| input.get("pr"))
        .or_else(|| input.get("ref"))
        .unwrap_or("?");
    let target = input.get("target").unwrap_or("issue/pr");
    let repo = input
        .get("repo")
        .or_else(|| input.get("repository"))
        .map(|value| format!(" in {value}"))
        .unwrap_or_default();
    format!("github {target} #{number} comment{repo}")
}

fn mcp_github_close_issue_target(input: &ToolInput) -> String {
    let number = input
        .get("number")
        .or_else(|| input.get("issue"))
        .or_else(|| input.get("ref"))
        .unwrap_or("?");
    let repo = input
        .get("repo")
        .or_else(|| input.get("repository"))
        .map(|value| format!(" in {value}"))
        .unwrap_or_default();
    format!("github issue #{number} close{repo}")
}

fn safe_mcp_workspace_path(
    workspace: &Path,
    raw_path: &str,
    tool_name: &str,
) -> AppResult<PathBuf> {
    let raw = Path::new(raw_path);
    if raw.as_os_str().is_empty() || raw.is_absolute() {
        return Err(app_error(format!("unsafe {tool_name} path `{raw_path}`")));
    }
    let mut relative = PathBuf::new();
    for component in raw.components() {
        match component {
            Component::Normal(part) => relative.push(part),
            Component::CurDir => {}
            _ => return Err(app_error(format!("unsafe {tool_name} path `{raw_path}`"))),
        }
    }
    if relative.as_os_str().is_empty() {
        return Err(app_error(format!("unsafe {tool_name} path `{raw_path}`")));
    }
    let target = workspace.join(relative);
    ensure_mcp_path_parent_within_workspace(workspace, &target, tool_name)?;
    Ok(target)
}

fn ensure_mcp_path_parent_within_workspace(
    workspace: &Path,
    target: &Path,
    tool_name: &str,
) -> AppResult<()> {
    let workspace_root = fs::canonicalize(workspace).map_err(|error| {
        app_error(format!(
            "could not resolve MCP workspace `{}`: {error}",
            workspace.display()
        ))
    })?;
    let mut ancestor = target.parent();
    while let Some(path) = ancestor {
        if path.exists() {
            let parent = fs::canonicalize(path).map_err(|error| {
                app_error(format!(
                    "could not resolve {tool_name} parent `{}`: {error}",
                    path.display()
                ))
            })?;
            if parent.starts_with(&workspace_root) {
                return Ok(());
            }
            return Err(app_error(format!(
                "{tool_name} parent escapes MCP workspace: {}",
                path.display()
            )));
        }
        ancestor = path.parent();
    }
    Err(app_error(format!(
        "{tool_name} target has no existing workspace ancestor: {}",
        target.display()
    )))
}

fn execute_mcp_run_shell(input: ToolInput, state: &McpStdioState) -> AppResult<String> {
    let Some(command) = input.get("command") else {
        return Err(app_error("run_shell requires a command"));
    };
    if !is_safe_shell_command(command) {
        return Err(app_error(format!("command not allowed: {command}")));
    }

    if let Some(thread_id) = state.approval_thread_id.as_deref() {
        if state.approval.require_shell_confirmation && !env_flag("DSCODE_AUTO_APPROVE_SHELL") {
            let approval = state.store.append_permission_request(
                thread_id,
                state.approval_turn_id.as_deref(),
                "run_shell".to_string(),
                "shell".to_string(),
                command.to_string(),
                input.args.clone(),
            )?;
            wait_for_mcp_permission_response(state, thread_id, &approval, "run_shell")?;
        }
    } else if !state.allow_side_effect_tools {
        return Err(app_error(
            "MCP side-effect tool `run_shell` is disabled; set DSCODE_MCP_ENABLE_SIDE_EFFECTS=1 for trusted direct execution or DSCODE_MCP_ENABLE_DURABLE_APPROVALS=1 to route through runtime approvals",
        ));
    }

    Ok(RunShellTool.execute(input)?.summary)
}

fn execute_mcp_run_tests(input: ToolInput, state: &McpStdioState) -> AppResult<String> {
    let command = render_run_tests_command(&input)?;
    if let Some(thread_id) = state.approval_thread_id.as_deref() {
        if state.approval.require_shell_confirmation && !env_flag("DSCODE_AUTO_APPROVE_SHELL") {
            let approval = state.store.append_permission_request(
                thread_id,
                state.approval_turn_id.as_deref(),
                "run_tests".to_string(),
                "shell".to_string(),
                command,
                input.args.clone(),
            )?;
            wait_for_mcp_permission_response(state, thread_id, &approval, "run_tests")?;
        }
    } else if !state.allow_side_effect_tools {
        return Err(app_error(
            "MCP side-effect tool `run_tests` is disabled; set DSCODE_MCP_ENABLE_SIDE_EFFECTS=1 for trusted direct execution or DSCODE_MCP_ENABLE_DURABLE_APPROVALS=1 to route through runtime approvals",
        ));
    }

    Ok(RunTestsTool.execute(input)?.summary)
}

fn execute_mcp_shell_tool(
    name: &str,
    input: ToolInput,
    state: &McpStdioState,
) -> AppResult<String> {
    if matches!(name, "exec_shell" | "task_shell_start") {
        let Some(command) = input.get("command") else {
            return Err(app_error(format!("{name} requires a command")));
        };
        if !is_safe_shell_command(command) {
            return Err(app_error(format!("command not allowed: {command}")));
        }
    }
    if let Some(thread_id) = state.approval_thread_id.as_deref() {
        if state.approval.require_shell_confirmation && !env_flag("DSCODE_AUTO_APPROVE_SHELL") {
            let approval = state.store.append_permission_request(
                thread_id,
                state.approval_turn_id.as_deref(),
                name.to_string(),
                "shell".to_string(),
                mcp_shell_target(name, &input),
                input.args.clone(),
            )?;
            wait_for_mcp_permission_response(state, thread_id, &approval, name)?;
        }
    }
    let output = match name {
        "exec_shell" => ExecShellTool.execute(input)?,
        "task_shell_start" => TaskShellStartTool.execute(input)?,
        "exec_shell_interact" => ExecShellInteractTool {
            tool_name: "exec_shell_interact",
        }
        .execute(input)?,
        "exec_interact" => ExecShellInteractTool {
            tool_name: "exec_interact",
        }
        .execute(input)?,
        "exec_shell_cancel" => ExecShellCancelTool.execute(input)?,
        _ => return Err(app_error(format!("unknown MCP shell-session tool: {name}"))),
    };
    Ok(output.summary)
}

fn execute_mcp_rlm_python_session(input: ToolInput, state: &McpStdioState) -> AppResult<String> {
    if let Some(thread_id) = state.approval_thread_id.as_deref() {
        if state.approval.require_write_confirmation && !env_flag("DSCODE_AUTO_APPROVE_WRITES") {
            let approval = state.store.append_permission_request(
                thread_id,
                state.approval_turn_id.as_deref(),
                "rlm_python_session".to_string(),
                "write".to_string(),
                mcp_rlm_python_session_target(&input),
                input.args.clone(),
            )?;
            wait_for_mcp_permission_response(state, thread_id, &approval, "rlm_python_session")?;
        }
    } else if !state.allow_side_effect_tools {
        return Err(app_error(
            "MCP RLM state tool `rlm_python_session` is disabled; set DSCODE_MCP_ENABLE_SIDE_EFFECTS=1 for trusted direct execution or DSCODE_MCP_ENABLE_DURABLE_APPROVALS=1 to route through runtime approvals",
        ));
    }

    Ok(RlmPythonSessionTool {
        config: state.config.clone(),
    }
    .execute(input)?
    .summary)
}

fn execute_mcp_model_rlm_tool(
    name: &'static str,
    input: ToolInput,
    state: &McpStdioState,
) -> AppResult<String> {
    if !mcp_side_effect_tools_enabled(state) {
        return Err(app_error(format!(
            "MCP model-running RLM tool `{name}` is disabled; set DSCODE_MCP_ENABLE_SIDE_EFFECTS=1 for trusted direct execution or DSCODE_MCP_ENABLE_DURABLE_APPROVALS=1 to route through runtime approvals"
        )));
    }
    if let Some(thread_id) = state.approval_thread_id.as_deref() {
        if state.approval.require_mcp_confirmation && !env_flag("DSCODE_AUTO_APPROVE_MCP") {
            let approval = state.store.append_permission_request(
                thread_id,
                state.approval_turn_id.as_deref(),
                name.to_string(),
                "mcp".to_string(),
                mcp_model_rlm_target(name, &input),
                input.args.clone(),
            )?;
            wait_for_mcp_permission_response(state, thread_id, &approval, name)?;
        }
    }

    let output = match name {
        "rlm" | "rlm_query" | "llm_query" | "rlm_process" => RlmTool {
            tool_name: name,
            config: state.config.clone(),
            parent_depth: 0,
        }
        .execute(input)?,
        "rlm_batch" | "rlm_query_batched" | "llm_query_batched" => RlmBatchTool {
            tool_name: name,
            config: state.config.clone(),
            parent_depth: 0,
        }
        .execute(input)?,
        _ => return Err(app_error(format!("unknown MCP RLM tool: {name}"))),
    };
    Ok(output.summary)
}

fn execute_mcp_image_analyze(input: ToolInput, state: &McpStdioState) -> AppResult<String> {
    if !mcp_side_effect_tools_enabled(state) {
        return Err(app_error(
            "MCP model-running vision tool `image_analyze` is disabled; set DSCODE_MCP_ENABLE_SIDE_EFFECTS=1 for trusted direct execution or DSCODE_MCP_ENABLE_DURABLE_APPROVALS=1 to route through runtime approvals",
        ));
    }
    if let Some(thread_id) = state.approval_thread_id.as_deref() {
        if state.approval.require_mcp_confirmation && !env_flag("DSCODE_AUTO_APPROVE_MCP") {
            let approval = state.store.append_permission_request(
                thread_id,
                state.approval_turn_id.as_deref(),
                "image_analyze".to_string(),
                "mcp".to_string(),
                mcp_image_analyze_target(&input),
                input.args.clone(),
            )?;
            wait_for_mcp_permission_response(state, thread_id, &approval, "image_analyze")?;
        }
    }

    Ok(ImageAnalyzeTool::new(&state.config).execute(input)?.summary)
}

fn execute_mcp_note(input: ToolInput, state: &McpStdioState) -> AppResult<String> {
    let Some(thread_id) = state.approval_thread_id.as_deref() else {
        return Err(app_error(
            "MCP write tool `note` is disabled; durable runtime approvals are required",
        ));
    };
    if state.approval.require_write_confirmation && !env_flag("DSCODE_AUTO_APPROVE_WRITES") {
        let target = format!("note {}", state.config.memory.notes_path().display());
        let approval = state.store.append_permission_request(
            thread_id,
            state.approval_turn_id.as_deref(),
            "note".to_string(),
            "write".to_string(),
            target,
            input.args.clone(),
        )?;
        wait_for_mcp_permission_response(state, thread_id, &approval, "note")?;
    }
    Ok(NoteTool::new(state.config.memory.notes_path())
        .execute(input)?
        .summary)
}

fn execute_mcp_remember(input: ToolInput, state: &McpStdioState) -> AppResult<String> {
    let Some(thread_id) = state.approval_thread_id.as_deref() else {
        return Err(app_error(
            "MCP write tool `remember` is disabled; durable runtime approvals are required",
        ));
    };
    if state.approval.require_write_confirmation && !env_flag("DSCODE_AUTO_APPROVE_WRITES") {
        let target = format!("remember {}", state.config.memory.memory_path().display());
        let approval = state.store.append_permission_request(
            thread_id,
            state.approval_turn_id.as_deref(),
            "remember".to_string(),
            "write".to_string(),
            target,
            input.args.clone(),
        )?;
        wait_for_mcp_permission_response(state, thread_id, &approval, "remember")?;
    }
    Ok(RememberTool::new(state.config.memory.memory_path())
        .execute(input)?
        .summary)
}

fn mcp_model_rlm_target(name: &str, input: &ToolInput) -> String {
    input
        .get("task")
        .or_else(|| input.get("question"))
        .or_else(|| input.get("context"))
        .map(|target| format!("{name}: {}", mcp_compact_target(target, 160)))
        .unwrap_or_else(|| name.to_string())
}

fn mcp_image_analyze_target(input: &ToolInput) -> String {
    input
        .get("image_path")
        .or_else(|| input.get("path"))
        .map(|target| format!("image_analyze: {}", mcp_compact_target(target, 160)))
        .unwrap_or_else(|| "image_analyze".to_string())
}

fn mcp_rlm_python_session_target(input: &ToolInput) -> String {
    input
        .get("session_id")
        .map(|session_id| format!("rlm_python_session: {session_id}"))
        .unwrap_or_else(|| "rlm_python_session".to_string())
}

fn mcp_shell_target(name: &str, input: &ToolInput) -> String {
    if matches!(name, "exec_shell" | "task_shell_start") {
        return input
            .get("command")
            .map(|command| mcp_compact_target(command, 160))
            .unwrap_or_else(|| name.to_string());
    }
    if name == "exec_shell_cancel" && input.get("all").is_some_and(truthy_str) {
        return "all background shell jobs".to_string();
    }
    input
        .get("task_id")
        .or_else(|| input.get("id"))
        .map(|task_id| format!("{name}: {task_id}"))
        .unwrap_or_else(|| name.to_string())
}

fn truthy_str(value: &str) -> bool {
    matches!(value.trim(), "1" | "true" | "TRUE" | "yes" | "on")
}

fn wait_for_mcp_permission_response(
    state: &McpStdioState,
    thread_id: &str,
    approval: &RuntimeEvent,
    tool_name: &str,
) -> AppResult<()> {
    let mut polls = 0_usize;
    loop {
        for event in state.store.read_events(thread_id, approval.seq)? {
            if let Some(approved) = mcp_approval_response_decision(&event, &approval.id) {
                if approved {
                    return Ok(());
                }
                return Err(app_error(format!(
                    "MCP {tool_name} denied by runtime approval {}",
                    approval.id
                )));
            }
        }
        polls = polls.saturating_add(1);
        if state
            .approval_max_polls
            .is_some_and(|max_polls| polls >= max_polls)
        {
            return Err(app_error(format!(
                "timed out waiting for MCP permission response {}",
                approval.id
            )));
        }
        thread::sleep(state.approval_poll_interval);
    }
}

fn mcp_approval_response_decision(event: &RuntimeEvent, request_id: &str) -> Option<bool> {
    if event.kind != "permission_response" {
        return None;
    }
    let payload = json_as_object(&event.payload)?;
    let response_request_id = payload.get("request_id").and_then(json_as_string)?;
    if response_request_id != request_id {
        return None;
    }
    match payload.get("decision").and_then(json_as_string)? {
        "approved" => Some(true),
        "denied" => Some(false),
        _ => None,
    }
}

fn tool_input_from_json(arguments: &BTreeMap<String, JsonValue>) -> ToolInput {
    let mut input = ToolInput::new();
    for (key, value) in arguments {
        if matches!(value, JsonValue::Null) {
            continue;
        }
        input = input.with_arg(key, mcp_argument_to_string(value));
    }
    input
}

fn mcp_argument_to_string(value: &JsonValue) -> String {
    match value {
        JsonValue::String(value) => value.clone(),
        JsonValue::Number(value) => value.clone(),
        JsonValue::Bool(value) => value.to_string(),
        JsonValue::Null => String::new(),
        JsonValue::Array(_) | JsonValue::Object(_) => json_value_to_string(value),
    }
}

fn mcp_limit(arguments: &BTreeMap<String, JsonValue>, default: usize, max: usize) -> usize {
    arguments
        .get("limit")
        .and_then(|value| match value {
            JsonValue::Number(value) => value.parse::<usize>().ok(),
            JsonValue::String(value) => value.parse::<usize>().ok(),
            _ => None,
        })
        .unwrap_or(default)
        .clamp(1, max)
}

fn mcp_required_string<'a>(
    arguments: &'a BTreeMap<String, JsonValue>,
    key: &str,
) -> AppResult<&'a str> {
    arguments
        .get(key)
        .and_then(json_as_string)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| app_error(format!("MCP tool requires string argument `{key}`")))
}

fn mcp_required_any(arguments: &BTreeMap<String, JsonValue>, keys: &[&str]) -> AppResult<String> {
    keys.iter()
        .find_map(|key| {
            arguments
                .get(*key)
                .and_then(json_as_string)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
        })
        .ok_or_else(|| app_error(format!("MCP tool requires `{}`", keys.join("` or `"))))
}

fn mcp_input_required_any(input: &ToolInput, keys: &[&str], tool_name: &str) -> AppResult<String> {
    keys.iter()
        .find_map(|key| mcp_input_optional(input, key))
        .ok_or_else(|| app_error(format!("{tool_name} requires `{}`", keys.join("` or `"))))
}

fn mcp_input_optional(input: &ToolInput, key: &str) -> Option<String> {
    input
        .get(key)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn mcp_input_optional_bool(input: &ToolInput, key: &str) -> Option<bool> {
    input
        .get(key)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| matches!(value, "1" | "true" | "TRUE" | "yes" | "on"))
}

fn mcp_success_response(id: JsonValue, result: JsonValue) -> JsonValue {
    object([
        ("jsonrpc", JsonValue::String("2.0".to_string())),
        ("id", id),
        ("result", result),
    ])
}

fn mcp_error_response(id: JsonValue, code: i64, message: &str, data: &str) -> JsonValue {
    object([
        ("jsonrpc", JsonValue::String("2.0".to_string())),
        ("id", id),
        (
            "error",
            object([
                ("code", JsonValue::Number(code.to_string())),
                ("message", JsonValue::String(message.to_string())),
                ("data", JsonValue::String(data.to_string())),
            ]),
        ),
    ])
}

fn mcp_tool_text_result(text: String, is_error: bool) -> JsonValue {
    object([
        (
            "content",
            JsonValue::Array(vec![object([
                ("type", JsonValue::String("text".to_string())),
                ("text", JsonValue::String(text)),
            ])]),
        ),
        ("isError", JsonValue::Bool(is_error)),
    ])
}

fn mcp_tool_definitions(state: &McpStdioState) -> Vec<JsonValue> {
    let mut tools = vec![
        mcp_tool_definition(
            "list_files",
            "List workspace files with depth and result limits.",
            mcp_schema(
                vec![
                    ("root", string_property("Directory to list, default `.`.")),
                    (
                        "max_depth",
                        number_property("Maximum directory depth, default 3."),
                    ),
                    ("limit", number_property("Maximum entries, default 40.")),
                ],
                &[],
            ),
        ),
        mcp_tool_definition(
            "list_dir",
            "DeepSeek-TUI-compatible alias for listing workspace files and directories.",
            mcp_schema(
                vec![
                    ("path", string_property("Directory to list, default `.`.")),
                    (
                        "max_depth",
                        number_property("Maximum directory depth, default 3."),
                    ),
                    ("limit", number_property("Maximum entries, default 40.")),
                ],
                &[],
            ),
        ),
        mcp_tool_definition(
            "read_file",
            "Read a UTF-8 file with line numbers.",
            mcp_schema(
                vec![
                    ("path", string_property("Path to read.")),
                    ("max_lines", number_property("Maximum lines, default 80.")),
                ],
                &["path"],
            ),
        ),
        mcp_tool_definition(
            "retrieve_tool_result",
            "Retrieve a spilled large tool result by id, filename, or spillover path.",
            mcp_schema(
                vec![
                    ("ref", string_property("Tool output ref or spillover path.")),
                    (
                        "mode",
                        string_property("summary, head, tail, lines, or query."),
                    ),
                    ("query", string_property("Substring for query mode.")),
                    ("lines", string_property("Line selector such as 10-40.")),
                    ("start_line", number_property("1-based start line.")),
                    ("end_line", number_property("1-based end line.")),
                    ("line_count", number_property("Head/tail line count.")),
                    ("max_bytes", number_property("Maximum excerpt bytes.")),
                    ("max_matches", number_property("Maximum matches.")),
                    ("context_lines", number_property("Query context lines.")),
                ],
                &["ref"],
            ),
        ),
        mcp_tool_definition(
            "search_text",
            "Search workspace text for a literal query.",
            mcp_schema(
                vec![
                    ("query", string_property("Literal query to search for.")),
                    ("root", string_property("Search root, default `.`.")),
                    ("limit", number_property("Maximum matches, default 20.")),
                ],
                &["query"],
            ),
        ),
        mcp_tool_definition(
            "grep_files",
            "DeepSeek-TUI-compatible literal text search over workspace files.",
            mcp_schema(
                vec![
                    ("pattern", string_property("Literal pattern to search for.")),
                    ("path", string_property("Search root, default `.`.")),
                    (
                        "max_results",
                        number_property("Maximum matches, default 20."),
                    ),
                    ("limit", number_property("Maximum matches, default 20.")),
                ],
                &["pattern"],
            ),
        ),
        mcp_tool_definition(
            "file_search",
            "Find workspace files by filename or path.",
            mcp_schema(
                vec![
                    ("query", string_property("Filename or path query.")),
                    ("path", string_property("Search root, default `.`.")),
                    (
                        "extensions",
                        string_property("Optional comma-separated extensions."),
                    ),
                    ("limit", number_property("Maximum matches, default 20.")),
                    (
                        "max_results",
                        number_property("Maximum matches, default 20."),
                    ),
                ],
                &["query"],
            ),
        ),
        mcp_tool_definition(
            "web_search",
            "Search the web and return ranked results with URLs and snippets.",
            mcp_schema(
                vec![
                    ("query", string_property("Search query.")),
                    ("q", string_property("Search query alias.")),
                    (
                        "search_query",
                        string_property("JSON array compatibility form with q/max_results."),
                    ),
                    (
                        "max_results",
                        number_property("Maximum results, default 5 and max 10."),
                    ),
                    (
                        "timeout_ms",
                        number_property("Timeout milliseconds, default 15000."),
                    ),
                ],
                &[],
            ),
        ),
        mcp_tool_definition(
            "web_run",
            "DeepSeek-TUI-style aggregate web wrapper for search_query, open, click, find, finance, image_query, and cached-PDF screenshot actions.",
            mcp_schema(
                vec![
                    (
                        "search_query",
                        string_property("JSON object or array of search actions with q/query."),
                    ),
                    (
                        "open",
                        string_property("JSON object/array or direct URL to fetch/open."),
                    ),
                    (
                        "click",
                        string_property("JSON object or array with ref_id and id/link_id."),
                    ),
                    (
                        "find",
                        string_property("JSON object or array with ref_id/url and pattern."),
                    ),
                    (
                        "finance",
                        string_property("JSON object or array with ticker/symbol."),
                    ),
                    (
                        "image_query",
                        string_property("JSON object or array of image search actions with q/query."),
                    ),
                    (
                        "screenshot",
                        string_property("JSON object or array with cached PDF ref_id and pageno."),
                    ),
                    (
                        "response_length",
                        string_property("short, medium, or long page window length."),
                    ),
                ],
                &[],
            ),
        ),
        mcp_tool_definition(
            "fetch_url",
            "Fetch a known HTTP/HTTPS URL and return decoded text or raw content.",
            mcp_schema(
                vec![
                    ("url", string_property("Absolute HTTP/HTTPS URL.")),
                    ("format", string_property("text, markdown, or raw.")),
                    (
                        "max_bytes",
                        number_property("Maximum response bytes, default 1000000."),
                    ),
                    (
                        "timeout_ms",
                        number_property("Timeout milliseconds, default 15000."),
                    ),
                ],
                &["url"],
            ),
        ),
        mcp_tool_definition(
            "finance",
            "Fetch a live market quote through a Yahoo Finance-compatible endpoint.",
            mcp_schema(
                vec![
                    ("ticker", string_property("Ticker symbol to look up.")),
                    ("symbol", string_property("Alias for ticker.")),
                    (
                        "type",
                        string_property("Optional asset type hint such as equity or crypto."),
                    ),
                    ("market", string_property("Optional market hint.")),
                    (
                        "timeout_ms",
                        number_property("Timeout milliseconds, default 10000."),
                    ),
                ],
                &[],
            ),
        ),
        mcp_tool_definition(
            "pandoc_convert",
            "Convert a workspace document through local pandoc. Inline text output is read-only; output_path requires durable runtime approvals.",
            mcp_schema(
                vec![
                    ("source_path", string_property("Workspace-relative source document path.")),
                    ("target_format", string_property("Target format such as markdown, html, plain, docx, odt, or epub.")),
                    ("output_path", string_property("Optional workspace-relative output path; required for binary formats and requires durable approvals.")),
                ],
                &["source_path", "target_format"],
            ),
        ),
        mcp_tool_definition(
            "image_ocr",
            "Extract text from a workspace image through local tesseract.",
            mcp_schema(
                vec![("path", string_property("Workspace-relative image path."))],
                &["path"],
            ),
        ),
        mcp_tool_definition(
            "git_status",
            "Show concise git status for the workspace.",
            mcp_schema(
                vec![
                    (
                        "cwd",
                        string_property("Git working directory, default workspace root."),
                    ),
                    (
                        "path",
                        string_property("Optional pathspec to scope status."),
                    ),
                ],
                &[],
            ),
        ),
        mcp_tool_definition(
            "git_diff",
            "Show the current git working-tree diff.",
            mcp_schema(
                vec![
                    (
                        "cwd",
                        string_property("Git working directory, default workspace root."),
                    ),
                    ("path", string_property("Optional pathspec to scope diff.")),
                    (
                        "cached",
                        string_property("Set true to show staged changes."),
                    ),
                    (
                        "unified",
                        number_property("Context lines to include, default 3."),
                    ),
                    ("max_chars", number_property("Maximum output characters.")),
                ],
                &[],
            ),
        ),
        mcp_tool_definition(
            "project_map",
            "Render a high-level project tree, summary, and key files.",
            mcp_schema(
                vec![
                    (
                        "path",
                        string_property("Project root, default workspace root."),
                    ),
                    (
                        "max_depth",
                        number_property("Maximum tree depth, default 3."),
                    ),
                    (
                        "limit",
                        number_property("Maximum tree entries, default 120."),
                    ),
                ],
                &[],
            ),
        ),
        mcp_tool_definition(
            "validate_data",
            "Validate JSON or TOML content from inline input or a workspace file.",
            mcp_schema(
                vec![
                    ("path", string_property("Optional file path to validate.")),
                    (
                        "content",
                        string_property("Optional inline content to validate."),
                    ),
                    (
                        "format",
                        string_property("Validation format: auto, json, or toml."),
                    ),
                ],
                &[],
            ),
        ),
        mcp_tool_definition(
            "git_log",
            "Read recent git history.",
            mcp_schema(
                vec![
                    (
                        "cwd",
                        string_property("Git working directory, default `.`."),
                    ),
                    ("ref", string_property("Optional git ref.")),
                    ("path", string_property("Optional path filter.")),
                    ("limit", number_property("Maximum commits, default 20.")),
                    ("max_chars", number_property("Maximum output characters.")),
                ],
                &[],
            ),
        ),
        mcp_tool_definition(
            "git_show",
            "Show one git commit or ref with patch.",
            mcp_schema(
                vec![
                    (
                        "cwd",
                        string_property("Git working directory, default `.`."),
                    ),
                    ("ref", string_property("Git ref, default HEAD.")),
                    ("path", string_property("Optional path filter.")),
                    ("max_chars", number_property("Maximum output characters.")),
                ],
                &[],
            ),
        ),
        mcp_tool_definition(
            "git_blame",
            "Read git blame for a file and line range.",
            mcp_schema(
                vec![
                    (
                        "cwd",
                        string_property("Git working directory, default `.`."),
                    ),
                    ("path", string_property("File to blame.")),
                    ("ref", string_property("Git ref, default HEAD.")),
                    ("line_start", number_property("Start line, default 1.")),
                    ("line_end", number_property("Optional end line.")),
                    (
                        "limit",
                        number_property("Line count if line_end is omitted."),
                    ),
                    ("max_chars", number_property("Maximum output characters.")),
                ],
                &["path"],
            ),
        ),
        mcp_tool_definition(
            "github_issue_context",
            "Read GitHub issue context through the gh CLI.",
            mcp_schema(
                vec![
                    ("number", string_property("Issue number or reference.")),
                    ("issue", string_property("Alias for number.")),
                    ("ref", string_property("Alias for number.")),
                    ("repo", string_property("Optional owner/repo for gh -R.")),
                    ("repository", string_property("Alias for repo.")),
                    (
                        "include_comments",
                        string_property("Set false to omit comments, default true."),
                    ),
                    ("max_chars", number_property("Maximum JSON characters.")),
                ],
                &["number"],
            ),
        ),
        mcp_tool_definition(
            "github_pr_context",
            "Read GitHub pull request context through the gh CLI.",
            mcp_schema(
                vec![
                    ("number", string_property("PR number or reference.")),
                    ("pr", string_property("Alias for number.")),
                    ("ref", string_property("Alias for number.")),
                    ("repo", string_property("Optional owner/repo for gh -R.")),
                    ("repository", string_property("Alias for repo.")),
                    (
                        "include_diff",
                        string_property("Set true to include a bounded patch diff."),
                    ),
                    ("max_chars", number_property("Maximum JSON characters.")),
                    (
                        "diff_max_chars",
                        number_property("Maximum diff characters."),
                    ),
                ],
                &["number"],
            ),
        ),
        mcp_tool_definition(
            "review",
            "Run deterministic local code review over a workspace file, git diff, or github_pr_context output, including PR review/status signals.",
            mcp_schema(
                vec![
                    ("target", string_property("Workspace file path, diff, staged, or github_pr_context.")),
                    ("kind", string_property("Optional source kind such as file or diff.")),
                    ("cwd", string_property("Workspace directory, default MCP workspace.")),
                    ("context", string_property("Inline github_pr_context output.")),
                    ("github_context", string_property("Alias for context.")),
                    ("pr_context", string_property("Alias for context.")),
                    ("staged", string_property("Set true to review staged diff.")),
                    ("max_chars", number_property("Maximum source characters.")),
                ],
                &["target"],
            ),
        ),
        mcp_tool_definition(
            "pr_review_comment_plan",
            "Create a read-only GitHub PR review comment plan from review JSON and optional github_pr_context output.",
            mcp_schema(
                vec![
                    ("review_output", string_property("JSON output from the review tool.")),
                    ("review_json", string_property("Alias for review_output.")),
                    ("review", string_property("Alias for review_output.")),
                    (
                        "github_context",
                        string_property("Optional github_pr_context output."),
                    ),
                    ("pr_context", string_property("Alias for github_context.")),
                    ("context", string_property("Alias for github_context.")),
                    ("number", string_property("Optional PR number.")),
                    ("pr", string_property("Alias for number.")),
                    ("repo", string_property("Optional owner/repo for GitHub.")),
                    ("repository", string_property("Alias for repo.")),
                    ("max_issues", number_property("Maximum findings to render.")),
                ],
                &["review_output"],
            ),
        ),
        mcp_tool_definition(
            "recall_archive",
            "Search durable runtime threads, turns, and items for prior context.",
            mcp_schema(
                vec![
                    ("query", string_property("Search query.")),
                    ("thread_id", string_property("Optional runtime thread id.")),
                    ("max_results", number_property("Maximum hits, default 3.")),
                ],
                &["query"],
            ),
        ),
        mcp_tool_definition(
            "tool_search_tool_regex",
            "Search the static DeepSeekCode tool catalog with a lightweight regex-like pattern.",
            mcp_schema(
                vec![
                    ("query", string_property("Regex-like search pattern.")),
                    ("limit", number_property("Maximum tool references.")),
                ],
                &["query"],
            ),
        ),
        mcp_tool_definition(
            "tool_search_tool_bm25",
            "Rank static DeepSeekCode tools by local term matching over names, descriptions, and schemas.",
            mcp_schema(
                vec![
                    ("query", string_property("Search terms.")),
                    ("limit", number_property("Maximum tool references.")),
                ],
                &["query"],
            ),
        ),
        mcp_tool_definition(
            "load_skill",
            "Load a configured TOML skill by name with policy, references, and suggested steps.",
            mcp_schema(
                vec![("name", string_property("Skill name to load."))],
                &["name"],
            ),
        ),
        mcp_tool_definition(
            "request_user_input",
            "Render a DeepSeek-TUI-style request for 1-3 user questions so an MCP/ACP client can ask and continue with the answer.",
            mcp_schema(
                vec![(
                    "questions",
                    string_property("JSON array of 1-3 question objects with header, id, question, and 2-3 options."),
                )],
                &["questions"],
            ),
        ),
        mcp_tool_definition(
            "notify",
            "Fire a single terminal attention signal and return a bounded confirmation.",
            mcp_schema(
                vec![
                    ("title", string_property("Notification title.")),
                    ("body", string_property("Optional notification body.")),
                ],
                &["title"],
            ),
        ),
        mcp_tool_definition(
            "exec_shell_list",
            "List in-process background shell jobs.",
            mcp_schema(Vec::new(), &[]),
        ),
        mcp_tool_definition(
            "exec_shell_show",
            "Show a background shell job snapshot.",
            mcp_schema(
                vec![
                    ("task_id", string_property("Background shell task id.")),
                    ("id", string_property("Alias for task_id.")),
                ],
                &["task_id"],
            ),
        ),
        mcp_tool_definition(
            "exec_shell_wait",
            "Wait for or poll a background exec_shell task and return incremental output.",
            mcp_schema(
                vec![
                    ("task_id", string_property("Background shell task id.")),
                    ("id", string_property("Alias for task_id.")),
                    ("wait", string_property("Set false to poll once.")),
                    ("timeout_ms", number_property("Maximum wait milliseconds.")),
                ],
                &["task_id"],
            ),
        ),
        mcp_tool_definition(
            "exec_wait",
            "Alias for exec_shell_wait.",
            mcp_schema(
                vec![
                    ("task_id", string_property("Background shell task id.")),
                    ("id", string_property("Alias for task_id.")),
                    ("wait", string_property("Set false to poll once.")),
                    ("timeout_ms", number_property("Maximum wait milliseconds.")),
                ],
                &["task_id"],
            ),
        ),
        mcp_tool_definition(
            "task_shell_wait",
            "DeepSeek-TUI-compatible wait/poll helper for task_shell_start jobs.",
            mcp_schema(
                vec![
                    ("task_id", string_property("Background shell task id.")),
                    ("id", string_property("Alias for task_id.")),
                    ("wait", string_property("Set false to poll once.")),
                    ("timeout_ms", number_property("Maximum wait milliseconds.")),
                    ("gate", string_property("Optional gate label.")),
                    ("command", string_property("Optional original command.")),
                ],
                &["task_id"],
            ),
        ),
        mcp_tool_definition(
            "rlm_chunk_plan",
            "Plan DeepSeek-TUI-style RLM chunks for a workspace file or inline content without running child agents.",
            mcp_schema(
                vec![
                    ("file_path", string_property("Workspace-relative file to chunk.")),
                    ("content", string_property("Inline content to chunk.")),
                    ("max_chars", number_property("Maximum chars per chunk.")),
                    ("overlap", number_property("Overlapping chars between chunks.")),
                    ("include_text", string_property("Set false for offset-only chunks.")),
                ],
                &[],
            ),
        ),
        mcp_tool_definition(
            "rlm_map_reduce_plan",
            "Plan a DeepSeek-TUI-style RLM map-reduce workflow without running child agents.",
            mcp_schema(
                vec![
                    ("task", string_property("Reduce objective.")),
                    ("question", string_property("Alias for task.")),
                    ("file_path", string_property("Workspace-relative file to chunk.")),
                    ("content", string_property("Inline content to chunk.")),
                    ("max_chars", number_property("Maximum chars per chunk.")),
                    ("overlap", number_property("Overlapping chars between chunks.")),
                    ("include_text", string_property("Set false for offset-only chunks.")),
                    ("map_limit", number_property("Maximum map tasks to render.")),
                    ("steps", string_property("Suggested child step budget.")),
                ],
                &[],
            ),
        ),
        mcp_tool_definition(
            "rlm_recursive_plan",
            "Plan a multi-round DeepSeek-TUI-style RLM recursive map/reduce workflow without running child agents.",
            mcp_schema(
                vec![
                    ("task", string_property("Overall recursive reduce objective.")),
                    ("question", string_property("Alias for task.")),
                    ("file_path", string_property("Workspace-relative file to chunk.")),
                    ("content", string_property("Inline content to chunk.")),
                    ("max_chars", number_property("Maximum chars per chunk.")),
                    ("overlap", number_property("Overlapping chars between chunks.")),
                    ("include_text", string_property("Set false for offset-only chunks.")),
                    ("map_limit", number_property("Maximum map tasks to render.")),
                    ("fan_in", number_property("Maximum inputs per recursive reduce group.")),
                    ("steps", string_property("Suggested child step budget.")),
                ],
                &[],
            ),
        ),
        mcp_tool_definition(
            "rlm_python",
            "Run a short restricted Python helper for pure RLM computation. Imports, files, network, subprocess, and OS access are blocked.",
            mcp_schema(
                vec![
                    ("code", string_property("Restricted Python code.")),
                    ("context", string_property("Optional context string.")),
                    ("question", string_property("Optional question string.")),
                    ("timeout_ms", number_property("Timeout milliseconds, clamped.")),
                ],
                &["code"],
            ),
        ),
        mcp_tool_definition(
            "rlm_python_sessions",
            "List or inspect persisted rlm_python_session JSON state without running Python.",
            mcp_schema(
                vec![
                    ("session_id", string_property("Optional session id to inspect.")),
                    ("limit", number_property("Maximum sessions to list.")),
                ],
                &[],
            ),
        ),
        mcp_tool_definition(
            "diagnostics",
            "Run workspace or path-scoped diagnostics.",
            mcp_schema(
                vec![
                    ("cwd", string_property("Workspace directory, default `.`.")),
                    (
                        "paths",
                        string_property("Comma, semicolon, or newline separated paths."),
                    ),
                ],
                &[],
            ),
        ),
    ];
    if mcp_side_effect_tools_enabled(state) {
        tools.push(mcp_tool_definition(
            "exec_shell",
            "DeepSeek-TUI-compatible shell execution. Use background=true for long-running commands. Requires trusted side effects or durable runtime approvals.",
            mcp_schema(
                vec![
                    ("command", string_property("Allowlisted shell command.")),
                    ("cwd", string_property("Working directory, default workspace root.")),
                    ("background", string_property("Set true to run in background.")),
                    ("stdin", string_property("Optional initial stdin.")),
                    ("input", string_property("Alias for stdin.")),
                    ("data", string_property("Alias for stdin.")),
                ],
                &["command"],
            ),
        ));
        tools.push(mcp_tool_definition(
            "task_shell_start",
            "DeepSeek-TUI-compatible background shell starter. Requires trusted side effects or durable runtime approvals.",
            mcp_schema(
                vec![
                    ("command", string_property("Allowlisted shell command.")),
                    ("cwd", string_property("Working directory, default workspace root.")),
                    ("stdin", string_property("Optional initial stdin.")),
                    ("input", string_property("Alias for stdin.")),
                    ("timeout_ms", number_property("Compatibility timeout metadata.")),
                    ("tty", string_property("Accepted compatibility flag.")),
                ],
                &["command"],
            ),
        ));
        tools.push(mcp_tool_definition(
            "exec_shell_interact",
            "Send stdin to a running background shell job. Requires trusted side effects or durable runtime approvals.",
            mcp_schema(
                vec![
                    ("task_id", string_property("Background shell task id.")),
                    ("id", string_property("Alias for task_id.")),
                    ("input", string_property("Input to send.")),
                    ("stdin", string_property("Alias for input.")),
                    ("data", string_property("Alias for input.")),
                    ("close_stdin", string_property("Set true to close stdin.")),
                    ("timeout_ms", number_property("Wait milliseconds after input.")),
                ],
                &["task_id"],
            ),
        ));
        tools.push(mcp_tool_definition(
            "exec_interact",
            "Alias for exec_shell_interact. Requires trusted side effects or durable runtime approvals.",
            mcp_schema(
                vec![
                    ("task_id", string_property("Background shell task id.")),
                    ("id", string_property("Alias for task_id.")),
                    ("input", string_property("Input to send.")),
                    ("stdin", string_property("Alias for input.")),
                    ("data", string_property("Alias for input.")),
                    ("close_stdin", string_property("Set true to close stdin.")),
                    ("timeout_ms", number_property("Wait milliseconds after input.")),
                ],
                &["task_id"],
            ),
        ));
        tools.push(mcp_tool_definition(
            "exec_shell_cancel",
            "Cancel one or all running background shell jobs. Requires trusted side effects or durable runtime approvals.",
            mcp_schema(
                vec![
                    ("task_id", string_property("Background shell task id.")),
                    ("id", string_property("Alias for task_id.")),
                    ("all", string_property("Set true to cancel all running jobs.")),
                ],
                &[],
            ),
        ));
        tools.push(mcp_tool_definition(
            "rlm_python_session",
            "Run restricted Python with persisted JSON state. Requires trusted side effects or durable runtime approvals because it writes .dscode/rlm-python state.",
            mcp_schema(
                vec![
                    ("session_id", string_property("RLM Python session id.")),
                    ("code", string_property("Restricted Python code.")),
                    ("context", string_property("Optional context string.")),
                    ("question", string_property("Optional question string.")),
                    ("reset", string_property("Set true to clear state before running.")),
                    ("persistent", string_property("Set true to reuse a process.")),
                    ("timeout_ms", number_property("Timeout milliseconds, clamped.")),
                ],
                &["session_id", "code"],
            ),
        ));
        tools.push(mcp_tool_definition(
            "image_analyze",
            "Analyze a workspace image through an OpenAI-compatible vision model. Requires trusted side effects or durable MCP approval because it can spend model tokens and use networked APIs.",
            mcp_schema(
                vec![
                    ("image_path", string_property("Workspace-relative image path.")),
                    ("path", string_property("Alias for image_path.")),
                    ("prompt", string_property("Optional analysis prompt.")),
                    ("model", string_property("Optional vision model override.")),
                    ("base_url", string_property("Optional OpenAI-compatible base URL override.")),
                    ("api_key_env", string_property("Optional API key environment variable override.")),
                    ("max_tokens", number_property("Maximum response tokens.")),
                ],
                &[],
            ),
        ));
        for name in ["rlm", "rlm_query", "llm_query", "rlm_process"] {
            tools.push(mcp_tool_definition(
                name,
                "Run a bounded model-backed RLM child analysis. Requires trusted side effects or durable MCP approval because it can spend model tokens and use networked model APIs.",
                mcp_schema(
                    vec![
                        ("context", string_property("Context for lightweight RLM analysis.")),
                        ("question", string_property("Question for context mode.")),
                        ("task", string_property("Long-input RLM objective.")),
                        ("file_path", string_property("Workspace-relative long-input file.")),
                        ("content", string_property("Inline long input.")),
                        ("strategy", string_property("Optional strategy label.")),
                        ("steps", string_property("Child step budget.")),
                        ("max_depth", string_property("Alias for steps.")),
                    ],
                    &[],
                ),
            ));
        }
        for name in ["rlm_batch", "rlm_query_batched", "llm_query_batched"] {
            tools.push(mcp_tool_definition(
                name,
                "Run batched bounded model-backed RLM child analyses. Requires trusted side effects or durable MCP approval because it can spend model tokens and use networked model APIs.",
                mcp_schema(
                    vec![
                        ("context", string_property("Shared context for all questions.")),
                        ("questions", string_property("JSON array of questions.")),
                        ("strategy", string_property("Optional strategy label.")),
                        ("steps", string_property("Child step budget.")),
                    ],
                    &["context", "questions"],
                ),
            ));
        }
        tools.push(mcp_tool_definition(
            "run_tests",
            "Run a supported test command in the workspace. Requires trusted DSCODE_MCP_ENABLE_SIDE_EFFECTS=1 or durable runtime approvals.",
            mcp_schema(
                vec![
                    ("cwd", string_property("Working directory, default workspace root.")),
                    ("command", string_property("Optional supported test command.")),
                    ("args", string_property("Optional safe extra arguments.")),
                    (
                        "all_features",
                        string_property("Set true to add --all-features for cargo test."),
                    ),
                ],
                &[],
            ),
        ));
        tools.push(mcp_tool_definition(
            "run_shell",
            "Run an allowlisted shell command in the workspace. Requires trusted DSCODE_MCP_ENABLE_SIDE_EFFECTS=1 or durable runtime approvals.",
            mcp_schema(
                vec![
                    ("command", string_property("Allowlisted shell command to run.")),
                    ("cwd", string_property("Working directory, default `.`.")),
                ],
                &["command"],
            ),
        ));
    }
    if mcp_write_tools_enabled(state) {
        tools.push(mcp_tool_definition(
            "apply_patch",
            "Apply a unified diff in the workspace. Requires durable runtime approvals.",
            mcp_schema(
                vec![
                    ("patch", string_property("Unified diff content to apply.")),
                    (
                        "cwd",
                        string_property("Workspace directory, default MCP workspace."),
                    ),
                ],
                &["patch"],
            ),
        ));
        tools.push(mcp_tool_definition(
            "write_file",
            "Write UTF-8 text to a relative path under the MCP workspace. Requires durable runtime approvals.",
            mcp_schema(
                vec![
                    (
                        "path",
                        string_property("Relative workspace path to create or overwrite."),
                    ),
                    ("content", string_property("Complete UTF-8 file content.")),
                ],
                &["path", "content"],
            ),
        ));
        tools.push(mcp_tool_definition(
            "note",
            "Append a persistent maintainer or agent note to the configured notes file. Requires durable runtime approvals.",
            mcp_schema(
                vec![
                    ("content", string_property("Note content to append.")),
                    ("note", string_property("Alias for content.")),
                ],
                &[],
            ),
        ));
        if state.config.memory.enabled {
            tools.push(mcp_tool_definition(
                "remember",
                "Append a durable user-memory note to the configured memory file. Requires durable runtime approvals and enabled memory.",
                mcp_schema(
                    vec![
                        ("note", string_property("Single-sentence durable note to remember.")),
                        ("content", string_property("Alias for note.")),
                    ],
                    &[],
                ),
            ));
        }
        tools.push(mcp_tool_definition(
            "edit_file",
            "Replace exact text in one UTF-8 file under the MCP workspace. Requires durable runtime approvals.",
            mcp_schema(
                vec![
                    (
                        "path",
                        string_property("Relative workspace file path to edit."),
                    ),
                    ("search", string_property("Exact text to find.")),
                    ("replace", string_property("Replacement text.")),
                ],
                &["path", "search", "replace"],
            ),
        ));
        tools.push(mcp_tool_definition(
            "delete_file",
            "Delete one regular file at a relative path under the MCP workspace. Requires durable runtime approvals.",
            mcp_schema(
                vec![(
                    "path",
                    string_property("Relative workspace file path to delete."),
                )],
                &["path"],
            ),
        ));
        tools.push(mcp_tool_definition(
            "copy_file",
            "Copy one regular file between relative paths under the MCP workspace. Requires durable runtime approvals.",
            mcp_schema(
                vec![
                    (
                        "source_path",
                        string_property("Relative workspace file path to copy."),
                    ),
                    (
                        "destination_path",
                        string_property("Relative workspace destination path."),
                    ),
                ],
                &["source_path", "destination_path"],
            ),
        ));
        tools.push(mcp_tool_definition(
            "move_file",
            "Move one regular file between relative paths under the MCP workspace. Requires durable runtime approvals.",
            mcp_schema(
                vec![
                    (
                        "source_path",
                        string_property("Relative workspace file path to move."),
                    ),
                    (
                        "destination_path",
                        string_property("Relative workspace destination path."),
                    ),
                ],
                &["source_path", "destination_path"],
            ),
        ));
        tools.push(mcp_tool_definition(
            "revert_turn",
            "Restore workspace files to a rollback snapshot or recent runtime turn. Requires durable runtime approvals.",
            mcp_schema(
                vec![
                    (
                        "turn_offset",
                        number_property("1-based recent runtime turn snapshot offset."),
                    ),
                    ("offset", number_property("Alias for turn_offset.")),
                    ("turn_id", string_property("Runtime assistant turn id.")),
                    (
                        "thread_id",
                        string_property("Optional runtime thread id used with turn_offset."),
                    ),
                    ("snapshot_id", string_property("Rollback snapshot id.")),
                    ("checkpoint_id", string_property("Alias for snapshot_id.")),
                    ("id", string_property("Alias for snapshot_id or turn id.")),
                    ("dry_run", string_property("Set true to preview only.")),
                    ("apply", string_property("Set false to preview only.")),
                ],
                &[],
            ),
        ));
        tools.push(mcp_tool_definition(
            "github_comment",
            "Post an evidence-backed GitHub issue or PR comment through the gh CLI. Requires durable runtime approvals.",
            mcp_schema(
                vec![
                    ("target", string_property("Comment target: issue or pr.")),
                    ("number", string_property("Issue or PR number.")),
                    ("body", string_property("Comment body to post.")),
                    (
                        "evidence",
                        string_property("JSON object with supporting evidence."),
                    ),
                    ("repo", string_property("Optional owner/repo for gh -R.")),
                    ("repository", string_property("Alias for repo.")),
                    ("dry_run", string_property("Set true to validate only.")),
                ],
                &["target", "number", "body", "evidence"],
            ),
        ));
        tools.push(mcp_tool_definition(
            "github_close_issue",
            "Close a GitHub issue as completed through the gh CLI after structured evidence. Requires durable runtime approvals.",
            mcp_schema(
                vec![
                    ("number", string_property("Issue number.")),
                    (
                        "acceptance_criteria",
                        string_property("JSON array of satisfied acceptance criteria."),
                    ),
                    (
                        "evidence",
                        string_property("JSON object with files_changed, tests_run, and final_status."),
                    ),
                    ("comment", string_property("Optional closing comment.")),
                    (
                        "allow_dirty",
                        string_property("Set true to allow a dirty local worktree."),
                    ),
                    ("cwd", string_property("Workspace directory for git status.")),
                    ("repo", string_property("Optional owner/repo for gh -R.")),
                    ("repository", string_property("Alias for repo.")),
                    ("dry_run", string_property("Set true to validate only.")),
                ],
                &["number", "acceptance_criteria", "evidence"],
            ),
        ));
        tools.push(mcp_tool_definition(
            "runtime_create_task",
            "Create a durable runtime task. Requires durable runtime approvals.",
            mcp_schema(
                vec![
                    ("summary", string_property("Task summary or prompt.")),
                    ("prompt", string_property("Alias for summary.")),
                    ("kind", string_property("Task kind, default agent.")),
                    ("status", string_property("Task status, default pending.")),
                    (
                        "session_id",
                        string_property("Optional runtime session id."),
                    ),
                    ("thread_id", string_property("Optional runtime thread id.")),
                    (
                        "parent_task_id",
                        string_property("Optional parent runtime task id."),
                    ),
                ],
                &[],
            ),
        ));
        tools.push(mcp_tool_definition(
            "runtime_cancel_task",
            "Cancel a durable runtime task and append a cancel event when linked to a thread. Requires durable runtime approvals.",
            mcp_schema(
                vec![
                    ("task_id", string_property("Runtime task id.")),
                    ("id", string_property("Alias for task_id.")),
                    ("reason", string_property("Optional cancellation reason.")),
                ],
                &["task_id"],
            ),
        ));
        tools.push(mcp_tool_definition(
            "runtime_create_automation",
            "Create a durable runtime automation. Requires durable runtime approvals.",
            mcp_schema(
                vec![
                    ("name", string_property("Automation name.")),
                    (
                        "prompt",
                        string_property("Prompt to enqueue when triggered."),
                    ),
                    (
                        "rrule",
                        string_property("Schedule expression; alias for schedule."),
                    ),
                    ("schedule", string_property("Schedule expression.")),
                    (
                        "status",
                        string_property("Automation status, default active."),
                    ),
                    ("paused", string_property("Set true to create paused.")),
                    (
                        "session_id",
                        string_property("Optional runtime session id."),
                    ),
                    ("thread_id", string_property("Optional runtime thread id.")),
                    (
                        "last_run_at",
                        string_property("Optional last-run timestamp."),
                    ),
                    (
                        "next_run_at",
                        string_property("Optional next-run timestamp."),
                    ),
                ],
                &["name", "prompt"],
            ),
        ));
        tools.push(mcp_tool_definition(
            "runtime_update_automation",
            "Update durable runtime automation metadata. Requires durable runtime approvals.",
            mcp_schema(
                vec![
                    ("automation_id", string_property("Runtime automation id.")),
                    ("id", string_property("Alias for automation_id.")),
                    ("name", string_property("Optional replacement name.")),
                    ("prompt", string_property("Optional replacement prompt.")),
                    (
                        "rrule",
                        string_property("Optional replacement schedule alias."),
                    ),
                    (
                        "schedule",
                        string_property("Optional replacement schedule."),
                    ),
                    ("status", string_property("Optional replacement status.")),
                    (
                        "paused",
                        string_property("Set true/false to pause or resume."),
                    ),
                    (
                        "next_run_at",
                        string_property("Optional next-run timestamp."),
                    ),
                ],
                &["automation_id"],
            ),
        ));
        tools.push(mcp_tool_definition(
            "runtime_pause_automation",
            "Pause a durable runtime automation. Requires durable runtime approvals.",
            mcp_schema(
                vec![
                    ("automation_id", string_property("Runtime automation id.")),
                    ("id", string_property("Alias for automation_id.")),
                ],
                &["automation_id"],
            ),
        ));
        tools.push(mcp_tool_definition(
            "runtime_resume_automation",
            "Resume a durable runtime automation. Requires durable runtime approvals.",
            mcp_schema(
                vec![
                    ("automation_id", string_property("Runtime automation id.")),
                    ("id", string_property("Alias for automation_id.")),
                ],
                &["automation_id"],
            ),
        ));
        tools.push(mcp_tool_definition(
            "runtime_delete_automation",
            "Delete a durable runtime automation. Requires durable runtime approvals.",
            mcp_schema(
                vec![
                    ("automation_id", string_property("Runtime automation id.")),
                    ("id", string_property("Alias for automation_id.")),
                ],
                &["automation_id"],
            ),
        ));
        tools.push(mcp_tool_definition(
            "runtime_trigger_automation",
            "Trigger a durable runtime automation into a pending task. Requires durable runtime approvals.",
            mcp_schema(
                vec![
                    ("automation_id", string_property("Runtime automation id.")),
                    ("id", string_property("Alias for automation_id.")),
                    ("prompt", string_property("Optional prompt override.")),
                    ("prompt_override", string_property("Alias for prompt.")),
                ],
                &["automation_id"],
            ),
        ));
        tools.push(mcp_tool_definition(
            "runtime_spawn_agent",
            "Create a durable runtime thread and pending sub-agent task. Requires durable runtime approvals.",
            mcp_schema(
                vec![
                    ("prompt", string_property("Sub-agent prompt.")),
                    ("message", string_property("Alias for prompt.")),
                    ("objective", string_property("Alias for prompt.")),
                    ("task", string_property("Alias for prompt.")),
                    ("cwd", string_property("Workspace directory.")),
                    ("workspace", string_property("Alias for cwd.")),
                    ("model", string_property("Runtime thread model.")),
                    ("mode", string_property("Runtime thread mode, default agent.")),
                    ("title", string_property("Optional runtime thread title.")),
                    ("thread_id", string_property("Existing runtime thread id.")),
                    (
                        "parent_task_id",
                        string_property("Optional parent runtime task id."),
                    ),
                ],
                &["prompt"],
            ),
        ));
        tools.push(mcp_tool_definition(
            "runtime_cancel_agent",
            "Cancel a durable runtime sub-agent task. Requires durable runtime approvals.",
            mcp_schema(
                vec![
                    ("agent_id", string_property("Runtime sub-agent task id.")),
                    ("id", string_property("Alias for agent_id.")),
                ],
                &["agent_id"],
            ),
        ));
        tools.push(mcp_tool_definition(
            "runtime_close_agent",
            "Close a durable runtime sub-agent task by cancelling non-terminal tasks. Requires durable runtime approvals.",
            mcp_schema(
                vec![
                    ("agent_id", string_property("Runtime sub-agent task id.")),
                    ("id", string_property("Alias for agent_id.")),
                ],
                &["agent_id"],
            ),
        ));
        tools.push(mcp_tool_definition(
            "runtime_resume_agent",
            "Resume or fork a durable runtime sub-agent task. Requires durable runtime approvals.",
            mcp_schema(
                vec![
                    ("agent_id", string_property("Runtime sub-agent task id.")),
                    ("id", string_property("Alias for agent_id.")),
                    ("prompt", string_property("Optional resumed prompt.")),
                    ("message", string_property("Alias for prompt.")),
                ],
                &["agent_id"],
            ),
        ));
        tools.push(mcp_tool_definition(
            "runtime_send_agent_input",
            "Append user input to a runtime sub-agent thread and queue a follow-up subagent_input task. Requires durable runtime approvals.",
            mcp_schema(
                vec![
                    ("agent_id", string_property("Runtime sub-agent task id.")),
                    ("id", string_property("Alias for agent_id.")),
                    ("message", string_property("User input message.")),
                    ("input", string_property("Alias for message.")),
                    ("prompt", string_property("Alias for message.")),
                ],
                &["agent_id", "message"],
            ),
        ));
    }
    tools.extend([
        mcp_tool_definition(
            "runtime_health",
            "Return DeepSeekCode MCP server health metadata.",
            mcp_schema(Vec::new(), &[]),
        ),
        mcp_tool_definition(
            "runtime_list_sessions",
            "List durable runtime sessions.",
            mcp_schema(vec![("limit", number_property("Maximum sessions."))], &[]),
        ),
        mcp_tool_definition(
            "runtime_list_threads",
            "List durable runtime threads.",
            mcp_schema(vec![("limit", number_property("Maximum threads."))], &[]),
        ),
        mcp_tool_definition(
            "runtime_read_thread",
            "Read one durable runtime thread with turns and items.",
            mcp_schema(
                vec![("thread_id", string_property("Runtime thread id."))],
                &["thread_id"],
            ),
        ),
        mcp_tool_definition(
            "runtime_list_tasks",
            "List durable runtime tasks.",
            mcp_schema(vec![("limit", number_property("Maximum tasks."))], &[]),
        ),
        mcp_tool_definition(
            "runtime_read_task",
            "Read one durable runtime task.",
            mcp_schema(
                vec![("task_id", string_property("Runtime task id."))],
                &["task_id"],
            ),
        ),
        mcp_tool_definition(
            "runtime_list_agents",
            "List durable runtime sub-agent tasks.",
            mcp_schema(vec![("limit", number_property("Maximum sub-agents."))], &[]),
        ),
        mcp_tool_definition(
            "runtime_agent_result",
            "Read one durable runtime sub-agent snapshot.",
            mcp_schema(
                vec![
                    ("agent_id", string_property("Runtime sub-agent task id.")),
                    ("id", string_property("Alias for agent_id.")),
                ],
                &["agent_id"],
            ),
        ),
    ]);
    tools
}

fn mcp_tool_definition(name: &str, description: &str, input_schema: JsonValue) -> JsonValue {
    object([
        ("name", JsonValue::String(name.to_string())),
        ("description", JsonValue::String(description.to_string())),
        ("inputSchema", input_schema),
    ])
}

fn mcp_schema(properties: Vec<(&str, JsonValue)>, required: &[&str]) -> JsonValue {
    let mut property_map = BTreeMap::new();
    for (name, property) in properties {
        property_map.insert(name.to_string(), property);
    }
    object([
        ("type", JsonValue::String("object".to_string())),
        ("properties", JsonValue::Object(property_map)),
        (
            "required",
            JsonValue::Array(
                required
                    .iter()
                    .map(|field| JsonValue::String((*field).to_string()))
                    .collect(),
            ),
        ),
    ])
}

fn string_property(description: &str) -> JsonValue {
    object([
        ("type", JsonValue::String("string".to_string())),
        ("description", JsonValue::String(description.to_string())),
    ])
}

fn number_property(description: &str) -> JsonValue {
    object([
        ("type", JsonValue::String("number".to_string())),
        ("description", JsonValue::String(description.to_string())),
    ])
}

struct AcpStdioState {
    config: AppConfig,
    store: RuntimeStore,
    rollback: RollbackStore,
    default_cwd: PathBuf,
    approval_poll_interval: Duration,
    approval_max_polls: Option<usize>,
    sessions: BTreeMap<String, AcpSession>,
    next_session: u64,
}

struct AcpSession {
    cwd: PathBuf,
    runtime_session_id: Option<String>,
    runtime_thread_id: Option<String>,
}

struct AcpToolCallOutcome {
    result: JsonValue,
    text: String,
    is_error: bool,
    turn_id: Option<String>,
    call_item_id: Option<String>,
    result_item_id: Option<String>,
}

enum AcpDispatch {
    Responses(Vec<JsonValue>),
    Shutdown(Vec<JsonValue>),
}

fn run_acp_stdio(args: ServeAcpArgs) -> AppResult<()> {
    let workspace = args
        .workspace
        .map(PathBuf::from)
        .unwrap_or(std::env::current_dir()?);
    let config = {
        let cwd_guard = CwdGuard::enter(&workspace)?;
        let config = load_or_default()?;
        cwd_guard.restore()?;
        config
    };
    let mut state = AcpStdioState {
        store: RuntimeStore::new(PathBuf::from(&config.workspace.config_dir).join("runtime")),
        rollback: RollbackStore::new(PathBuf::from(&config.workspace.config_dir).join("rollback")),
        config,
        default_cwd: workspace,
        approval_poll_interval: Duration::from_millis(250),
        approval_max_polls: None,
        sessions: BTreeMap::new(),
        next_session: 0,
    };
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();

    for line in BufReader::new(stdin.lock()).lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        if acp_try_streaming_tool_call(&line, &mut state, &mut stdout)? {
            continue;
        }
        match acp_dispatch_for_message(&line, &mut state) {
            AcpDispatch::Responses(responses) => {
                write_json_responses(&mut stdout, responses)?;
            }
            AcpDispatch::Shutdown(responses) => {
                write_json_responses(&mut stdout, responses)?;
                break;
            }
        }
    }

    Ok(())
}

fn write_json_responses<W: Write>(writer: &mut W, responses: Vec<JsonValue>) -> AppResult<()> {
    for response in responses {
        writer.write_all(json_value_to_string(&response).as_bytes())?;
        writer.write_all(b"\n")?;
        writer.flush()?;
    }
    Ok(())
}

fn acp_try_streaming_tool_call<W: Write>(
    message: &str,
    state: &mut AcpStdioState,
    writer: &mut W,
) -> AppResult<bool> {
    let Ok(root) = parse_root_object(message) else {
        return Ok(false);
    };
    if root.get("jsonrpc").and_then(json_as_string) != Some("2.0") {
        return Ok(false);
    }
    if root.get("method").and_then(json_as_string) != Some("session/tools/call") {
        return Ok(false);
    }
    let params = root
        .get("params")
        .and_then(json_as_object)
        .cloned()
        .unwrap_or_default();
    let Some(name) = params.get("name").and_then(json_as_string) else {
        return Ok(false);
    };
    if !matches!(name, "exec_shell" | "task_shell_start") {
        return Ok(false);
    }
    let arguments = params.get("arguments").and_then(json_as_object);
    if !acp_json_truthy(arguments.and_then(|args| args.get("stream")))
        && !acp_json_truthy(arguments.and_then(|args| args.get("follow")))
    {
        return Ok(false);
    }

    let response_id = root.get("id").cloned().unwrap_or(JsonValue::Null);
    match acp_stream_shell_tool_call(response_id.clone(), &params, state, writer) {
        Ok(()) => Ok(true),
        Err((code, message)) => {
            write_json_responses(
                writer,
                vec![jsonrpc_error_response(response_id, code, &message)],
            )?;
            Ok(true)
        }
    }
}

fn acp_json_truthy(value: Option<&JsonValue>) -> bool {
    match value {
        Some(JsonValue::Bool(value)) => *value,
        Some(JsonValue::Number(value)) => value != "0",
        Some(JsonValue::String(value)) => truthy_str(value),
        _ => false,
    }
}

fn acp_dispatch_for_message(message: &str, state: &mut AcpStdioState) -> AcpDispatch {
    let root = match parse_root_object(message) {
        Ok(root) => root,
        Err(error) => {
            return AcpDispatch::Responses(vec![jsonrpc_error_response(
                JsonValue::Null,
                -32700,
                &format!("invalid json: {error}"),
            )]);
        }
    };
    if root.get("jsonrpc").and_then(json_as_string) != Some("2.0") {
        let id = root.get("id").cloned().unwrap_or(JsonValue::Null);
        return AcpDispatch::Responses(vec![jsonrpc_error_response(
            id,
            -32600,
            "jsonrpc version must be 2.0",
        )]);
    }
    let id = root.get("id").cloned();
    let Some(method) = root.get("method").and_then(json_as_string) else {
        return AcpDispatch::Responses(vec![jsonrpc_error_response(
            id.unwrap_or(JsonValue::Null),
            -32600,
            "missing method",
        )]);
    };
    let params = root
        .get("params")
        .and_then(json_as_object)
        .cloned()
        .unwrap_or_default();

    let Some(response_id) = id else {
        return AcpDispatch::Responses(Vec::new());
    };
    match acp_handle_request(method, &params, state) {
        Ok(AcpDispatch::Responses(mut responses)) => {
            for response in &mut responses {
                if acp_response_needs_id(response) {
                    add_jsonrpc_id(response, response_id.clone());
                }
            }
            AcpDispatch::Responses(responses)
        }
        Ok(AcpDispatch::Shutdown(mut responses)) => {
            for response in &mut responses {
                if acp_response_needs_id(response) {
                    add_jsonrpc_id(response, response_id.clone());
                }
            }
            AcpDispatch::Shutdown(responses)
        }
        Err(error) => {
            AcpDispatch::Responses(vec![jsonrpc_error_response(response_id, error.0, &error.1)])
        }
    }
}

fn acp_handle_request(
    method: &str,
    params: &BTreeMap<String, JsonValue>,
    state: &mut AcpStdioState,
) -> Result<AcpDispatch, (i64, String)> {
    match method {
        "initialize" => Ok(AcpDispatch::Responses(vec![
            jsonrpc_success_response_without_id(acp_initialize_result(
                params.get("protocolVersion").and_then(json_as_u64),
            )),
        ])),
        "session/new" => Ok(AcpDispatch::Responses(vec![
            jsonrpc_success_response_without_id(acp_new_session(params, state)?),
        ])),
        "session/list" => Ok(AcpDispatch::Responses(vec![
            jsonrpc_success_response_without_id(acp_list_sessions(params, state)?),
        ])),
        "session/load" => Ok(AcpDispatch::Responses(vec![
            jsonrpc_success_response_without_id(acp_load_session(params, state)?),
        ])),
        "session/checkpoints" => Ok(AcpDispatch::Responses(vec![
            jsonrpc_success_response_without_id(acp_list_checkpoints(params, state)?),
        ])),
        "session/checkpoint/read" => Ok(AcpDispatch::Responses(vec![
            jsonrpc_success_response_without_id(acp_read_checkpoint(params, state)?),
        ])),
        "session/checkpoint/restore" => Ok(AcpDispatch::Responses(vec![
            jsonrpc_success_response_without_id(acp_restore_checkpoint(params, state)?),
        ])),
        "session/tools/list" => Ok(AcpDispatch::Responses(vec![
            jsonrpc_success_response_without_id(acp_session_tools_list(params, state)?),
        ])),
        "session/tools/call" => Ok(AcpDispatch::Responses(acp_session_tools_call_responses(
            params, state,
        )?)),
        "session/prompt" => {
            let (session_id, output) = acp_prompt(params, state)?;
            let mut responses = Vec::new();
            if !output.is_empty() {
                responses.push(acp_session_update(&session_id, output));
            }
            responses.push(jsonrpc_success_response_without_id(object([(
                "stopReason",
                JsonValue::String("end_turn".to_string()),
            )])));
            Ok(AcpDispatch::Responses(responses))
        }
        "session/cancel" => Ok(AcpDispatch::Responses(vec![
            jsonrpc_success_response_without_id(JsonValue::Null),
        ])),
        "shutdown" => Ok(AcpDispatch::Shutdown(vec![
            jsonrpc_success_response_without_id(JsonValue::Null),
        ])),
        other => Err((-32601, format!("method not found: {other}"))),
    }
}

fn acp_initialize_result(client_protocol_version: Option<u64>) -> JsonValue {
    object([
        (
            "protocolVersion",
            JsonValue::Number(
                client_protocol_version
                    .map(|version| version.min(ACP_PROTOCOL_VERSION))
                    .unwrap_or(ACP_PROTOCOL_VERSION)
                    .to_string(),
            ),
        ),
        (
            "agentCapabilities",
            object([
                ("loadSession", JsonValue::Bool(true)),
                (
                    "promptCapabilities",
                    object([
                        ("image", JsonValue::Bool(false)),
                        ("audio", JsonValue::Bool(false)),
                        ("embeddedContext", JsonValue::Bool(true)),
                    ]),
                ),
                (
                    "mcpCapabilities",
                    object([
                        ("http", JsonValue::Bool(false)),
                        ("sse", JsonValue::Bool(false)),
                    ]),
                ),
                (
                    "sessionCapabilities",
                    object([
                        (
                            "checkpoints",
                            object([
                                ("readOnly", JsonValue::Bool(false)),
                                ("restore", JsonValue::Bool(true)),
                                ("apply", JsonValue::Bool(true)),
                            ]),
                        ),
                        (
                            "tools",
                            object([
                                ("readOnly", JsonValue::Bool(true)),
                                ("permissioned", JsonValue::Bool(true)),
                            ]),
                        ),
                    ]),
                ),
            ]),
        ),
        (
            "agentInfo",
            object([
                ("name", JsonValue::String("deepseek-code".to_string())),
                ("title", JsonValue::String("DeepSeekCode".to_string())),
                (
                    "version",
                    JsonValue::String(env!("CARGO_PKG_VERSION").to_string()),
                ),
            ]),
        ),
        ("authMethods", JsonValue::Array(Vec::new())),
    ])
}

fn acp_new_session(
    params: &BTreeMap<String, JsonValue>,
    state: &mut AcpStdioState,
) -> Result<JsonValue, (i64, String)> {
    let cwd = params
        .get("cwd")
        .and_then(json_as_string)
        .map(PathBuf::from)
        .unwrap_or_else(|| state.default_cwd.clone());
    state.next_session = state.next_session.saturating_add(1);
    let session_id = format!("deepseekcode-{}-{}", std::process::id(), state.next_session);
    state.sessions.insert(
        session_id.clone(),
        AcpSession {
            cwd,
            runtime_session_id: None,
            runtime_thread_id: None,
        },
    );
    Ok(object([(
        "sessionId",
        JsonValue::String(session_id.to_string()),
    )]))
}

fn acp_list_sessions(
    params: &BTreeMap<String, JsonValue>,
    state: &AcpStdioState,
) -> Result<JsonValue, (i64, String)> {
    let limit = params
        .get("limit")
        .and_then(json_as_u64)
        .map(|limit| limit.clamp(1, 100) as usize)
        .unwrap_or(20);
    let sessions = state
        .store
        .list_sessions(limit)
        .map_err(|error| (-32603, error.to_string()))?
        .iter()
        .map(acp_session_summary)
        .collect::<Vec<_>>();
    Ok(object([
        ("sessions", JsonValue::Array(sessions)),
        ("nextCursor", JsonValue::Null),
    ]))
}

fn acp_load_session(
    params: &BTreeMap<String, JsonValue>,
    state: &mut AcpStdioState,
) -> Result<JsonValue, (i64, String)> {
    let runtime_session_id = params
        .get("sessionId")
        .and_then(json_as_string)
        .filter(|id| !id.trim().is_empty())
        .ok_or_else(|| (-32602, "sessionId is required".to_string()))?;
    let runtime_session = state
        .store
        .load_session(runtime_session_id)
        .map_err(|error| (-32602, error.to_string()))?;
    let thread = match params
        .get("threadId")
        .and_then(json_as_string)
        .filter(|id| !id.trim().is_empty())
    {
        Some(thread_id) => Some(
            state
                .store
                .load_thread(thread_id)
                .map_err(|error| (-32602, error.to_string()))?,
        ),
        None => runtime_session
            .active_thread_id
            .as_deref()
            .map(|thread_id| state.store.load_thread(thread_id))
            .transpose()
            .map_err(|error| (-32602, error.to_string()))?,
    };
    if let Some(thread) = thread.as_ref() {
        if thread.session_id.as_deref() != Some(runtime_session.id.as_str()) {
            return Err((
                -32602,
                format!(
                    "threadId {} does not belong to sessionId {}",
                    thread.id, runtime_session.id
                ),
            ));
        }
    }
    let cwd = thread
        .as_ref()
        .map(|thread| PathBuf::from(&thread.workspace))
        .unwrap_or_else(|| PathBuf::from(&runtime_session.workspace));
    let acp_session_id = match thread.as_ref() {
        Some(thread) => format!("runtime-{}", thread.id),
        None => format!("runtime-{}", runtime_session.id),
    };
    state.sessions.insert(
        acp_session_id.clone(),
        AcpSession {
            cwd,
            runtime_session_id: Some(runtime_session.id.clone()),
            runtime_thread_id: thread.as_ref().map(|thread| thread.id.clone()),
        },
    );

    if let Some(thread) = thread.as_ref() {
        Ok(object([
            ("sessionId", JsonValue::String(acp_session_id)),
            ("runtimeSession", acp_session_summary(&runtime_session)),
            ("runtimeThread", thread_to_json(thread)),
        ]))
    } else {
        Ok(object([
            ("sessionId", JsonValue::String(acp_session_id)),
            ("runtimeSession", acp_session_summary(&runtime_session)),
        ]))
    }
}

fn acp_session_summary(session: &crate::core::runtime::SessionRecord) -> JsonValue {
    session_to_json(session)
}

fn acp_session_from_params<'a>(
    params: &BTreeMap<String, JsonValue>,
    state: &'a AcpStdioState,
) -> Result<&'a AcpSession, (i64, String)> {
    let session_id = params
        .get("sessionId")
        .and_then(json_as_string)
        .filter(|id| !id.trim().is_empty())
        .ok_or_else(|| (-32602, "sessionId is required".to_string()))?;
    state
        .sessions
        .get(session_id)
        .ok_or_else(|| (-32602, "unknown sessionId".to_string()))
}

fn acp_mcp_state_for_session(session: &AcpSession, state: &AcpStdioState) -> McpStdioState {
    acp_mcp_state_for_session_turn(session, state, None)
}

fn acp_mcp_state_for_session_turn(
    session: &AcpSession,
    state: &AcpStdioState,
    approval_turn_id: Option<String>,
) -> McpStdioState {
    McpStdioState {
        store: state.store.clone(),
        rollback: state.rollback.clone(),
        config: state.config.clone(),
        workspace: session.cwd.clone(),
        approval: state.config.approval.clone(),
        diagnostics: state.config.diagnostics.clone(),
        approval_thread_id: session.runtime_thread_id.clone(),
        approval_turn_id,
        approval_poll_interval: state.approval_poll_interval,
        approval_max_polls: state.approval_max_polls,
        allow_side_effect_tools: false,
    }
}

fn acp_session_tools_list(
    params: &BTreeMap<String, JsonValue>,
    state: &AcpStdioState,
) -> Result<JsonValue, (i64, String)> {
    let session = acp_session_from_params(params, state)?;
    let mcp_state = acp_mcp_state_for_session(session, state);
    Ok(object([(
        "tools",
        JsonValue::Array(mcp_tool_definitions(&mcp_state)),
    )]))
}

fn acp_session_tools_call_responses(
    params: &BTreeMap<String, JsonValue>,
    state: &AcpStdioState,
) -> Result<Vec<JsonValue>, (i64, String)> {
    let session_id = acp_session_id_from_params(params)?;
    let name = acp_tool_name_from_params(params)?;
    let arguments = match params.get("arguments") {
        Some(value) => json_as_object(value)
            .ok_or_else(|| (-32602, "arguments must be an object".to_string()))?
            .clone(),
        None => BTreeMap::new(),
    };
    let tool_call_id = acp_next_tool_call_id();
    let outcome = acp_session_tools_call(params, state)?;
    let mut responses = vec![acp_session_tool_call_update(
        session_id,
        name,
        &tool_call_id,
        &arguments,
        &outcome,
    )];
    responses.extend(acp_session_tool_progress_updates(
        session_id,
        name,
        &tool_call_id,
        &outcome,
    ));
    responses.push(acp_session_tool_result_update(
        session_id,
        name,
        &tool_call_id,
        &outcome,
    ));
    responses.push(jsonrpc_success_response_without_id(outcome.result));
    Ok(responses)
}

fn acp_session_id_from_params(params: &BTreeMap<String, JsonValue>) -> Result<&str, (i64, String)> {
    params
        .get("sessionId")
        .and_then(json_as_string)
        .filter(|id| !id.trim().is_empty())
        .ok_or_else(|| (-32602, "sessionId is required".to_string()))
}

fn acp_tool_name_from_params(params: &BTreeMap<String, JsonValue>) -> Result<&str, (i64, String)> {
    params
        .get("name")
        .and_then(json_as_string)
        .filter(|name| !name.trim().is_empty())
        .ok_or_else(|| (-32602, "name is required".to_string()))
}

fn acp_session_tools_call(
    params: &BTreeMap<String, JsonValue>,
    state: &AcpStdioState,
) -> Result<AcpToolCallOutcome, (i64, String)> {
    let session = acp_session_from_params(params, state)?;
    let name = acp_tool_name_from_params(params)?;
    let arguments = match params.get("arguments") {
        Some(value) => json_as_object(value)
            .ok_or_else(|| (-32602, "arguments must be an object".to_string()))?
            .clone(),
        None => BTreeMap::new(),
    };
    if let Some(thread_id) = session.runtime_thread_id.as_deref() {
        return acp_session_tools_call_recorded(thread_id, session, state, name, &arguments);
    }
    let mcp_state = acp_mcp_state_for_session(session, state);
    match execute_mcp_tool(name, &arguments, &mcp_state) {
        Ok(text) => Ok(AcpToolCallOutcome {
            result: mcp_tool_text_result(text.clone(), false),
            text,
            is_error: false,
            turn_id: None,
            call_item_id: None,
            result_item_id: None,
        }),
        Err(error) => {
            let text = error.to_string();
            Ok(AcpToolCallOutcome {
                result: mcp_tool_text_result(text.clone(), true),
                text,
                is_error: true,
                turn_id: None,
                call_item_id: None,
                result_item_id: None,
            })
        }
    }
}

fn acp_session_tools_call_recorded(
    thread_id: &str,
    session: &AcpSession,
    state: &AcpStdioState,
    name: &str,
    arguments: &BTreeMap<String, JsonValue>,
) -> Result<AcpToolCallOutcome, (i64, String)> {
    let turn = state
        .store
        .append_turn(
            thread_id,
            "assistant".to_string(),
            format!("ACP tool call `{name}` running"),
        )
        .map_err(|error| (-32603, error.to_string()))?;
    let call_item = state
        .store
        .append_item(
            thread_id,
            Some(&turn.id),
            "tool_call".to_string(),
            Some("assistant".to_string()),
            acp_tool_call_content(name, arguments),
            "running".to_string(),
        )
        .map_err(|error| (-32603, error.to_string()))?;
    let mcp_state = acp_mcp_state_for_session_turn(session, state, Some(turn.id.clone()));
    let (text, is_error) = match execute_mcp_tool(name, arguments, &mcp_state) {
        Ok(text) => (text, false),
        Err(error) => (error.to_string(), true),
    };
    let status = if is_error { "failed" } else { "completed" };
    state
        .store
        .update_item(
            thread_id,
            &call_item.id,
            call_item.content,
            status.to_string(),
        )
        .map_err(|error| (-32603, error.to_string()))?;
    let result_item = state
        .store
        .append_item(
            thread_id,
            Some(&turn.id),
            "tool_result".to_string(),
            Some("tool".to_string()),
            text.clone(),
            status.to_string(),
        )
        .map_err(|error| (-32603, error.to_string()))?;
    state
        .store
        .update_turn(
            thread_id,
            &turn.id,
            format!("ACP tool call `{name}` {status}"),
            status.to_string(),
        )
        .map_err(|error| (-32603, error.to_string()))?;
    Ok(AcpToolCallOutcome {
        result: mcp_tool_text_result(text.clone(), is_error),
        text,
        is_error,
        turn_id: Some(turn.id),
        call_item_id: Some(call_item.id),
        result_item_id: Some(result_item.id),
    })
}

fn acp_stream_shell_tool_call<W: Write>(
    response_id: JsonValue,
    params: &BTreeMap<String, JsonValue>,
    state: &AcpStdioState,
    writer: &mut W,
) -> Result<(), (i64, String)> {
    let session_id = acp_session_id_from_params(params)?.to_string();
    let session = acp_session_from_params(params, state)?;
    let name = acp_tool_name_from_params(params)?.to_string();
    let mut arguments = match params.get("arguments") {
        Some(value) => json_as_object(value)
            .ok_or_else(|| (-32602, "arguments must be an object".to_string()))?
            .clone(),
        None => BTreeMap::new(),
    };
    if name == "exec_shell" {
        arguments.insert("background".to_string(), JsonValue::Bool(true));
    }

    let tool_call_id = acp_next_tool_call_id();
    let mut turn_id = None;
    let mut call_item_id = None;
    if let Some(thread_id) = session.runtime_thread_id.as_deref() {
        let turn = state
            .store
            .append_turn(
                thread_id,
                "assistant".to_string(),
                format!("ACP streaming tool call `{name}` running"),
            )
            .map_err(|error| (-32603, error.to_string()))?;
        let call_item = state
            .store
            .append_item(
                thread_id,
                Some(&turn.id),
                "tool_call".to_string(),
                Some("assistant".to_string()),
                acp_tool_call_content(&name, &arguments),
                "running".to_string(),
            )
            .map_err(|error| (-32603, error.to_string()))?;
        turn_id = Some(turn.id);
        call_item_id = Some(call_item.id);
    }

    let running = AcpToolCallOutcome {
        result: JsonValue::Null,
        text: String::new(),
        is_error: false,
        turn_id: turn_id.clone(),
        call_item_id: call_item_id.clone(),
        result_item_id: None,
    };
    write_json_responses(
        writer,
        vec![acp_session_tool_call_update(
            &session_id,
            &name,
            &tool_call_id,
            &arguments,
            &running,
        )],
    )
    .map_err(|error| (-32603, error.to_string()))?;

    let mcp_state = acp_mcp_state_for_session_turn(session, state, turn_id.clone());
    if !mcp_side_effect_tools_enabled(&mcp_state) {
        let outcome = acp_finish_recorded_tool_call(
            state,
            session,
            &name,
            turn_id,
            call_item_id,
            format!("ACP shell-session tool `{name}` requires a loaded runtime thread"),
            true,
        )?;
        return acp_write_tool_call_completion(
            response_id,
            &session_id,
            &name,
            &tool_call_id,
            outcome,
            writer,
        );
    }
    let start_input =
        mcp_input_with_workspace_defaults(&name, tool_input_from_json(&arguments), &mcp_state);
    let start_result = execute_mcp_shell_tool(&name, start_input, &mcp_state);
    let start_text = match start_result {
        Ok(text) => text,
        Err(error) => {
            let outcome = acp_finish_recorded_tool_call(
                state,
                session,
                &name,
                turn_id,
                call_item_id,
                error.to_string(),
                true,
            )?;
            return acp_write_tool_call_completion(
                response_id,
                &session_id,
                &name,
                &tool_call_id,
                outcome,
                writer,
            );
        }
    };

    let Some(task_id) = acp_extract_task_id(&start_text) else {
        let outcome = acp_finish_recorded_tool_call(
            state,
            session,
            &name,
            turn_id,
            call_item_id,
            start_text,
            false,
        )?;
        return acp_write_tool_call_completion(
            response_id,
            &session_id,
            &name,
            &tool_call_id,
            outcome,
            writer,
        );
    };

    let wait_tool_name = if name == "task_shell_start" {
        "task_shell_wait"
    } else {
        "exec_shell_wait"
    };
    let timeout_ms = acp_stream_timeout_ms(&arguments);
    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    let mut progress_count = 0_usize;
    let latest_delta = loop {
        let wait_input = ToolInput::new()
            .with_arg("task_id", task_id.clone())
            .with_arg("wait", "false")
            .with_arg("timeout_ms", "1");
        let delta = ExecShellWaitTool {
            tool_name: wait_tool_name,
        }
        .execute(wait_input)
        .map_err(|error| (-32603, error.to_string()))?
        .summary;
        if acp_shell_delta_has_output(&delta) {
            progress_count += 1;
            write_json_responses(
                writer,
                vec![acp_session_tool_progress_update(
                    &session_id,
                    &name,
                    &tool_call_id,
                    &running,
                    delta.clone(),
                    progress_count,
                    progress_count,
                    false,
                )],
            )
            .map_err(|error| (-32603, error.to_string()))?;
        }
        let running_status = acp_shell_status(&delta).is_some_and(|status| status == "running");
        if !running_status || Instant::now() >= deadline {
            break delta;
        }
        thread::sleep(Duration::from_millis(25));
    };

    let snapshot = ExecShellShowTool
        .execute(ToolInput::new().with_arg("task_id", task_id))
        .map(|output| output.summary)
        .unwrap_or(latest_delta);
    let final_text = format!("{start_text}\n\n{snapshot}");
    let is_error = acp_shell_status(&final_text).is_some_and(|status| status == "failed");
    let outcome = acp_finish_recorded_tool_call(
        state,
        session,
        &name,
        turn_id,
        call_item_id,
        final_text,
        is_error,
    )?;
    acp_write_tool_call_completion(
        response_id,
        &session_id,
        &name,
        &tool_call_id,
        outcome,
        writer,
    )
}

fn acp_finish_recorded_tool_call(
    state: &AcpStdioState,
    session: &AcpSession,
    name: &str,
    turn_id: Option<String>,
    call_item_id: Option<String>,
    text: String,
    is_error: bool,
) -> Result<AcpToolCallOutcome, (i64, String)> {
    let status = if is_error { "failed" } else { "completed" };
    let mut result_item_id = None;
    if let (Some(thread_id), Some(turn_id), Some(call_item_id)) = (
        session.runtime_thread_id.as_deref(),
        turn_id.as_deref(),
        call_item_id.as_deref(),
    ) {
        let call_item = state
            .store
            .load_item(thread_id, call_item_id)
            .map_err(|error| (-32603, error.to_string()))?;
        state
            .store
            .update_item(
                thread_id,
                call_item_id,
                call_item.content,
                status.to_string(),
            )
            .map_err(|error| (-32603, error.to_string()))?;
        let result_item = state
            .store
            .append_item(
                thread_id,
                Some(turn_id),
                "tool_result".to_string(),
                Some("tool".to_string()),
                text.clone(),
                status.to_string(),
            )
            .map_err(|error| (-32603, error.to_string()))?;
        state
            .store
            .update_turn(
                thread_id,
                turn_id,
                format!("ACP streaming tool call `{name}` {status}"),
                status.to_string(),
            )
            .map_err(|error| (-32603, error.to_string()))?;
        result_item_id = Some(result_item.id);
    }
    Ok(AcpToolCallOutcome {
        result: mcp_tool_text_result(text.clone(), is_error),
        text,
        is_error,
        turn_id,
        call_item_id,
        result_item_id,
    })
}

fn acp_write_tool_call_completion<W: Write>(
    response_id: JsonValue,
    session_id: &str,
    name: &str,
    tool_call_id: &str,
    outcome: AcpToolCallOutcome,
    writer: &mut W,
) -> Result<(), (i64, String)> {
    let mut final_response = jsonrpc_success_response_without_id(outcome.result.clone());
    add_jsonrpc_id(&mut final_response, response_id);
    write_json_responses(
        writer,
        vec![
            acp_session_tool_result_update(session_id, name, tool_call_id, &outcome),
            final_response,
        ],
    )
    .map_err(|error| (-32603, error.to_string()))
}

fn acp_extract_task_id(text: &str) -> Option<String> {
    text.lines()
        .find_map(|line| line.strip_prefix("task_id: "))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn acp_stream_timeout_ms(arguments: &BTreeMap<String, JsonValue>) -> u64 {
    arguments
        .get("stream_timeout_ms")
        .or_else(|| arguments.get("timeout_ms"))
        .and_then(json_as_u64)
        .unwrap_or(5_000)
        .clamp(1, 600_000)
}

fn acp_shell_status(text: &str) -> Option<&str> {
    text.lines()
        .filter_map(|line| line.strip_prefix("status: "))
        .map(str::trim)
        .last()
}

fn acp_shell_delta_has_output(text: &str) -> bool {
    text.contains("stdout_delta:\n") || text.contains("stderr_delta:\n")
}

fn acp_tool_call_content(name: &str, arguments: &BTreeMap<String, JsonValue>) -> String {
    json_value_to_string(&object([
        ("tool", JsonValue::String(name.to_string())),
        ("arguments", JsonValue::Object(arguments.clone())),
    ]))
}

fn acp_list_checkpoints(
    params: &BTreeMap<String, JsonValue>,
    state: &AcpStdioState,
) -> Result<JsonValue, (i64, String)> {
    let limit = params
        .get("limit")
        .and_then(json_as_u64)
        .map(|limit| limit.clamp(1, 100) as usize)
        .unwrap_or(20);
    let thread_filter = acp_checkpoint_thread_filter(params, state)?;
    let checkpoints = state
        .rollback
        .list_snapshots(limit)
        .map_err(|error| (-32603, error.to_string()))?
        .into_iter()
        .filter(|snapshot| {
            thread_filter
                .as_ref()
                .map(|thread_id| snapshot.runtime_thread_id.as_deref() == Some(thread_id.as_str()))
                .unwrap_or(true)
        })
        .map(|snapshot| snapshot_to_json(&snapshot))
        .collect::<Vec<_>>();
    Ok(object([
        ("checkpoints", JsonValue::Array(checkpoints)),
        ("nextCursor", JsonValue::Null),
    ]))
}

fn acp_checkpoint_thread_filter(
    params: &BTreeMap<String, JsonValue>,
    state: &AcpStdioState,
) -> Result<Option<String>, (i64, String)> {
    if let Some(session_id) = params
        .get("sessionId")
        .and_then(json_as_string)
        .filter(|id| !id.trim().is_empty())
    {
        let session = state
            .sessions
            .get(session_id)
            .ok_or_else(|| (-32602, "unknown sessionId".to_string()))?;
        return Ok(session
            .runtime_thread_id
            .clone()
            .or_else(|| Some("__no_runtime_thread__".to_string())));
    }
    Ok(params
        .get("threadId")
        .and_then(json_as_string)
        .filter(|id| !id.trim().is_empty())
        .map(str::to_string))
}

fn acp_read_checkpoint(
    params: &BTreeMap<String, JsonValue>,
    state: &AcpStdioState,
) -> Result<JsonValue, (i64, String)> {
    let checkpoint_id = params
        .get("checkpointId")
        .or_else(|| params.get("id"))
        .and_then(json_as_string)
        .filter(|id| !id.trim().is_empty())
        .ok_or_else(|| (-32602, "checkpointId is required".to_string()))?;
    let checkpoint = state
        .rollback
        .load_snapshot_or_turn(checkpoint_id)
        .map_err(|error| (-32602, error.to_string()))?;
    let include_patch = matches!(params.get("includePatch"), Some(JsonValue::Bool(true)));
    let mut response = BTreeMap::new();
    response.insert("checkpoint".to_string(), snapshot_to_json(&checkpoint));
    if include_patch {
        let patch = state
            .rollback
            .snapshot_patch(&checkpoint.id)
            .map_err(|error| (-32603, error.to_string()))?;
        response.insert("patch".to_string(), JsonValue::String(patch));
    }
    Ok(JsonValue::Object(response))
}

fn acp_restore_checkpoint(
    params: &BTreeMap<String, JsonValue>,
    state: &AcpStdioState,
) -> Result<JsonValue, (i64, String)> {
    let checkpoint_id = params
        .get("checkpointId")
        .or_else(|| params.get("id"))
        .and_then(json_as_string)
        .filter(|id| !id.trim().is_empty())
        .ok_or_else(|| (-32602, "checkpointId is required".to_string()))?;
    let checkpoint = state
        .rollback
        .load_snapshot_or_turn(checkpoint_id)
        .map_err(|error| (-32602, error.to_string()))?;
    if let Some(thread_id) = acp_checkpoint_thread_filter(params, state)? {
        if checkpoint.runtime_thread_id.as_deref() != Some(thread_id.as_str()) {
            return Err((
                -32602,
                "checkpoint does not belong to the requested session/thread".to_string(),
            ));
        }
    }
    let apply = matches!(params.get("apply"), Some(JsonValue::Bool(true)));
    let plan = state
        .rollback
        .restore_snapshot(&checkpoint.id, apply)
        .map_err(|error| (-32603, error.to_string()))?;
    Ok(object([
        ("checkpoint", snapshot_to_json(&checkpoint)),
        ("restore", restore_plan_to_json(&plan)),
        (
            "mode",
            JsonValue::String(if apply { "applied" } else { "dry_run" }.to_string()),
        ),
    ]))
}

fn restore_plan_to_json(plan: &RestorePlan) -> JsonValue {
    object([
        ("snapshot_id", JsonValue::String(plan.snapshot_id.clone())),
        ("applied", JsonValue::Bool(plan.applied)),
        ("git_root", JsonValue::String(plan.git_root.clone())),
        ("git_head", JsonValue::String(plan.git_head.clone())),
        (
            "patch_bytes",
            JsonValue::Number(plan.patch_bytes.to_string()),
        ),
        (
            "staged_patch_bytes",
            JsonValue::Number(plan.staged_patch_bytes.to_string()),
        ),
        (
            "unstaged_patch_bytes",
            JsonValue::Number(plan.unstaged_patch_bytes.to_string()),
        ),
        (
            "current_patch_bytes",
            JsonValue::Number(plan.current_patch_bytes.to_string()),
        ),
        (
            "changed_files",
            JsonValue::Array(
                plan.changed_files
                    .iter()
                    .cloned()
                    .map(JsonValue::String)
                    .collect(),
            ),
        ),
    ])
}

fn acp_prompt(
    params: &BTreeMap<String, JsonValue>,
    state: &AcpStdioState,
) -> Result<(String, String), (i64, String)> {
    let session_id = params
        .get("sessionId")
        .and_then(json_as_string)
        .ok_or_else(|| (-32602, "sessionId is required".to_string()))?;
    let session = state
        .sessions
        .get(session_id)
        .ok_or_else(|| (-32602, "unknown sessionId".to_string()))?;
    let _runtime_session_id = session.runtime_session_id.as_deref();
    let prompt = params
        .get("prompt")
        .and_then(acp_extract_prompt_text)
        .filter(|text| !text.trim().is_empty())
        .ok_or_else(|| (-32602, "prompt must include text content".to_string()))?;
    let (output, usage) = acp_run_prompt(&state.config, &prompt, &session.cwd)
        .map_err(|error| (-32603, error.to_string()))?;
    if let Some(thread_id) = session.runtime_thread_id.as_deref() {
        acp_record_prompt_result(&state.store, thread_id, &prompt, &output, usage)
            .map_err(|error| (-32603, error.to_string()))?;
    }
    Ok((session_id.to_string(), output))
}

fn acp_run_prompt(
    config: &AppConfig,
    prompt: &str,
    cwd: &Path,
) -> AppResult<(String, Option<TokenUsage>)> {
    let _cwd_guard = CwdGuard::enter(cwd)?;
    let client = DeepSeekClient {
        config: config.model.clone(),
    };
    let request = ModelRequest {
        system_prompt: "You are a coding assistant inside an ACP-compatible editor. Give concise, actionable responses.".to_string(),
        task: prompt.to_string(),
        image_inputs: Vec::new(),
        profile_name: "acp".to_string(),
        profile_hints: Vec::new(),
        primary_file: None,
        suggested_test_command: None,
        available_tools: Vec::new(),
        observations: Vec::new(),
        todos: Vec::new(),
        planning_mode: false,
        recent_steps: Vec::new(),
    };
    let mut events = NoopStreamEvents;
    let (response, usage) = client.respond(request, &mut events)?;
    Ok((response.message, usage))
}

fn acp_record_prompt_result(
    store: &RuntimeStore,
    thread_id: &str,
    prompt: &str,
    output: &str,
    usage: Option<TokenUsage>,
) -> AppResult<()> {
    let user = store.append_turn(thread_id, "user".to_string(), prompt.to_string())?;
    store.append_item(
        thread_id,
        Some(&user.id),
        "message".to_string(),
        Some("user".to_string()),
        prompt.to_string(),
        "completed".to_string(),
    )?;
    let assistant = store.append_turn(thread_id, "assistant".to_string(), output.to_string())?;
    store.append_item(
        thread_id,
        Some(&assistant.id),
        "message".to_string(),
        Some("assistant".to_string()),
        output.to_string(),
        "completed".to_string(),
    )?;
    if let Some(usage) = usage {
        let thread = store.load_thread(thread_id)?;
        store.append_usage_with_cache(
            thread_id,
            Some(&assistant.id),
            usage.model.unwrap_or(thread.model),
            "acp".to_string(),
            usage.prompt,
            usage.completion,
            usage.prompt_cache_hit,
            usage.prompt_cache_miss,
        )?;
    }
    Ok(())
}

fn acp_extract_prompt_text(value: &JsonValue) -> Option<String> {
    match value {
        JsonValue::String(text) => Some(text.clone()),
        JsonValue::Array(blocks) => {
            let parts = blocks
                .iter()
                .filter_map(acp_content_block_text)
                .collect::<Vec<_>>();
            (!parts.is_empty()).then(|| parts.join("\n\n"))
        }
        _ => None,
    }
}

fn acp_content_block_text(value: &JsonValue) -> Option<String> {
    let object = json_as_object(value)?;
    match object.get("type").and_then(json_as_string)? {
        "text" => object
            .get("text")
            .and_then(json_as_string)
            .map(str::to_string),
        "resource" => {
            if let Some(resource) = object.get("resource").and_then(json_as_object) {
                if let Some(text) = resource.get("text").and_then(json_as_string) {
                    return Some(text.to_string());
                }
                return acp_resource_uri(resource);
            }
            if let Some(text) = object.get("text").and_then(json_as_string) {
                return Some(text.to_string());
            }
            acp_resource_uri(object)
        }
        "resource_link" | "resourceLink" => acp_resource_uri(object),
        _ => None,
    }
}

fn acp_resource_uri(object: &BTreeMap<String, JsonValue>) -> Option<String> {
    object
        .get("uri")
        .and_then(json_as_string)
        .map(|uri| format!("@{uri}"))
}

fn acp_session_update(session_id: &str, text: String) -> JsonValue {
    object([
        ("jsonrpc", JsonValue::String("2.0".to_string())),
        ("method", JsonValue::String("session/update".to_string())),
        (
            "params",
            object([
                ("sessionId", JsonValue::String(session_id.to_string())),
                (
                    "update",
                    object([
                        (
                            "sessionUpdate",
                            JsonValue::String("agent_message_chunk".to_string()),
                        ),
                        (
                            "content",
                            object([
                                ("type", JsonValue::String("text".to_string())),
                                ("text", JsonValue::String(text)),
                            ]),
                        ),
                    ]),
                ),
            ]),
        ),
    ])
}

fn acp_next_tool_call_id() -> String {
    let id = ACP_TOOL_CALL_COUNTER.fetch_add(1, Ordering::Relaxed) + 1;
    format!("tool_call_{id}")
}

fn acp_session_tool_call_update(
    session_id: &str,
    name: &str,
    tool_call_id: &str,
    arguments: &BTreeMap<String, JsonValue>,
    outcome: &AcpToolCallOutcome,
) -> JsonValue {
    let mut update = BTreeMap::new();
    update.insert(
        "sessionUpdate".to_string(),
        JsonValue::String("tool_call".to_string()),
    );
    update.insert(
        "toolCallId".to_string(),
        JsonValue::String(tool_call_id.to_string()),
    );
    update.insert("title".to_string(), JsonValue::String(acp_tool_title(name)));
    update.insert(
        "kind".to_string(),
        JsonValue::String(acp_tool_kind(name).to_string()),
    );
    update.insert(
        "status".to_string(),
        JsonValue::String("in_progress".to_string()),
    );
    update.insert("rawInput".to_string(), JsonValue::Object(arguments.clone()));
    if let Some(meta) = acp_tool_runtime_meta(outcome) {
        update.insert("_meta".to_string(), meta);
    }
    acp_structured_session_update(session_id, JsonValue::Object(update))
}

fn acp_session_tool_result_update(
    session_id: &str,
    name: &str,
    tool_call_id: &str,
    outcome: &AcpToolCallOutcome,
) -> JsonValue {
    let status = if outcome.is_error {
        "failed"
    } else {
        "completed"
    };
    let mut update = BTreeMap::new();
    update.insert(
        "sessionUpdate".to_string(),
        JsonValue::String("tool_call_update".to_string()),
    );
    update.insert(
        "toolCallId".to_string(),
        JsonValue::String(tool_call_id.to_string()),
    );
    update.insert("status".to_string(), JsonValue::String(status.to_string()));
    update.insert(
        "content".to_string(),
        JsonValue::Array(vec![object([
            ("type", JsonValue::String("content".to_string())),
            (
                "content",
                object([
                    ("type", JsonValue::String("text".to_string())),
                    ("text", JsonValue::String(outcome.text.clone())),
                ]),
            ),
        ])]),
    );
    update.insert(
        "rawOutput".to_string(),
        object([
            ("text", JsonValue::String(outcome.text.clone())),
            ("isError", JsonValue::Bool(outcome.is_error)),
            ("tool", JsonValue::String(name.to_string())),
        ]),
    );
    if let Some(meta) = acp_tool_runtime_meta(outcome) {
        update.insert("_meta".to_string(), meta);
    }
    acp_structured_session_update(session_id, JsonValue::Object(update))
}

fn acp_session_tool_progress_updates(
    session_id: &str,
    name: &str,
    tool_call_id: &str,
    outcome: &AcpToolCallOutcome,
) -> Vec<JsonValue> {
    let (chunks, truncated) = acp_tool_progress_chunks(&outcome.text);
    let chunk_count = chunks.len();
    chunks
        .into_iter()
        .enumerate()
        .map(|(index, chunk)| {
            acp_session_tool_progress_update(
                session_id,
                name,
                tool_call_id,
                outcome,
                chunk,
                index + 1,
                chunk_count,
                truncated,
            )
        })
        .collect()
}

fn acp_session_tool_progress_update(
    session_id: &str,
    name: &str,
    tool_call_id: &str,
    outcome: &AcpToolCallOutcome,
    chunk: String,
    chunk_index: usize,
    chunk_count: usize,
    truncated: bool,
) -> JsonValue {
    let mut update = BTreeMap::new();
    update.insert(
        "sessionUpdate".to_string(),
        JsonValue::String("tool_call_update".to_string()),
    );
    update.insert(
        "toolCallId".to_string(),
        JsonValue::String(tool_call_id.to_string()),
    );
    update.insert(
        "status".to_string(),
        JsonValue::String("in_progress".to_string()),
    );
    update.insert(
        "content".to_string(),
        JsonValue::Array(vec![object([
            ("type", JsonValue::String("content".to_string())),
            (
                "content",
                object([
                    ("type", JsonValue::String("text".to_string())),
                    ("text", JsonValue::String(chunk.clone())),
                ]),
            ),
        ])]),
    );
    update.insert(
        "rawOutput".to_string(),
        object([
            ("text", JsonValue::String(chunk)),
            ("tool", JsonValue::String(name.to_string())),
            ("partial", JsonValue::Bool(true)),
            ("chunkIndex", JsonValue::Number(chunk_index.to_string())),
            ("chunkCount", JsonValue::Number(chunk_count.to_string())),
            ("truncated", JsonValue::Bool(truncated)),
        ]),
    );
    if let Some(meta) = acp_tool_runtime_meta(outcome) {
        update.insert("_meta".to_string(), meta);
    }
    acp_structured_session_update(session_id, JsonValue::Object(update))
}

fn acp_tool_progress_chunks(text: &str) -> (Vec<String>, bool) {
    let total_chars = text.chars().count();
    if total_chars <= ACP_TOOL_PROGRESS_MIN_CHARS {
        return (Vec::new(), false);
    }

    let mut chunks = Vec::new();
    let mut current = String::new();
    let mut current_len = 0usize;
    let mut emitted_chars = 0usize;
    for character in text.chars() {
        if chunks.len() >= ACP_TOOL_PROGRESS_MAX_CHUNKS {
            break;
        }
        current.push(character);
        current_len += 1;
        emitted_chars += 1;
        if current_len >= ACP_TOOL_PROGRESS_CHUNK_CHARS {
            chunks.push(std::mem::take(&mut current));
            current_len = 0;
        }
    }
    if chunks.len() < ACP_TOOL_PROGRESS_MAX_CHUNKS && !current.is_empty() {
        chunks.push(current);
    }
    (chunks, emitted_chars < total_chars)
}

fn acp_tool_title(name: &str) -> String {
    match name {
        "read_file" => "Reading file".to_string(),
        "write_file" | "edit_file" | "fim_edit" | "apply_patch" => "Editing workspace".to_string(),
        "delete_file" => "Deleting file".to_string(),
        "copy_file" | "move_file" => "Moving workspace files".to_string(),
        "run_shell" | "run_tests" | "exec_shell" | "task_shell_start" => {
            "Running command".to_string()
        }
        _ => format!("Running {name}"),
    }
}

fn acp_tool_kind(name: &str) -> &'static str {
    match name {
        "read_file"
        | "retrieve_tool_result"
        | "list_files"
        | "list_dir"
        | "git_status"
        | "git_diff"
        | "git_log"
        | "git_show"
        | "git_blame"
        | "github_issue_context"
        | "github_pr_context"
        | "runtime_health"
        | "runtime_list_sessions"
        | "runtime_list_threads"
        | "runtime_read_thread"
        | "runtime_list_tasks"
        | "runtime_read_task"
        | "runtime_list_agents"
        | "runtime_agent_result"
        | "review"
        | "pr_review_comment_plan"
        | "recall_archive"
        | "load_skill"
        | "image_ocr" => "read",
        "write_file" | "edit_file" | "fim_edit" | "apply_patch" | "revert_turn" | "note"
        | "remember" => "edit",
        "delete_file" => "delete",
        "copy_file" | "move_file" => "move",
        "search_text"
        | "grep_files"
        | "file_search"
        | "web_search"
        | "web_run"
        | "tool_search_tool_regex"
        | "tool_search_tool_bm25" => "search",
        "fetch_url" | "finance" | "pandoc_convert" => "fetch",
        "run_shell"
        | "run_tests"
        | "exec_shell"
        | "task_shell_start"
        | "exec_shell_interact"
        | "exec_interact"
        | "exec_shell_cancel"
        | "task_shell_wait"
        | "exec_shell_wait"
        | "exec_wait"
        | "rlm_python"
        | "rlm_python_session" => "execute",
        "rlm"
        | "rlm_query"
        | "llm_query"
        | "rlm_process"
        | "rlm_batch"
        | "rlm_query_batched"
        | "llm_query_batched"
        | "rlm_chunk_plan"
        | "rlm_map_reduce_plan"
        | "rlm_recursive_plan"
        | "image_analyze" => "think",
        _ => "other",
    }
}

fn acp_tool_runtime_meta(outcome: &AcpToolCallOutcome) -> Option<JsonValue> {
    if outcome.turn_id.is_none()
        && outcome.call_item_id.is_none()
        && outcome.result_item_id.is_none()
    {
        return None;
    }
    let mut runtime = BTreeMap::new();
    if let Some(turn_id) = outcome.turn_id.as_deref() {
        runtime.insert("turnId".to_string(), JsonValue::String(turn_id.to_string()));
    }
    if let Some(call_item_id) = outcome.call_item_id.as_deref() {
        runtime.insert(
            "callItemId".to_string(),
            JsonValue::String(call_item_id.to_string()),
        );
    }
    if let Some(result_item_id) = outcome.result_item_id.as_deref() {
        runtime.insert(
            "resultItemId".to_string(),
            JsonValue::String(result_item_id.to_string()),
        );
    }
    Some(object([("runtime", JsonValue::Object(runtime))]))
}

fn acp_structured_session_update(session_id: &str, update: JsonValue) -> JsonValue {
    object([
        ("jsonrpc", JsonValue::String("2.0".to_string())),
        ("method", JsonValue::String("session/update".to_string())),
        (
            "params",
            object([
                ("sessionId", JsonValue::String(session_id.to_string())),
                ("update", update),
            ]),
        ),
    ])
}

fn jsonrpc_success_response_without_id(result: JsonValue) -> JsonValue {
    object([
        ("jsonrpc", JsonValue::String("2.0".to_string())),
        ("result", result),
    ])
}

fn jsonrpc_error_response(id: JsonValue, code: i64, message: &str) -> JsonValue {
    object([
        ("jsonrpc", JsonValue::String("2.0".to_string())),
        ("id", id),
        (
            "error",
            object([
                ("code", JsonValue::Number(code.to_string())),
                ("message", JsonValue::String(message.to_string())),
            ]),
        ),
    ])
}

fn acp_response_needs_id(response: &JsonValue) -> bool {
    json_as_object(response)
        .map(|object| object.get("result").is_some() && object.get("id").is_none())
        .unwrap_or(false)
}

fn add_jsonrpc_id(response: &mut JsonValue, id: JsonValue) {
    if let JsonValue::Object(object) = response {
        object.insert("id".to_string(), id);
    }
}

fn serve_http_listener(listener: TcpListener, once: bool, store: &RuntimeStore) -> AppResult<()> {
    let state = RuntimeHttpState::new(store.clone());
    if once {
        let (stream, _) = listener.accept()?;
        handle_http_stream(stream, &state)?;
        return Ok(());
    }

    serve_http_listener_with_limit(listener, None, store)
}

pub(crate) fn serve_http_listener_with_limit(
    listener: TcpListener,
    request_limit: Option<usize>,
    store: &RuntimeStore,
) -> AppResult<()> {
    if request_limit == Some(0) {
        return Ok(());
    }

    let state = RuntimeHttpState::new(store.clone());
    let mut accepted = 0_usize;
    let mut handles = Vec::new();
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let worker_state = state.clone();
                let handle =
                    thread::spawn(move || match handle_http_stream(stream, &worker_state) {
                        Ok(()) => None,
                        Err(error) => Some(error.to_string()),
                    });
                accepted += 1;
                if request_limit.is_some() {
                    handles.push(handle);
                }
                if request_limit.is_some_and(|limit| accepted >= limit) {
                    break;
                }
            }
            Err(error) => return Err(app_error(format!("HTTP runtime accept failed: {error}"))),
        }
    }
    for handle in handles {
        match handle.join() {
            Ok(None) => {}
            Ok(Some(error)) => {
                return Err(app_error(format!("HTTP runtime worker failed: {error}")))
            }
            Err(_) => return Err(app_error("HTTP runtime worker panicked")),
        }
    }
    Ok(())
}

fn handle_http_stream(mut stream: TcpStream, state: &RuntimeHttpState) -> AppResult<()> {
    let mut buffer = [0_u8; 8192];
    let read = stream.read(&mut buffer)?;
    let request = String::from_utf8_lossy(&buffer[..read]);
    if handle_sse_follow_request(&request, &mut stream, &state.store)? {
        return Ok(());
    }
    let response = response_for_request_with_state(&request, state);
    stream.write_all(response.to_http_bytes().as_bytes())?;
    stream.flush()?;
    Ok(())
}

fn handle_sse_follow_request(
    request: &str,
    stream: &mut TcpStream,
    store: &RuntimeStore,
) -> AppResult<bool> {
    let Some(request_line) = request.lines().next() else {
        return Ok(false);
    };
    let parts = request_line.split_whitespace().collect::<Vec<_>>();
    if parts.len() < 2 || parts[0] != "GET" {
        return Ok(false);
    }
    let request_target = parts[1];
    if query_param_u64(request_target, "follow").unwrap_or(0) != 1 {
        return Ok(false);
    }
    let path = request_target.split('?').next().unwrap_or(request_target);
    if path == "/v1/events/stream" {
        return handle_global_sse_follow_request(request_target, stream, store);
    }
    let Some(thread_id) = event_stream_thread_id(path) else {
        return Ok(false);
    };
    if let Err(error) = validate_record_id(thread_id).and_then(|_| store.load_thread(thread_id)) {
        stream.write_all(error_response(error.to_string()).to_http_bytes().as_bytes())?;
        stream.flush()?;
        return Ok(true);
    }

    stream.write_all(
        b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream; charset=utf-8\r\nCache-Control: no-cache\r\nX-Accel-Buffering: no\r\nConnection: close\r\n\r\n",
    )?;
    stream.flush()?;

    let mut since_seq = query_param_u64(request_target, "since_seq").unwrap_or(0);
    let poll_ms = query_param_u64(request_target, "poll_ms")
        .unwrap_or(100)
        .clamp(SSE_MIN_POLL_MS, SSE_MAX_POLL_MS);
    let max_events = query_param_u64(request_target, "max_events")
        .unwrap_or(u64::MAX)
        .max(1);
    let deadline = query_param_u64(request_target, "max_ms")
        .filter(|max_ms| *max_ms > 0)
        .map(|max_ms| Instant::now() + Duration::from_millis(max_ms));
    let mut sent_events = 0_u64;

    loop {
        let events = store.read_events(thread_id, since_seq)?;
        if events.is_empty() {
            if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                stream.write_all(b": follow timeout reached\n\n")?;
                stream.flush()?;
                return Ok(true);
            }
            thread::sleep(Duration::from_millis(poll_ms));
            continue;
        }
        for event in events {
            let frame = sse_event_frame(&event);
            stream.write_all(frame.as_bytes())?;
            stream.flush()?;
            since_seq = event.seq;
            sent_events = sent_events.saturating_add(1);
            if sent_events >= max_events {
                return Ok(true);
            }
        }
    }
}

fn handle_global_sse_follow_request(
    request_target: &str,
    stream: &mut TcpStream,
    store: &RuntimeStore,
) -> AppResult<bool> {
    let mut cursor = event_cursor_from_query(request_target)?;
    let default_since_seq = query_param_u64(request_target, "since_seq").unwrap_or(0);
    stream.write_all(
        b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream; charset=utf-8\r\nCache-Control: no-cache\r\nX-Accel-Buffering: no\r\nConnection: close\r\n\r\n",
    )?;
    stream.flush()?;

    let poll_ms = query_param_u64(request_target, "poll_ms")
        .unwrap_or(100)
        .clamp(SSE_MIN_POLL_MS, SSE_MAX_POLL_MS);
    let max_events = query_param_u64(request_target, "max_events")
        .unwrap_or(u64::MAX)
        .max(1);
    let deadline = query_param_u64(request_target, "max_ms")
        .filter(|max_ms| *max_ms > 0)
        .map(|max_ms| Instant::now() + Duration::from_millis(max_ms));
    let mut sent_events = 0_u64;

    loop {
        let events = read_global_runtime_events(store, &cursor, default_since_seq)?;
        if events.is_empty() {
            if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                stream.write_all(b": follow timeout reached\n\n")?;
                stream.flush()?;
                return Ok(true);
            }
            thread::sleep(Duration::from_millis(poll_ms));
            continue;
        }
        for event in events {
            let frame = sse_global_event_frame(&event);
            stream.write_all(frame.as_bytes())?;
            stream.flush()?;
            cursor.insert(event.thread_id.clone(), event.seq);
            sent_events = sent_events.saturating_add(1);
            if sent_events >= max_events {
                return Ok(true);
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct HttpResponse {
    status: u16,
    reason: &'static str,
    content_type: &'static str,
    headers: Vec<(&'static str, &'static str)>,
    body: String,
}

impl HttpResponse {
    fn json(status: u16, reason: &'static str, body: JsonValue) -> Self {
        Self {
            status,
            reason,
            content_type: "application/json; charset=utf-8",
            headers: Vec::new(),
            body: json_value_to_string(&body),
        }
    }

    fn text(status: u16, reason: &'static str, body: impl Into<String>) -> Self {
        Self {
            status,
            reason,
            content_type: "text/plain; charset=utf-8",
            headers: Vec::new(),
            body: body.into(),
        }
    }

    fn sse(body: impl Into<String>) -> Self {
        Self {
            status: 200,
            reason: "OK",
            content_type: "text/event-stream; charset=utf-8",
            headers: vec![("Cache-Control", "no-cache"), ("X-Accel-Buffering", "no")],
            body: body.into(),
        }
    }

    fn to_http_bytes(&self) -> String {
        let mut response = format!(
            "HTTP/1.1 {} {}\r\nContent-Type: {}\r\n",
            self.status, self.reason, self.content_type,
        );
        for (name, value) in &self.headers {
            response.push_str(name);
            response.push_str(": ");
            response.push_str(value);
            response.push_str("\r\n");
        }
        response.push_str(&format!(
            "Content-Length: {}\r\nConnection: close\r\n\r\n{}",
            self.body.as_bytes().len(),
            self.body
        ));
        response
    }
}

#[derive(Clone)]
struct RuntimeHttpState {
    store: RuntimeStore,
    diagnostics: Arc<RuntimeDiagnosticsBroker>,
}

impl RuntimeHttpState {
    fn new(store: RuntimeStore) -> Self {
        Self {
            store,
            diagnostics: Arc::new(RuntimeDiagnosticsBroker::new()),
        }
    }
}

struct RuntimeDiagnosticsBroker {
    session: Mutex<Option<crate::language::diagnostics::WarmDiagnosticSession>>,
}

impl RuntimeDiagnosticsBroker {
    fn new() -> Self {
        Self {
            session: Mutex::new(None),
        }
    }

    fn run(&self, cwd: &Path, files: &[String]) -> crate::language::diagnostics::DiagnosticReport {
        let mut guard = match self.session.lock() {
            Ok(guard) => guard,
            Err(_) => {
                return crate::language::diagnostics::run_diagnostics(cwd, files);
            }
        };
        let reset = guard
            .as_ref()
            .map(|session| session.cwd() != cwd)
            .unwrap_or(true);
        if reset {
            *guard = Some(crate::language::diagnostics::WarmDiagnosticSession::new(
                cwd.to_path_buf(),
                files,
            ));
        }
        guard
            .as_mut()
            .map(|session| session.run(files))
            .unwrap_or_else(|| crate::language::diagnostics::run_diagnostics(cwd, files))
    }
}

#[cfg(test)]
fn response_for_request(request: &str, store: &RuntimeStore) -> HttpResponse {
    let state = RuntimeHttpState::new(store.clone());
    response_for_request_with_state(request, &state)
}

fn response_for_request_with_state(request: &str, state: &RuntimeHttpState) -> HttpResponse {
    match try_response_for_request(request, state) {
        Ok(response) => response,
        Err(error) => error_response(error.to_string()),
    }
}

fn try_response_for_request(request: &str, state: &RuntimeHttpState) -> AppResult<HttpResponse> {
    let Some(request_line) = request.lines().next() else {
        return Ok(bad_request("empty request"));
    };
    let parts = request_line.split_whitespace().collect::<Vec<_>>();
    if parts.len() < 2 {
        return Ok(bad_request("malformed request line"));
    }
    let method = parts[0];
    let request_target = parts[1];
    let path = request_target.split('?').next().unwrap_or(request_target);
    let body = request_body(request);
    if !matches!(method, "GET" | "HEAD" | "POST" | "PATCH") {
        return Ok(HttpResponse::json(
            405,
            "Method Not Allowed",
            object([("error", JsonValue::String("method_not_allowed".to_string()))]),
        ));
    }

    let response = match (method, path) {
        (_, "/") => HttpResponse::text(
            200,
            "OK",
            "DeepSeekCode HTTP runtime. Use /health, /runtime, or /v1/threads.\n",
        ),
        ("GET" | "HEAD", "/health" | "/v1/health") => health_response(),
        ("GET" | "HEAD", "/runtime" | "/v1/runtime") => runtime_response(),
        ("GET" | "HEAD", "/v1/diagnostics") => diagnostics_info_response(),
        ("POST", "/v1/diagnostics") => diagnostics_response(state, body)?,
        ("GET" | "HEAD", "/v1/automations") => {
            list_automations_response(&state.store, request_target, None, None)?
        }
        ("POST", "/v1/automations") => create_automation_response(&state.store, body, None, None)?,
        ("GET" | "HEAD", "/v1/sessions") => list_sessions_response(&state.store, request_target)?,
        ("POST", "/v1/sessions") => create_session_response(&state.store, body)?,
        ("GET" | "HEAD", "/v1/tasks") => {
            list_tasks_response(&state.store, request_target, None, None)?
        }
        ("POST", "/v1/tasks") => create_task_response(&state.store, body, None, None)?,
        ("GET" | "HEAD", "/v1/events/stream") => {
            let wait_ms = query_param_u64(request_target, "wait_ms").unwrap_or(0);
            let poll_ms = query_param_u64(request_target, "poll_ms").unwrap_or(100);
            global_events_stream_response(&state.store, request_target, wait_ms, poll_ms)?
        }
        ("GET" | "HEAD", "/v1/threads") => list_threads_response(&state.store, request_target)?,
        ("POST", "/v1/threads") => create_thread_response(&state.store, body)?,
        ("GET" | "HEAD", "/v1/usage/summary") => {
            usage_summary_response(&state.store, request_target, None)?
        }
        ("GET" | "HEAD", "/v1/usage") => usage_response(&state.store, request_target, None)?,
        _ if path.starts_with("/v1/automations/") => {
            route_automation_path(method, path, &state.store, body)?
        }
        _ if path.starts_with("/v1/tasks/") => route_task_path(method, path, &state.store, body)?,
        _ if path.starts_with("/v1/sessions/") => {
            route_session_path(method, request_target, path, &state.store, body)?
        }
        _ => route_thread_path(method, request_target, path, &state.store, body)?,
    };
    Ok(response)
}

fn health_response() -> HttpResponse {
    HttpResponse::json(
        200,
        "OK",
        object([
            ("status", JsonValue::String("ok".to_string())),
            ("service", JsonValue::String("DeepSeekCode".to_string())),
            (
                "version",
                JsonValue::String(env!("CARGO_PKG_VERSION").to_string()),
            ),
            ("runtime", JsonValue::String("http".to_string())),
            (
                "schema",
                JsonValue::String("deepseek.runtime.health.v1".to_string()),
            ),
        ]),
    )
}

fn runtime_response() -> HttpResponse {
    HttpResponse::json(
        200,
        "OK",
        object([
            ("service", JsonValue::String("DeepSeekCode".to_string())),
            (
                "version",
                JsonValue::String(env!("CARGO_PKG_VERSION").to_string()),
            ),
            ("api_version", JsonValue::String("v1".to_string())),
            ("transport", JsonValue::String("http".to_string())),
            (
                "endpoints",
                JsonValue::Array(
                    [
                        "/health",
                        "/v1/health",
                        "/runtime",
                        "/v1/runtime",
                        "/v1/automations",
                        "/v1/automations/{id}",
                        "/v1/automations/{id}/trigger",
                        "/v1/diagnostics",
                        "/v1/sessions",
                        "/v1/sessions/{id}",
                        "/v1/sessions/{id}/automations",
                        "/v1/sessions/{id}/threads",
                        "/v1/sessions/{id}/tasks",
                        "/v1/tasks",
                        "/v1/tasks/{id}",
                        "/v1/tasks/{id}/claim",
                        "/v1/tasks/{id}/cancel",
                        "/v1/tasks/{id}/pause",
                        "/v1/tasks/{id}/resume",
                        "/v1/events/stream",
                        "/v1/threads",
                        "/v1/threads/{id}",
                        "/v1/threads/{id}/automations",
                        "/v1/threads/{id}/compact",
                        "/v1/threads/{id}/items",
                        "/v1/threads/{id}/items/{item_id}",
                        "/v1/threads/{id}/turns",
                        "/v1/threads/{id}/turns/{turn_id}/items",
                        "/v1/threads/{id}/events",
                        "/v1/threads/{id}/events/stream",
                        "/v1/threads/{id}/tasks",
                        "/v1/threads/{id}/usage",
                        "/v1/threads/{id}/usage/summary",
                        "/v1/usage",
                        "/v1/usage/summary",
                    ]
                    .into_iter()
                    .map(|value| JsonValue::String(value.to_string()))
                    .collect(),
                ),
            ),
            (
                "capabilities",
                object([
                    ("health", JsonValue::Bool(true)),
                    ("runtime_metadata", JsonValue::Bool(true)),
                    ("sessions", JsonValue::Bool(true)),
                    ("threads", JsonValue::Bool(true)),
                    ("thread_compaction", JsonValue::Bool(true)),
                    ("turns", JsonValue::Bool(true)),
                    ("items", JsonValue::Bool(true)),
                    ("events", JsonValue::Bool(true)),
                    ("events_write", JsonValue::Bool(true)),
                    ("cancellation_events", JsonValue::Bool(true)),
                    ("events_sse", JsonValue::Bool(true)),
                    ("events_sse_wait", JsonValue::Bool(true)),
                    ("events_sse_follow", JsonValue::Bool(true)),
                    ("events_global_sse", JsonValue::Bool(true)),
                    ("events_global_sse_follow", JsonValue::Bool(true)),
                    ("diagnostics", JsonValue::Bool(true)),
                    ("diagnostics_changed", JsonValue::Bool(true)),
                    ("diagnostics_broker", JsonValue::Bool(true)),
                    ("tasks", JsonValue::Bool(true)),
                    ("task_claim", JsonValue::Bool(true)),
                    ("task_cancel", JsonValue::Bool(true)),
                    ("task_pause", JsonValue::Bool(true)),
                    ("task_resume", JsonValue::Bool(true)),
                    ("task_updates", JsonValue::Bool(true)),
                    ("automations", JsonValue::Bool(true)),
                    ("automation_trigger", JsonValue::Bool(true)),
                    ("usage", JsonValue::Bool(true)),
                    ("usage_summary", JsonValue::Bool(true)),
                ]),
            ),
        ]),
    )
}

fn diagnostics_info_response() -> HttpResponse {
    HttpResponse::json(
        200,
        "OK",
        json_object([
            (
                "schema",
                JsonValue::String("deepseek.runtime.diagnostics.info.v1".to_string()),
            ),
            (
                "endpoint",
                JsonValue::String("/v1/diagnostics".to_string()),
            ),
            (
                "methods",
                json_array(vec![
                    JsonValue::String("GET".to_string()),
                    JsonValue::String("POST".to_string()),
                ]),
            ),
            ("changed", JsonValue::Bool(true)),
            ("warmed_session", JsonValue::Bool(true)),
            (
                "request_schema",
                JsonValue::String(
                    r#"{"cwd":"optional workspace path","changed":false,"paths":["optional/file.rs"]}"#
                        .to_string(),
                ),
            ),
        ]),
    )
}

fn diagnostics_response(state: &RuntimeHttpState, body: &str) -> AppResult<HttpResponse> {
    let root = if body.trim().is_empty() {
        BTreeMap::new()
    } else {
        parse_json_object_body(body)?
    };
    let cwd = match json_optional_string_field(&root, "cwd")? {
        Some(cwd) => PathBuf::from(cwd),
        None => std::env::current_dir()
            .map_err(|error| app_error(format!("failed to read current directory: {error}")))?,
    };
    let changed = json_bool_field(&root, "changed", false)?;
    let paths = json_string_array_field(&root, "paths")?;
    let files = if changed {
        crate::cli::commands::diagnostics::changed_files(&cwd)?
    } else {
        paths
    };

    if changed && files.is_empty() {
        return Ok(HttpResponse::json(
            200,
            "OK",
            json_object([
                (
                    "schema",
                    JsonValue::String("deepseek.runtime.diagnostics.v1".to_string()),
                ),
                ("cwd", JsonValue::String(cwd.display().to_string())),
                ("changed", JsonValue::Bool(changed)),
                ("skipped", JsonValue::Bool(true)),
                ("files", json_array(Vec::new())),
                (
                    "message",
                    JsonValue::String("no changed files to diagnose".to_string()),
                ),
            ]),
        ));
    }

    let report = state.diagnostics.run(&cwd, &files);
    Ok(HttpResponse::json(
        200,
        "OK",
        json_object([
            (
                "schema",
                JsonValue::String("deepseek.runtime.diagnostics.v1".to_string()),
            ),
            ("cwd", JsonValue::String(cwd.display().to_string())),
            ("changed", JsonValue::Bool(changed)),
            ("skipped", JsonValue::Bool(false)),
            (
                "files",
                json_array(files.into_iter().map(JsonValue::String).collect()),
            ),
            ("report", diagnostic_report_to_json(&report)),
        ]),
    ))
}

fn diagnostic_report_to_json(report: &crate::language::diagnostics::DiagnosticReport) -> JsonValue {
    report.to_json_value()
}

fn list_sessions_response(store: &RuntimeStore, request_target: &str) -> AppResult<HttpResponse> {
    let limit = query_param_u64(request_target, "limit")
        .unwrap_or(50)
        .clamp(1, 200) as usize;
    let sessions = store
        .list_sessions(limit)?
        .into_iter()
        .map(|record| session_to_json(&record))
        .collect();
    Ok(HttpResponse::json(
        200,
        "OK",
        json_object([
            (
                "schema",
                JsonValue::String("deepseek.runtime.sessions.v1".to_string()),
            ),
            ("sessions", json_array(sessions)),
        ]),
    ))
}

fn create_session_response(store: &RuntimeStore, body: &str) -> AppResult<HttpResponse> {
    let root = parse_json_object_body(body)?;
    let session = store.create_session(
        json_string_field(&root, "title", "Untitled session")?,
        json_string_field(&root, "workspace", ".")?,
    )?;
    Ok(HttpResponse::json(
        201,
        "Created",
        json_object([
            (
                "schema",
                JsonValue::String("deepseek.runtime.session.v1".to_string()),
            ),
            ("session", session_to_json(&session)),
        ]),
    ))
}

fn list_automations_response(
    store: &RuntimeStore,
    request_target: &str,
    session_id: Option<&str>,
    thread_id: Option<&str>,
) -> AppResult<HttpResponse> {
    let limit = query_param_u64(request_target, "limit")
        .unwrap_or(50)
        .clamp(1, 200) as usize;
    let requested_session_id = session_id
        .map(str::to_string)
        .or_else(|| query_param_string(request_target, "session_id"));
    let requested_thread_id = thread_id
        .map(str::to_string)
        .or_else(|| query_param_string(request_target, "thread_id"));
    let automations = store
        .list_automations(
            requested_session_id.as_deref(),
            requested_thread_id.as_deref(),
            limit,
        )?
        .into_iter()
        .map(|record| automation_to_json(&record))
        .collect();
    Ok(HttpResponse::json(
        200,
        "OK",
        json_object([
            (
                "schema",
                JsonValue::String("deepseek.runtime.automations.v1".to_string()),
            ),
            (
                "session_id",
                requested_session_id
                    .map(JsonValue::String)
                    .unwrap_or(JsonValue::Null),
            ),
            (
                "thread_id",
                requested_thread_id
                    .map(JsonValue::String)
                    .unwrap_or(JsonValue::Null),
            ),
            ("automations", json_array(automations)),
        ]),
    ))
}

fn create_automation_response(
    store: &RuntimeStore,
    body: &str,
    session_id: Option<&str>,
    thread_id: Option<&str>,
) -> AppResult<HttpResponse> {
    let root = parse_json_object_body(body)?;
    let requested_session_id = match session_id {
        Some(session_id) => Some(session_id.to_string()),
        None => json_optional_string_field(&root, "session_id")?,
    };
    let requested_thread_id = match thread_id {
        Some(thread_id) => Some(thread_id.to_string()),
        None => json_optional_string_field(&root, "thread_id")?,
    };
    let automation = store.create_automation(
        requested_session_id.as_deref(),
        requested_thread_id.as_deref(),
        json_string_field(&root, "name", "Untitled automation")?,
        json_string_field(&root, "status", "active")?,
        json_string_field(&root, "schedule", "manual")?,
        json_string_field(&root, "prompt", "")?,
        json_optional_string_field(&root, "last_run_at")?,
        json_optional_string_field(&root, "next_run_at")?,
    )?;
    Ok(HttpResponse::json(
        201,
        "Created",
        json_object([
            (
                "schema",
                JsonValue::String("deepseek.runtime.automation.v1".to_string()),
            ),
            ("automation", automation_to_json(&automation)),
        ]),
    ))
}

fn list_tasks_response(
    store: &RuntimeStore,
    request_target: &str,
    session_id: Option<&str>,
    thread_id: Option<&str>,
) -> AppResult<HttpResponse> {
    let limit = query_param_u64(request_target, "limit")
        .unwrap_or(50)
        .clamp(1, 200) as usize;
    let requested_session_id = session_id
        .map(str::to_string)
        .or_else(|| query_param_string(request_target, "session_id"));
    let requested_thread_id = thread_id
        .map(str::to_string)
        .or_else(|| query_param_string(request_target, "thread_id"));
    let tasks = store
        .list_tasks(
            requested_session_id.as_deref(),
            requested_thread_id.as_deref(),
            limit,
        )?
        .into_iter()
        .map(|record| task_to_json(&record))
        .collect();
    Ok(HttpResponse::json(
        200,
        "OK",
        json_object([
            (
                "schema",
                JsonValue::String("deepseek.runtime.tasks.v1".to_string()),
            ),
            (
                "session_id",
                requested_session_id
                    .map(JsonValue::String)
                    .unwrap_or(JsonValue::Null),
            ),
            (
                "thread_id",
                requested_thread_id
                    .map(JsonValue::String)
                    .unwrap_or(JsonValue::Null),
            ),
            ("tasks", json_array(tasks)),
        ]),
    ))
}

fn create_task_response(
    store: &RuntimeStore,
    body: &str,
    session_id: Option<&str>,
    thread_id: Option<&str>,
) -> AppResult<HttpResponse> {
    let root = parse_json_object_body(body)?;
    let requested_session_id = match session_id {
        Some(session_id) => Some(session_id.to_string()),
        None => json_optional_string_field(&root, "session_id")?,
    };
    let requested_thread_id = match thread_id {
        Some(thread_id) => Some(thread_id.to_string()),
        None => json_optional_string_field(&root, "thread_id")?,
    };
    let parent_task_id = json_optional_string_field(&root, "parent_task_id")?;
    let task = store.create_task(
        requested_session_id.as_deref(),
        requested_thread_id.as_deref(),
        parent_task_id.as_deref(),
        json_string_field(&root, "kind", "agent")?,
        json_string_field(&root, "status", "pending")?,
        json_string_field(&root, "summary", "")?,
    )?;
    Ok(HttpResponse::json(
        201,
        "Created",
        json_object([
            (
                "schema",
                JsonValue::String("deepseek.runtime.task.v1".to_string()),
            ),
            ("task", task_to_json(&task)),
        ]),
    ))
}

fn list_threads_response(store: &RuntimeStore, request_target: &str) -> AppResult<HttpResponse> {
    let limit = query_param_u64(request_target, "limit")
        .unwrap_or(50)
        .clamp(1, 200) as usize;
    let threads = store
        .list_threads(limit)?
        .into_iter()
        .map(|record| thread_to_json(&record))
        .collect();
    Ok(HttpResponse::json(
        200,
        "OK",
        json_object([
            (
                "schema",
                JsonValue::String("deepseek.runtime.threads.v1".to_string()),
            ),
            ("threads", json_array(threads)),
        ]),
    ))
}

fn create_thread_response(store: &RuntimeStore, body: &str) -> AppResult<HttpResponse> {
    let root = parse_json_object_body(body)?;
    let title = json_string_field(&root, "title", "Untitled thread")?;
    let workspace = json_string_field(&root, "workspace", ".")?;
    let model = json_string_field(&root, "model", "deepseek-coder")?;
    let mode = json_string_field(&root, "mode", "agent")?;
    let thread = match json_optional_string_field(&root, "session_id")? {
        Some(session_id) => {
            store.create_thread_for_session(&session_id, title, workspace, model, mode)?
        }
        None => store.create_thread(title, workspace, model, mode)?,
    };
    Ok(HttpResponse::json(
        201,
        "Created",
        json_object([
            (
                "schema",
                JsonValue::String("deepseek.runtime.thread.v1".to_string()),
            ),
            ("thread", thread_to_json(&thread)),
        ]),
    ))
}

fn route_session_path(
    method: &str,
    request_target: &str,
    path: &str,
    store: &RuntimeStore,
    body: &str,
) -> AppResult<HttpResponse> {
    let Some(rest) = path.strip_prefix("/v1/sessions/") else {
        return Ok(not_found(path));
    };
    let parts = rest.split('/').collect::<Vec<_>>();
    match (method, parts.as_slice()) {
        ("GET" | "HEAD", [session_id]) => show_session_response(store, session_id),
        ("POST", [session_id, "threads"]) => {
            create_session_thread_response(store, session_id, body)
        }
        ("GET" | "HEAD", [session_id, "automations"]) => {
            store.load_session(session_id)?;
            list_automations_response(store, request_target, Some(session_id), None)
        }
        ("POST", [session_id, "automations"]) => {
            store.load_session(session_id)?;
            create_automation_response(store, body, Some(session_id), None)
        }
        ("GET" | "HEAD", [session_id, "tasks"]) => {
            store.load_session(session_id)?;
            list_tasks_response(store, request_target, Some(session_id), None)
        }
        ("POST", [session_id, "tasks"]) => {
            store.load_session(session_id)?;
            create_task_response(store, body, Some(session_id), None)
        }
        _ => Ok(not_found(path)),
    }
}

fn show_session_response(store: &RuntimeStore, session_id: &str) -> AppResult<HttpResponse> {
    validate_record_id(session_id)?;
    let session = store.load_session(session_id)?;
    let threads = store
        .list_session_threads(session_id, 200)?
        .into_iter()
        .map(|record| thread_to_json(&record))
        .collect();
    Ok(HttpResponse::json(
        200,
        "OK",
        json_object([
            (
                "schema",
                JsonValue::String("deepseek.runtime.session.v1".to_string()),
            ),
            ("session", session_to_json(&session)),
            ("threads", json_array(threads)),
        ]),
    ))
}

fn create_session_thread_response(
    store: &RuntimeStore,
    session_id: &str,
    body: &str,
) -> AppResult<HttpResponse> {
    validate_record_id(session_id)?;
    let root = parse_json_object_body(body)?;
    let thread = store.create_thread_for_session(
        session_id,
        json_string_field(&root, "title", "Untitled thread")?,
        json_string_field(&root, "workspace", ".")?,
        json_string_field(&root, "model", "deepseek-coder")?,
        json_string_field(&root, "mode", "agent")?,
    )?;
    Ok(HttpResponse::json(
        201,
        "Created",
        json_object([
            (
                "schema",
                JsonValue::String("deepseek.runtime.thread.v1".to_string()),
            ),
            ("thread", thread_to_json(&thread)),
        ]),
    ))
}

fn route_automation_path(
    method: &str,
    path: &str,
    store: &RuntimeStore,
    body: &str,
) -> AppResult<HttpResponse> {
    let Some(rest) = path.strip_prefix("/v1/automations/") else {
        return Ok(not_found(path));
    };
    let parts = rest.split('/').collect::<Vec<_>>();
    match (method, parts.as_slice()) {
        ("GET" | "HEAD", [automation_id]) => show_automation_response(store, automation_id),
        ("POST", [automation_id, "trigger"]) => {
            trigger_automation_response(store, automation_id, body)
        }
        _ => Ok(not_found(path)),
    }
}

fn show_automation_response(store: &RuntimeStore, automation_id: &str) -> AppResult<HttpResponse> {
    validate_record_id(automation_id)?;
    let automation = store.load_automation(automation_id)?;
    Ok(HttpResponse::json(
        200,
        "OK",
        json_object([
            (
                "schema",
                JsonValue::String("deepseek.runtime.automation.v1".to_string()),
            ),
            ("automation", automation_to_json(&automation)),
        ]),
    ))
}

fn trigger_automation_response(
    store: &RuntimeStore,
    automation_id: &str,
    body: &str,
) -> AppResult<HttpResponse> {
    validate_record_id(automation_id)?;
    let root = parse_json_object_body(body)?;
    let prompt_override = json_optional_string_field(&root, "prompt")?;
    let (automation, task) = store.trigger_automation(automation_id, prompt_override)?;
    Ok(HttpResponse::json(
        201,
        "Created",
        json_object([
            (
                "schema",
                JsonValue::String("deepseek.runtime.automation_trigger.v1".to_string()),
            ),
            ("automation", automation_to_json(&automation)),
            ("task", task_to_json(&task)),
        ]),
    ))
}

fn route_task_path(
    method: &str,
    path: &str,
    store: &RuntimeStore,
    body: &str,
) -> AppResult<HttpResponse> {
    let Some(rest) = path.strip_prefix("/v1/tasks/") else {
        return Ok(not_found(path));
    };
    let parts = rest.split('/').collect::<Vec<_>>();
    match (method, parts.as_slice()) {
        ("GET" | "HEAD", [task_id]) => show_task_response(store, task_id),
        ("POST", [task_id, "claim"]) => claim_task_response(store, task_id, body),
        ("POST", [task_id, "cancel"]) => cancel_task_response(store, task_id, body),
        ("POST", [task_id, "pause"]) => pause_task_response(store, task_id, body),
        ("POST", [task_id, "resume"]) => resume_task_response(store, task_id, body),
        ("PATCH" | "POST", [task_id]) => update_task_response(store, task_id, body),
        _ => Ok(not_found(path)),
    }
}

fn show_task_response(store: &RuntimeStore, task_id: &str) -> AppResult<HttpResponse> {
    validate_record_id(task_id)?;
    let task = store.load_task(task_id)?;
    Ok(HttpResponse::json(
        200,
        "OK",
        json_object([
            (
                "schema",
                JsonValue::String("deepseek.runtime.task.v1".to_string()),
            ),
            ("task", task_to_json(&task)),
        ]),
    ))
}

fn claim_task_response(store: &RuntimeStore, task_id: &str, body: &str) -> AppResult<HttpResponse> {
    validate_record_id(task_id)?;
    let root = parse_json_object_body(body)?;
    let runner_id = json_string_field(&root, "runner_id", "runtime-http")?;
    let task = store.claim_task(task_id, runner_id)?;
    Ok(HttpResponse::json(
        200,
        "OK",
        json_object([
            (
                "schema",
                JsonValue::String("deepseek.runtime.task_claim.v1".to_string()),
            ),
            ("task", task_to_json(&task)),
        ]),
    ))
}

fn cancel_task_response(
    store: &RuntimeStore,
    task_id: &str,
    body: &str,
) -> AppResult<HttpResponse> {
    validate_record_id(task_id)?;
    let root = parse_json_object_body(body)?;
    let reason = json_string_field(&root, "reason", "user requested cancellation")?;
    let (task, event) = store.cancel_task(task_id, reason)?;
    Ok(HttpResponse::json(
        200,
        "OK",
        json_object([
            (
                "schema",
                JsonValue::String("deepseek.runtime.task_cancel.v1".to_string()),
            ),
            ("task", task_to_json(&task)),
            (
                "event",
                event.as_ref().map(event_to_json).unwrap_or(JsonValue::Null),
            ),
        ]),
    ))
}

fn pause_task_response(store: &RuntimeStore, task_id: &str, body: &str) -> AppResult<HttpResponse> {
    validate_record_id(task_id)?;
    let root = parse_json_object_body(body)?;
    let summary = json_optional_string_field(&root, "summary")?;
    let task = store.pause_task(task_id, summary)?;
    Ok(HttpResponse::json(
        200,
        "OK",
        json_object([
            (
                "schema",
                JsonValue::String("deepseek.runtime.task_pause.v1".to_string()),
            ),
            ("task", task_to_json(&task)),
        ]),
    ))
}

fn resume_task_response(
    store: &RuntimeStore,
    task_id: &str,
    body: &str,
) -> AppResult<HttpResponse> {
    validate_record_id(task_id)?;
    let root = parse_json_object_body(body)?;
    let summary = json_optional_string_field(&root, "summary")?;
    let task = store.resume_task(task_id, summary)?;
    Ok(HttpResponse::json(
        200,
        "OK",
        json_object([
            (
                "schema",
                JsonValue::String("deepseek.runtime.task_resume.v1".to_string()),
            ),
            ("task", task_to_json(&task)),
        ]),
    ))
}

fn update_task_response(
    store: &RuntimeStore,
    task_id: &str,
    body: &str,
) -> AppResult<HttpResponse> {
    validate_record_id(task_id)?;
    let root = parse_json_object_body(body)?;
    let current = store.load_task(task_id)?;
    let status = json_string_field(&root, "status", &current.status)?;
    let summary = json_string_field(&root, "summary", &current.summary)?;
    let task = store.update_task(task_id, status, summary)?;
    Ok(HttpResponse::json(
        200,
        "OK",
        json_object([
            (
                "schema",
                JsonValue::String("deepseek.runtime.task.v1".to_string()),
            ),
            ("task", task_to_json(&task)),
        ]),
    ))
}

fn route_thread_path(
    method: &str,
    request_target: &str,
    path: &str,
    store: &RuntimeStore,
    body: &str,
) -> AppResult<HttpResponse> {
    let Some(rest) = path.strip_prefix("/v1/threads/") else {
        return Ok(not_found(path));
    };
    let parts = rest.split('/').collect::<Vec<_>>();
    match (method, parts.as_slice()) {
        ("GET" | "HEAD", [thread_id]) => show_thread_response(store, thread_id),
        ("GET" | "HEAD", [thread_id, "items"]) => {
            store.load_thread(thread_id)?;
            list_items_response(store, request_target, thread_id, None)
        }
        ("POST", [thread_id, "items"]) => {
            store.load_thread(thread_id)?;
            create_item_response(store, thread_id, None, body)
        }
        ("GET" | "HEAD", [thread_id, "items", item_id]) => {
            show_item_response(store, thread_id, item_id)
        }
        ("POST", [thread_id, "turns"]) => create_turn_response(store, thread_id, body),
        ("GET" | "HEAD", [thread_id, "turns", turn_id, "items"]) => {
            store.load_thread(thread_id)?;
            list_items_response(store, request_target, thread_id, Some(turn_id))
        }
        ("POST", [thread_id, "turns", turn_id, "items"]) => {
            store.load_thread(thread_id)?;
            create_item_response(store, thread_id, Some(turn_id), body)
        }
        ("GET" | "HEAD", [thread_id, "events"]) => {
            let since_seq = query_param_u64(request_target, "since_seq").unwrap_or(0);
            events_response(store, thread_id, since_seq)
        }
        ("POST", [thread_id, "events"]) => {
            store.load_thread(thread_id)?;
            create_event_response(store, thread_id, body)
        }
        ("GET" | "HEAD", [thread_id, "events", "stream"]) => {
            let since_seq = query_param_u64(request_target, "since_seq").unwrap_or(0);
            let wait_ms = query_param_u64(request_target, "wait_ms").unwrap_or(0);
            let poll_ms = query_param_u64(request_target, "poll_ms").unwrap_or(100);
            events_stream_response(store, thread_id, since_seq, wait_ms, poll_ms)
        }
        ("GET" | "HEAD", [thread_id, "automations"]) => {
            store.load_thread(thread_id)?;
            list_automations_response(store, request_target, None, Some(thread_id))
        }
        ("POST", [thread_id, "automations"]) => {
            store.load_thread(thread_id)?;
            create_automation_response(store, body, None, Some(thread_id))
        }
        ("GET" | "HEAD", [thread_id, "tasks"]) => {
            store.load_thread(thread_id)?;
            list_tasks_response(store, request_target, None, Some(thread_id))
        }
        ("POST", [thread_id, "tasks"]) => {
            store.load_thread(thread_id)?;
            create_task_response(store, body, None, Some(thread_id))
        }
        ("POST", [thread_id, "compact"]) => compact_thread_response(store, thread_id, body),
        ("GET" | "HEAD", [thread_id, "usage", "summary"]) => {
            store.load_thread(thread_id)?;
            usage_summary_response(store, request_target, Some(thread_id))
        }
        ("GET" | "HEAD", [thread_id, "usage"]) => {
            store.load_thread(thread_id)?;
            usage_response(store, request_target, Some(thread_id))
        }
        _ => Ok(not_found(path)),
    }
}

fn show_thread_response(store: &RuntimeStore, thread_id: &str) -> AppResult<HttpResponse> {
    validate_record_id(thread_id)?;
    let thread = store.load_thread(thread_id)?;
    let turns = store
        .list_turns(thread_id)?
        .into_iter()
        .map(|record| turn_to_json(&record))
        .collect();
    let items = store
        .list_items(thread_id, None)?
        .into_iter()
        .map(|record| item_to_json(&record))
        .collect();
    Ok(HttpResponse::json(
        200,
        "OK",
        json_object([
            (
                "schema",
                JsonValue::String("deepseek.runtime.thread.v1".to_string()),
            ),
            ("thread", thread_to_json(&thread)),
            ("turns", json_array(turns)),
            ("items", json_array(items)),
        ]),
    ))
}

fn create_turn_response(
    store: &RuntimeStore,
    thread_id: &str,
    body: &str,
) -> AppResult<HttpResponse> {
    validate_record_id(thread_id)?;
    let root = parse_json_object_body(body)?;
    let role = json_string_field(&root, "role", "user")?;
    let content = json_string_field(&root, "content", "")?;
    if content.trim().is_empty() {
        return Ok(bad_request("turn content must not be empty"));
    }
    let turn = store.append_turn(thread_id, role, content)?;
    Ok(HttpResponse::json(
        201,
        "Created",
        json_object([
            (
                "schema",
                JsonValue::String("deepseek.runtime.turn.v1".to_string()),
            ),
            ("turn", turn_to_json(&turn)),
        ]),
    ))
}

fn compact_thread_response(
    store: &RuntimeStore,
    thread_id: &str,
    body: &str,
) -> AppResult<HttpResponse> {
    validate_record_id(thread_id)?;
    let root = parse_json_object_body(body)?;
    let keep_tail_turns = json_usize_field(&root, "keep_tail_turns", 8, 200)?;
    let summary = json_optional_string_field(&root, "summary")?;
    let compaction = store.compact_thread(thread_id, keep_tail_turns, summary)?;
    Ok(HttpResponse::json(
        201,
        "Created",
        json_object([
            (
                "schema",
                JsonValue::String("deepseek.runtime.thread_compaction.v1".to_string()),
            ),
            ("compaction", thread_compaction_to_json(&compaction)),
        ]),
    ))
}

fn list_items_response(
    store: &RuntimeStore,
    request_target: &str,
    thread_id: &str,
    turn_id: Option<&str>,
) -> AppResult<HttpResponse> {
    validate_record_id(thread_id)?;
    if let Some(turn_id) = turn_id {
        ensure_thread_turn(store, thread_id, turn_id)?;
    }
    let limit = query_param_u64(request_target, "limit")
        .unwrap_or(50)
        .clamp(1, 200) as usize;
    let mut items = store.list_items(thread_id, turn_id)?;
    items.truncate(limit);
    let turn_id_value = turn_id
        .map(|value| JsonValue::String(value.to_string()))
        .unwrap_or(JsonValue::Null);
    Ok(HttpResponse::json(
        200,
        "OK",
        json_object([
            (
                "schema",
                JsonValue::String("deepseek.runtime.items.v1".to_string()),
            ),
            ("thread_id", JsonValue::String(thread_id.to_string())),
            ("turn_id", turn_id_value),
            (
                "items",
                json_array(
                    items
                        .into_iter()
                        .map(|record| item_to_json(&record))
                        .collect(),
                ),
            ),
        ]),
    ))
}

fn create_item_response(
    store: &RuntimeStore,
    thread_id: &str,
    turn_id: Option<&str>,
    body: &str,
) -> AppResult<HttpResponse> {
    validate_record_id(thread_id)?;
    if let Some(turn_id) = turn_id {
        validate_record_id(turn_id)?;
    }
    let root = parse_json_object_body(body)?;
    let body_turn_id = json_optional_string_field(&root, "turn_id")?;
    let requested_turn_id = match (turn_id, body_turn_id.as_deref()) {
        (Some(path_turn_id), Some(body_turn_id)) if path_turn_id != body_turn_id => {
            return Ok(bad_request("item turn_id does not match request path"));
        }
        (Some(path_turn_id), _) => Some(path_turn_id),
        (None, Some(body_turn_id)) => Some(body_turn_id),
        (None, None) => None,
    };
    let item_type = json_string_field(&root, "item_type", "message")?;
    let role = json_optional_string_field(&root, "role")?;
    let content = json_string_field(&root, "content", "")?;
    if content.trim().is_empty() {
        return Ok(bad_request("item content must not be empty"));
    }
    let status = json_string_field(&root, "status", "completed")?;
    let item = store.append_item(
        thread_id,
        requested_turn_id,
        item_type,
        role,
        content,
        status,
    )?;
    Ok(HttpResponse::json(
        201,
        "Created",
        json_object([
            (
                "schema",
                JsonValue::String("deepseek.runtime.item.v1".to_string()),
            ),
            ("item", item_to_json(&item)),
        ]),
    ))
}

fn show_item_response(
    store: &RuntimeStore,
    thread_id: &str,
    item_id: &str,
) -> AppResult<HttpResponse> {
    validate_record_id(thread_id)?;
    store.load_thread(thread_id)?;
    let item = store.load_item(thread_id, item_id)?;
    Ok(HttpResponse::json(
        200,
        "OK",
        json_object([
            (
                "schema",
                JsonValue::String("deepseek.runtime.item.v1".to_string()),
            ),
            ("item", item_to_json(&item)),
        ]),
    ))
}

fn ensure_thread_turn(store: &RuntimeStore, thread_id: &str, turn_id: &str) -> AppResult<()> {
    validate_record_id(turn_id)?;
    let exists = store
        .list_turns(thread_id)?
        .into_iter()
        .any(|turn| turn.id == turn_id);
    if exists {
        Ok(())
    } else {
        Err(app_error(format!("runtime turn not found: {turn_id}")))
    }
}

fn events_response(
    store: &RuntimeStore,
    thread_id: &str,
    since_seq: u64,
) -> AppResult<HttpResponse> {
    validate_record_id(thread_id)?;
    store.load_thread(thread_id)?;
    let events = store
        .read_events(thread_id, since_seq)?
        .into_iter()
        .map(|event| event_to_json(&event))
        .collect();
    Ok(HttpResponse::json(
        200,
        "OK",
        json_object([
            (
                "schema",
                JsonValue::String("deepseek.runtime.events.v1".to_string()),
            ),
            ("events", json_array(events)),
        ]),
    ))
}

fn create_event_response(
    store: &RuntimeStore,
    thread_id: &str,
    body: &str,
) -> AppResult<HttpResponse> {
    validate_record_id(thread_id)?;
    let root = parse_json_object_body(body)?;
    let event_kind = match json_optional_string_field(&root, "event_kind")? {
        Some(kind) => kind,
        None => json_string_field(&root, "type", "")?,
    };
    let payload = event_payload_object(&root)?;
    let turn_id = json_optional_string_field(&root, "turn_id")?;
    let event = match event_kind.as_str() {
        "permission_request" => {
            let permission_kind = match json_optional_string_field(payload, "permission_kind")? {
                Some(kind) => kind,
                None => json_string_field(payload, "kind", "permission")?,
            };
            store.append_permission_request(
                thread_id,
                turn_id.as_deref(),
                json_string_field(payload, "tool", "unknown")?,
                permission_kind,
                json_string_field(payload, "target", "")?,
                json_string_map_field(payload, "input")?,
            )?
        }
        "permission_response" => store.append_permission_response(
            thread_id,
            turn_id.as_deref(),
            json_string_field(payload, "request_id", "")?,
            json_string_field(payload, "decision", "")?,
        )?,
        "user_input_request" => {
            let questions = payload
                .get("questions")
                .cloned()
                .ok_or_else(|| app_error("user_input_request requires `questions`"))?;
            store.append_user_input_request(thread_id, turn_id.as_deref(), questions)?
        }
        "user_input_response" => store.append_user_input_response(
            thread_id,
            turn_id.as_deref(),
            json_string_field(payload, "request_id", "")?,
            json_string_map_field(payload, "answers")?,
        )?,
        "cancel_requested" => store.append_cancel_request(
            thread_id,
            turn_id.as_deref(),
            json_optional_string_field(payload, "task_id")?.as_deref(),
            json_string_field(payload, "reason", "user requested cancellation")?,
        )?,
        _ => {
            return Ok(bad_request(
                "only permission_request, permission_response, user_input_request, user_input_response, or cancel_requested events can be appended through this endpoint",
            ));
        }
    };

    Ok(HttpResponse::json(
        201,
        "Created",
        json_object([
            (
                "schema",
                JsonValue::String("deepseek.runtime.event.v1".to_string()),
            ),
            ("event", event_to_json(&event)),
        ]),
    ))
}

fn events_stream_response(
    store: &RuntimeStore,
    thread_id: &str,
    since_seq: u64,
    wait_ms: u64,
    poll_ms: u64,
) -> AppResult<HttpResponse> {
    validate_record_id(thread_id)?;
    store.load_thread(thread_id)?;
    let mut body = String::new();
    let events = read_events_with_wait(
        store,
        thread_id,
        since_seq,
        wait_ms.min(SSE_MAX_WAIT_MS),
        poll_ms.clamp(SSE_MIN_POLL_MS, SSE_MAX_POLL_MS),
    )?;
    for event in events {
        body.push_str(&sse_event_frame(&event));
    }
    if body.is_empty() {
        if wait_ms == 0 {
            body.push_str(": no runtime events after since_seq\n\n");
        } else {
            body.push_str(": no runtime events after since_seq before wait timeout\n\n");
        }
    }
    Ok(HttpResponse::sse(body))
}

fn global_events_stream_response(
    store: &RuntimeStore,
    request_target: &str,
    wait_ms: u64,
    poll_ms: u64,
) -> AppResult<HttpResponse> {
    let cursor = event_cursor_from_query(request_target)?;
    let default_since_seq = query_param_u64(request_target, "since_seq").unwrap_or(0);
    let mut body = String::new();
    let events = read_global_events_with_wait(
        store,
        &cursor,
        default_since_seq,
        wait_ms.min(SSE_MAX_WAIT_MS),
        poll_ms.clamp(SSE_MIN_POLL_MS, SSE_MAX_POLL_MS),
    )?;
    for event in events {
        body.push_str(&sse_global_event_frame(&event));
    }
    if body.is_empty() {
        if wait_ms == 0 {
            body.push_str(": no runtime events after cursor\n\n");
        } else {
            body.push_str(": no runtime events after cursor before wait timeout\n\n");
        }
    }
    Ok(HttpResponse::sse(body))
}

const SSE_MAX_WAIT_MS: u64 = 30_000;
const SSE_MIN_POLL_MS: u64 = 10;
const SSE_MAX_POLL_MS: u64 = 1_000;

fn event_cursor_from_query(request_target: &str) -> AppResult<BTreeMap<String, u64>> {
    let mut cursor = BTreeMap::new();
    let Some(raw_cursor) = query_param_string(request_target, "since") else {
        return Ok(cursor);
    };
    for entry in raw_cursor.split(',').filter(|entry| !entry.is_empty()) {
        let Some((thread_id, seq)) = entry.split_once(':') else {
            return Err(app_error(format!(
                "invalid runtime event cursor entry `{entry}`"
            )));
        };
        validate_record_id(thread_id)?;
        let seq = seq
            .parse::<u64>()
            .map_err(|_| app_error(format!("invalid runtime event cursor seq in `{entry}`")))?;
        cursor.insert(thread_id.to_string(), seq);
    }
    Ok(cursor)
}

fn read_global_events_with_wait(
    store: &RuntimeStore,
    cursor: &BTreeMap<String, u64>,
    default_since_seq: u64,
    wait_ms: u64,
    poll_ms: u64,
) -> AppResult<Vec<RuntimeEvent>> {
    let deadline = Instant::now() + Duration::from_millis(wait_ms);
    loop {
        let events = read_global_runtime_events(store, cursor, default_since_seq)?;
        if !events.is_empty() || wait_ms == 0 {
            return Ok(events);
        }
        let now = Instant::now();
        if now >= deadline {
            return Ok(events);
        }
        let remaining = deadline.saturating_duration_since(now);
        thread::sleep(remaining.min(Duration::from_millis(poll_ms)));
    }
}

fn read_global_runtime_events(
    store: &RuntimeStore,
    cursor: &BTreeMap<String, u64>,
    default_since_seq: u64,
) -> AppResult<Vec<RuntimeEvent>> {
    let mut events = Vec::new();
    for thread in store.list_threads(usize::MAX)? {
        let since_seq = cursor.get(&thread.id).copied().unwrap_or(default_since_seq);
        events.extend(store.read_events(&thread.id, since_seq)?);
    }
    events.sort_by(|a, b| {
        a.created_at
            .cmp(&b.created_at)
            .then_with(|| a.thread_id.cmp(&b.thread_id))
            .then_with(|| a.seq.cmp(&b.seq))
    });
    Ok(events)
}

fn event_stream_thread_id(path: &str) -> Option<&str> {
    let rest = path.strip_prefix("/v1/threads/")?;
    let parts = rest.split('/').collect::<Vec<_>>();
    match parts.as_slice() {
        [thread_id, "events", "stream"] => Some(thread_id),
        _ => None,
    }
}

fn sse_event_frame(event: &RuntimeEvent) -> String {
    sse_event_frame_with_id(event, &event.seq.to_string())
}

fn sse_global_event_frame(event: &RuntimeEvent) -> String {
    sse_event_frame_with_id(event, &format!("{}:{}", event.thread_id, event.seq))
}

fn sse_event_frame_with_id(event: &RuntimeEvent, id: &str) -> String {
    let mut frame = String::new();
    frame.push_str("id: ");
    frame.push_str(id);
    frame.push('\n');
    frame.push_str("event: ");
    frame.push_str(&event.kind);
    frame.push('\n');
    frame.push_str("data: ");
    frame.push_str(&json_value_to_string(&event_to_json(event)));
    frame.push_str("\n\n");
    frame
}

fn read_events_with_wait(
    store: &RuntimeStore,
    thread_id: &str,
    since_seq: u64,
    wait_ms: u64,
    poll_ms: u64,
) -> AppResult<Vec<RuntimeEvent>> {
    let deadline = Instant::now() + Duration::from_millis(wait_ms);
    loop {
        let events = store.read_events(thread_id, since_seq)?;
        if !events.is_empty() || wait_ms == 0 {
            return Ok(events);
        }
        let now = Instant::now();
        if now >= deadline {
            return Ok(events);
        }
        let remaining = deadline.saturating_duration_since(now);
        thread::sleep(remaining.min(Duration::from_millis(poll_ms)));
    }
}

fn usage_response(
    store: &RuntimeStore,
    request_target: &str,
    thread_id: Option<&str>,
) -> AppResult<HttpResponse> {
    let limit = query_param_u64(request_target, "limit")
        .unwrap_or(50)
        .clamp(1, 200) as usize;
    let requested_thread_id = match thread_id {
        Some(thread_id) => Some(thread_id.to_string()),
        None => query_param_string(request_target, "thread_id"),
    };
    if let Some(thread_id) = requested_thread_id.as_deref() {
        validate_record_id(thread_id)?;
        store.load_thread(thread_id)?;
    }
    let usage = store
        .list_usage(requested_thread_id.as_deref(), limit)?
        .into_iter()
        .map(|record| usage_to_json(&record))
        .collect();
    Ok(HttpResponse::json(
        200,
        "OK",
        json_object([
            (
                "schema",
                JsonValue::String("deepseek.runtime.usage.v1".to_string()),
            ),
            (
                "thread_id",
                requested_thread_id
                    .map(JsonValue::String)
                    .unwrap_or(JsonValue::Null),
            ),
            ("usage", json_array(usage)),
        ]),
    ))
}

fn usage_summary_response(
    store: &RuntimeStore,
    request_target: &str,
    thread_id: Option<&str>,
) -> AppResult<HttpResponse> {
    const CONTEXT_WINDOW_TOKENS: u64 = 1_000_000;
    const WARNING_THRESHOLD_TOKENS: u64 = 800_000;
    const HARD_THRESHOLD_TOKENS: u64 = 900_000;

    let requested_thread_id = match thread_id {
        Some(thread_id) => Some(thread_id.to_string()),
        None => query_param_string(request_target, "thread_id"),
    };
    if let Some(thread_id) = requested_thread_id.as_deref() {
        validate_record_id(thread_id)?;
        store.load_thread(thread_id)?;
    }
    let usage = store.list_usage(requested_thread_id.as_deref(), usize::MAX)?;
    let mut prompt_tokens = 0_u64;
    let mut completion_tokens = 0_u64;
    let mut total_tokens = 0_u64;
    let mut prompt_cache_hit_tokens = 0_u64;
    let mut prompt_cache_miss_tokens = 0_u64;
    let mut estimated_input_cost_microusd = 0_u64;
    let mut estimated_output_cost_microusd = 0_u64;
    let mut estimated_total_cost_microusd = 0_u64;
    let mut unpriced_record_count = 0_u64;
    for record in &usage {
        prompt_tokens = prompt_tokens.saturating_add(record.prompt_tokens);
        completion_tokens = completion_tokens.saturating_add(record.completion_tokens);
        total_tokens = total_tokens.saturating_add(record.total_tokens);
        prompt_cache_hit_tokens =
            prompt_cache_hit_tokens.saturating_add(record.prompt_cache_hit_tokens);
        prompt_cache_miss_tokens =
            prompt_cache_miss_tokens.saturating_add(record.prompt_cache_miss_tokens);
        match (
            record.estimated_input_cost_microusd,
            record.estimated_output_cost_microusd,
            record.estimated_total_cost_microusd,
        ) {
            (Some(input), Some(output), Some(total)) => {
                estimated_input_cost_microusd = estimated_input_cost_microusd.saturating_add(input);
                estimated_output_cost_microusd =
                    estimated_output_cost_microusd.saturating_add(output);
                estimated_total_cost_microusd = estimated_total_cost_microusd.saturating_add(total);
            }
            _ => unpriced_record_count = unpriced_record_count.saturating_add(1),
        }
    }
    let latest_total_tokens = usage.first().map(|record| record.total_tokens).unwrap_or(0);
    let remaining_tokens = CONTEXT_WINDOW_TOKENS.saturating_sub(latest_total_tokens);
    let utilization_basis_points =
        latest_total_tokens.saturating_mul(10_000) / CONTEXT_WINDOW_TOKENS;
    let cache_accounted_prompt_tokens =
        prompt_cache_hit_tokens.saturating_add(prompt_cache_miss_tokens);
    let prompt_cache_hit_basis_points = if cache_accounted_prompt_tokens == 0 {
        0
    } else {
        prompt_cache_hit_tokens.saturating_mul(10_000) / cache_accounted_prompt_tokens
    };
    let strategy = context_strategy(latest_total_tokens);
    let compaction_endpoint = requested_thread_id
        .as_ref()
        .map(|thread_id| JsonValue::String(format!("/v1/threads/{thread_id}/compact")))
        .unwrap_or(JsonValue::Null);
    let compaction_recommended = matches!(strategy, "prepare_compaction" | "must_compact_or_chunk");

    Ok(HttpResponse::json(
        200,
        "OK",
        json_object([
            (
                "schema",
                JsonValue::String("deepseek.runtime.usage_summary.v1".to_string()),
            ),
            (
                "thread_id",
                requested_thread_id
                    .map(JsonValue::String)
                    .unwrap_or(JsonValue::Null),
            ),
            ("record_count", JsonValue::Number(usage.len().to_string())),
            (
                "prompt_tokens",
                JsonValue::Number(prompt_tokens.to_string()),
            ),
            (
                "completion_tokens",
                JsonValue::Number(completion_tokens.to_string()),
            ),
            ("total_tokens", JsonValue::Number(total_tokens.to_string())),
            (
                "prompt_cache_hit_tokens",
                JsonValue::Number(prompt_cache_hit_tokens.to_string()),
            ),
            (
                "prompt_cache_miss_tokens",
                JsonValue::Number(prompt_cache_miss_tokens.to_string()),
            ),
            (
                "prompt_cache_hit_basis_points",
                JsonValue::Number(prompt_cache_hit_basis_points.to_string()),
            ),
            (
                "estimated_input_cost_microusd",
                JsonValue::Number(estimated_input_cost_microusd.to_string()),
            ),
            (
                "estimated_output_cost_microusd",
                JsonValue::Number(estimated_output_cost_microusd.to_string()),
            ),
            (
                "estimated_total_cost_microusd",
                JsonValue::Number(estimated_total_cost_microusd.to_string()),
            ),
            (
                "unpriced_record_count",
                JsonValue::Number(unpriced_record_count.to_string()),
            ),
            (
                "pricing_source",
                JsonValue::String(
                    "DeepSeek official USD pricing table when model is recognized; unknown models are excluded from estimated cost"
                        .to_string(),
                ),
            ),
            (
                "latest_total_tokens",
                JsonValue::Number(latest_total_tokens.to_string()),
            ),
            (
                "context_window_tokens",
                JsonValue::Number(CONTEXT_WINDOW_TOKENS.to_string()),
            ),
            (
                "warning_threshold_tokens",
                JsonValue::Number(WARNING_THRESHOLD_TOKENS.to_string()),
            ),
            (
                "hard_threshold_tokens",
                JsonValue::Number(HARD_THRESHOLD_TOKENS.to_string()),
            ),
            (
                "latest_context_remaining_tokens",
                JsonValue::Number(remaining_tokens.to_string()),
            ),
            (
                "latest_context_utilization_basis_points",
                JsonValue::Number(utilization_basis_points.to_string()),
            ),
            (
                "context_strategy",
                JsonValue::String(strategy.to_string()),
            ),
            (
                "compaction_recommended",
                JsonValue::Bool(compaction_recommended),
            ),
            ("compaction_endpoint", compaction_endpoint),
        ]),
    ))
}

fn context_strategy(latest_total_tokens: u64) -> &'static str {
    match latest_total_tokens {
        900_000.. => "must_compact_or_chunk",
        800_000.. => "prepare_compaction",
        500_000.. => "monitor",
        _ => "normal",
    }
}

fn bad_request(message: &str) -> HttpResponse {
    HttpResponse::json(
        400,
        "Bad Request",
        object([
            ("error", JsonValue::String("bad_request".to_string())),
            ("message", JsonValue::String(message.to_string())),
        ]),
    )
}

fn not_found(path: &str) -> HttpResponse {
    HttpResponse::json(
        404,
        "Not Found",
        object([
            ("error", JsonValue::String("not_found".to_string())),
            ("path", JsonValue::String(path.to_string())),
        ]),
    )
}

fn error_response(message: String) -> HttpResponse {
    let lower = message.to_ascii_lowercase();
    if lower.contains("not found") {
        return HttpResponse::json(
            404,
            "Not Found",
            object([
                ("error", JsonValue::String("not_found".to_string())),
                ("message", JsonValue::String(message)),
            ]),
        );
    }
    if lower.contains("invalid")
        || lower.contains("json")
        || lower.contains("request field")
        || lower.contains("must be")
        || lower.contains("missing")
    {
        return HttpResponse::json(
            400,
            "Bad Request",
            object([
                ("error", JsonValue::String("bad_request".to_string())),
                ("message", JsonValue::String(message)),
            ]),
        );
    }
    HttpResponse::json(
        500,
        "Internal Server Error",
        object([
            ("error", JsonValue::String("runtime_error".to_string())),
            ("message", JsonValue::String(message)),
        ]),
    )
}

fn request_body(request: &str) -> &str {
    request
        .split_once("\r\n\r\n")
        .map(|(_, body)| body)
        .or_else(|| request.split_once("\n\n").map(|(_, body)| body))
        .unwrap_or("")
}

fn query_param_u64(request_target: &str, key: &str) -> Option<u64> {
    let query = request_target.split_once('?')?.1;
    for pair in query.split('&') {
        let (pair_key, value) = pair.split_once('=').unwrap_or((pair, ""));
        if pair_key == key {
            return value.parse::<u64>().ok();
        }
    }
    None
}

fn query_param_string(request_target: &str, key: &str) -> Option<String> {
    let query = request_target.split_once('?')?.1;
    for pair in query.split('&') {
        let (pair_key, value) = pair.split_once('=').unwrap_or((pair, ""));
        if pair_key == key && !value.is_empty() {
            return Some(value.to_string());
        }
    }
    None
}

fn json_optional_string_field(
    root: &BTreeMap<String, JsonValue>,
    key: &str,
) -> AppResult<Option<String>> {
    match root.get(key) {
        Some(JsonValue::Null) | None => Ok(None),
        Some(value) => json_as_string(value)
            .map(|value| Some(value.to_string()))
            .ok_or_else(|| app_error(format!("request field `{key}` must be a string or null"))),
    }
}

fn json_usize_field(
    root: &BTreeMap<String, JsonValue>,
    key: &str,
    default: usize,
    max: usize,
) -> AppResult<usize> {
    match root.get(key) {
        None => Ok(default),
        Some(value) => {
            let Some(raw) = json_as_u64(value) else {
                return Err(app_error(format!("request field `{key}` must be a number")));
            };
            Ok(raw.min(max as u64) as usize)
        }
    }
}

fn json_bool_field(
    root: &BTreeMap<String, JsonValue>,
    key: &str,
    default: bool,
) -> AppResult<bool> {
    match root.get(key) {
        None => Ok(default),
        Some(JsonValue::Bool(value)) => Ok(*value),
        Some(_) => Err(app_error(format!(
            "request field `{key}` must be a boolean"
        ))),
    }
}

fn json_string_array_field(
    root: &BTreeMap<String, JsonValue>,
    key: &str,
) -> AppResult<Vec<String>> {
    let Some(value) = root.get(key) else {
        return Ok(Vec::new());
    };
    let Some(items) = json_as_array(value) else {
        return Err(app_error(format!("request field `{key}` must be an array")));
    };
    items
        .iter()
        .map(|item| {
            json_as_string(item)
                .map(str::to_string)
                .ok_or_else(|| app_error(format!("request field `{key}` items must be strings")))
        })
        .collect()
}

fn event_payload_object<'a>(
    root: &'a BTreeMap<String, JsonValue>,
) -> AppResult<&'a BTreeMap<String, JsonValue>> {
    match root.get("payload") {
        Some(value) => json_as_object(value)
            .ok_or_else(|| app_error("request field `payload` must be an object")),
        None => Ok(root),
    }
}

fn json_string_map_field(
    root: &BTreeMap<String, JsonValue>,
    key: &str,
) -> AppResult<BTreeMap<String, String>> {
    let Some(value) = root.get(key) else {
        return Ok(BTreeMap::new());
    };
    let Some(object) = json_as_object(value) else {
        return Err(app_error(format!(
            "request field `{key}` must be an object"
        )));
    };
    let mut result = BTreeMap::new();
    for (field, value) in object {
        let Some(value) = json_as_string(value) else {
            return Err(app_error(format!(
                "request field `{key}.{field}` must be a string"
            )));
        };
        result.insert(field.clone(), value.to_string());
    }
    Ok(result)
}

fn object<const N: usize>(items: [(&str, JsonValue); N]) -> JsonValue {
    let mut map = BTreeMap::new();
    for (key, value) in items {
        map.insert(key.to_string(), value);
    }
    JsonValue::Object(map)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::json::{json_as_array, json_as_object, json_as_string, parse_root_object};
    use std::fs;
    use std::io::{Read, Write};
    use std::net::TcpStream;
    use std::process::Command;
    use std::thread;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_store(label: &str) -> RuntimeStore {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        RuntimeStore::new(std::env::temp_dir().join(format!(
            "deepseek-serve-runtime-{label}-{}-{nanos}",
            std::process::id()
        )))
    }

    fn temp_dir(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "deepseek-serve-{label}-{}-{nanos}",
            std::process::id()
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn run_git(cwd: &Path, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn temp_git_repo(label: &str) -> PathBuf {
        let repo = temp_dir(label);
        run_git(&repo, &["init"]);
        run_git(&repo, &["config", "user.email", "test@example.com"]);
        run_git(&repo, &["config", "user.name", "DeepSeekCode Test"]);
        fs::write(repo.join("src.txt"), "base\n").unwrap();
        run_git(&repo, &["add", "src.txt"]);
        run_git(&repo, &["commit", "-m", "initial commit"]);
        repo
    }

    fn mcp_state(label: &str) -> McpStdioState {
        mcp_state_with_side_effects(label, false)
    }

    fn mcp_test_config(label: &str) -> AppConfig {
        let mut config = AppConfig::default();
        config.workspace.config_dir = temp_dir(&format!("{label}-config")).display().to_string();
        config
    }

    fn mcp_state_with_side_effects(label: &str, allow_side_effect_tools: bool) -> McpStdioState {
        McpStdioState {
            store: temp_store(label),
            rollback: RollbackStore::new(temp_dir(&format!("{label}-rollback"))),
            config: mcp_test_config(label),
            workspace: temp_dir(label),
            approval: ApprovalConfig::default(),
            diagnostics: DiagnosticsConfig::default(),
            approval_thread_id: None,
            approval_turn_id: None,
            approval_poll_interval: Duration::from_millis(1),
            approval_max_polls: Some(500),
            allow_side_effect_tools,
        }
    }

    fn mcp_state_with_durable_approvals(label: &str) -> McpStdioState {
        let store = temp_store(label);
        let workspace = temp_dir(label);
        let session = store
            .create_session("MCP approvals".to_string(), workspace.display().to_string())
            .unwrap();
        let thread = store
            .create_thread_for_session(
                &session.id,
                "MCP side-effect approvals".to_string(),
                workspace.display().to_string(),
                "deepseek-coder".to_string(),
                "mcp".to_string(),
            )
            .unwrap();
        McpStdioState {
            store,
            rollback: RollbackStore::new(temp_dir(&format!("{label}-rollback"))),
            config: mcp_test_config(label),
            workspace,
            approval: ApprovalConfig::default(),
            diagnostics: DiagnosticsConfig::default(),
            approval_thread_id: Some(thread.id),
            approval_turn_id: None,
            approval_poll_interval: Duration::from_millis(1),
            approval_max_polls: Some(500),
            allow_side_effect_tools: false,
        }
    }

    fn spawn_mcp_permission_responder(
        store: RuntimeStore,
        thread_id: String,
        decision: &'static str,
    ) -> thread::JoinHandle<()> {
        thread::spawn(move || {
            for _ in 0..100 {
                let events = store.read_events(&thread_id, 0).unwrap();
                if let Some(request) = events
                    .iter()
                    .filter(|event| event.kind == "permission_request")
                    .find(|request| {
                        !events.iter().any(|event| {
                            event.kind == "permission_response"
                                && json_as_object(&event.payload)
                                    .and_then(|payload| payload.get("request_id"))
                                    .and_then(json_as_string)
                                    == Some(request.id.as_str())
                        })
                    })
                {
                    store
                        .append_permission_response(
                            &thread_id,
                            None,
                            request.id.clone(),
                            decision.to_string(),
                        )
                        .unwrap();
                    return;
                }
                thread::sleep(Duration::from_millis(1));
            }
            panic!("permission request was not recorded");
        })
    }

    fn mcp_response_text(response: &JsonValue) -> String {
        let result = json_as_object(response)
            .and_then(|root| root.get("result"))
            .and_then(json_as_object)
            .expect("MCP response result object");
        let content = result
            .get("content")
            .and_then(json_as_array)
            .expect("MCP response content array");
        content
            .first()
            .and_then(json_as_object)
            .and_then(|item| item.get("text"))
            .and_then(json_as_string)
            .expect("MCP response text content")
            .to_string()
    }

    fn extract_task_id(text: &str) -> String {
        text.lines()
            .find_map(|line| line.strip_prefix("task_id: "))
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .expect("task_id line")
            .to_string()
    }

    fn acp_state(label: &str) -> AcpStdioState {
        let mut config = AppConfig::default();
        config.model.api_key_env = format!("DSCODE_TEST_NO_KEY_{label}");
        AcpStdioState {
            store: temp_store(label),
            rollback: RollbackStore::new(temp_dir(&format!("{label}-rollback"))),
            config,
            default_cwd: temp_dir(label),
            approval_poll_interval: Duration::from_millis(1),
            approval_max_polls: Some(500),
            sessions: BTreeMap::new(),
            next_session: 0,
        }
    }

    #[test]
    fn mcp_initialize_advertises_tools_capability() {
        let state = mcp_state("mcp-init");
        let response = mcp_response_for_message(
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
            &state,
        )
        .unwrap();
        let rendered = json_value_to_string(&response);

        assert!(rendered.contains(r#""protocolVersion":"2025-11-25""#));
        assert!(rendered.contains(r#""serverInfo":{"name":"DeepSeekCode""#));
        assert!(rendered.contains(r#""tools":{}"#));
    }

    #[test]
    fn mcp_tools_list_includes_workspace_and_runtime_tools() {
        let state = mcp_state("mcp-tools-list");
        let response = mcp_response_for_message(
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}"#,
            &state,
        )
        .unwrap();
        let rendered = json_value_to_string(&response);

        assert!(rendered.contains(r#""name":"read_file""#));
        assert!(rendered.contains(r#""name":"retrieve_tool_result""#));
        assert!(rendered.contains(r#""name":"list_dir""#));
        assert!(rendered.contains(r#""name":"grep_files""#));
        assert!(rendered.contains(r#""name":"file_search""#));
        assert!(rendered.contains(r#""name":"web_search""#));
        assert!(rendered.contains(r#""name":"web_run""#));
        assert!(rendered.contains(r#""name":"fetch_url""#));
        assert!(rendered.contains(r#""name":"finance""#));
        assert!(rendered.contains(r#""name":"pandoc_convert""#));
        assert!(rendered.contains(r#""name":"image_ocr""#));
        assert!(rendered.contains(r#""name":"git_status""#));
        assert!(rendered.contains(r#""name":"project_map""#));
        assert!(rendered.contains(r#""name":"validate_data""#));
        assert!(rendered.contains(r#""name":"git_log""#));
        assert!(rendered.contains(r#""name":"git_show""#));
        assert!(rendered.contains(r#""name":"git_blame""#));
        assert!(rendered.contains(r#""name":"github_issue_context""#));
        assert!(rendered.contains(r#""name":"github_pr_context""#));
        assert!(rendered.contains(r#""name":"review""#));
        assert!(rendered.contains(r#""name":"pr_review_comment_plan""#));
        assert!(rendered.contains(r#""name":"recall_archive""#));
        assert!(rendered.contains(r#""name":"tool_search_tool_regex""#));
        assert!(rendered.contains(r#""name":"tool_search_tool_bm25""#));
        assert!(rendered.contains(r#""name":"load_skill""#));
        assert!(rendered.contains(r#""name":"request_user_input""#));
        assert!(rendered.contains(r#""name":"notify""#));
        assert!(rendered.contains(r#""name":"exec_shell_list""#));
        assert!(rendered.contains(r#""name":"exec_shell_show""#));
        assert!(rendered.contains(r#""name":"exec_shell_wait""#));
        assert!(rendered.contains(r#""name":"exec_wait""#));
        assert!(rendered.contains(r#""name":"task_shell_wait""#));
        assert!(rendered.contains(r#""name":"rlm_chunk_plan""#));
        assert!(rendered.contains(r#""name":"rlm_map_reduce_plan""#));
        assert!(rendered.contains(r#""name":"rlm_recursive_plan""#));
        assert!(rendered.contains(r#""name":"rlm_python""#));
        assert!(rendered.contains(r#""name":"rlm_python_sessions""#));
        assert!(rendered.contains(r#""name":"diagnostics""#));
        assert!(rendered.contains(r#""name":"runtime_list_sessions""#));
        assert!(rendered.contains(r#""name":"runtime_list_agents""#));
        assert!(rendered.contains(r#""name":"runtime_agent_result""#));
        assert!(!rendered.contains(r#""name":"exec_shell""#));
        assert!(!rendered.contains(r#""name":"task_shell_start""#));
        assert!(!rendered.contains(r#""name":"exec_shell_interact""#));
        assert!(!rendered.contains(r#""name":"exec_interact""#));
        assert!(!rendered.contains(r#""name":"exec_shell_cancel""#));
        assert!(!rendered.contains(r#""name":"rlm_python_session""#));
        assert!(!rendered.contains(r#""name":"rlm""#));
        assert!(!rendered.contains(r#""name":"rlm_query""#));
        assert!(!rendered.contains(r#""name":"llm_query""#));
        assert!(!rendered.contains(r#""name":"rlm_process""#));
        assert!(!rendered.contains(r#""name":"rlm_batch""#));
        assert!(!rendered.contains(r#""name":"rlm_query_batched""#));
        assert!(!rendered.contains(r#""name":"llm_query_batched""#));
        assert!(!rendered.contains(r#""name":"run_shell""#));
        assert!(!rendered.contains(r#""name":"run_tests""#));
        assert!(!rendered.contains(r#""name":"image_analyze""#));
        assert!(!rendered.contains(r#""name":"note""#));
        assert!(!rendered.contains(r#""name":"remember""#));
    }

    #[test]
    fn mcp_tools_list_includes_run_shell_when_side_effects_enabled() {
        let state = mcp_state_with_side_effects("mcp-tools-list-side-effects", true);
        let response = mcp_response_for_message(
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}"#,
            &state,
        )
        .unwrap();
        let rendered = json_value_to_string(&response);

        assert!(rendered.contains(r#""name":"run_shell""#));
        assert!(rendered.contains(r#""name":"run_tests""#));
        assert!(rendered.contains(r#""name":"exec_shell""#));
        assert!(rendered.contains(r#""name":"task_shell_start""#));
        assert!(rendered.contains(r#""name":"exec_shell_interact""#));
        assert!(rendered.contains(r#""name":"exec_interact""#));
        assert!(rendered.contains(r#""name":"exec_shell_cancel""#));
        assert!(rendered.contains(r#""name":"rlm_python_session""#));
        assert!(rendered.contains(r#""name":"rlm""#));
        assert!(rendered.contains(r#""name":"rlm_query""#));
        assert!(rendered.contains(r#""name":"llm_query""#));
        assert!(rendered.contains(r#""name":"rlm_process""#));
        assert!(rendered.contains(r#""name":"rlm_batch""#));
        assert!(rendered.contains(r#""name":"rlm_query_batched""#));
        assert!(rendered.contains(r#""name":"llm_query_batched""#));
        assert!(rendered.contains(r#""name":"image_analyze""#));
    }

    #[test]
    fn mcp_prompts_list_includes_builtin_workflows() {
        let state = mcp_state("mcp-prompts-list");
        let response = mcp_response_for_message(
            r#"{"jsonrpc":"2.0","id":3,"method":"prompts/list","params":{}}"#,
            &state,
        )
        .unwrap();
        let rendered = json_value_to_string(&response);

        assert!(rendered.contains(r#""prompts":["#) || rendered.contains(r#""prompts":[{"#));
        assert!(rendered.contains(r#""name":"review_code""#));
        assert!(rendered.contains(r#""name":"explain_code""#));
        assert!(rendered.contains(r#""name":"plan_task""#));
        assert!(rendered.contains(r#""required":true"#));
    }

    #[test]
    fn mcp_prompts_get_renders_builtin_prompt_messages() {
        let state = mcp_state("mcp-prompts-get");
        let response = mcp_response_for_message(
            r#"{"jsonrpc":"2.0","id":4,"method":"prompts/get","params":{"name":"review_code","arguments":{"path":"src/lib.rs","focus":"tests"}}}"#,
            &state,
        )
        .unwrap();
        let rendered = json_value_to_string(&response);

        assert!(rendered.contains("Review a file or code area"));
        assert!(rendered.contains("Review `src/lib.rs`"));
        assert!(rendered.contains("Focus: tests"));
        assert!(rendered.contains(r#""messages""#));
        assert!(rendered.contains(r#""role":"user""#));
    }

    #[test]
    fn mcp_prompts_get_rejects_missing_required_argument() {
        let state = mcp_state("mcp-prompts-get-missing-arg");
        let response = mcp_response_for_message(
            r#"{"jsonrpc":"2.0","id":5,"method":"prompts/get","params":{"name":"review_code","arguments":{}}}"#,
            &state,
        )
        .unwrap();
        let rendered = json_value_to_string(&response);

        assert!(rendered.contains(r#""code":-32602"#));
        assert!(rendered.contains("MCP prompt requires string argument `path`"));
    }

    #[test]
    fn mcp_tools_call_executes_read_file() {
        let state = mcp_state("mcp-read-file");
        let root =
            std::env::temp_dir().join(format!("deepseek-mcp-read-file-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let file = root.join("note.txt");
        fs::write(&file, "hello from mcp\nsecond line\n").unwrap();
        let request = format!(
            r#"{{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{{"name":"read_file","arguments":{{"path":"{}","max_lines":1}}}}}}"#,
            crate::util::json::json_escape(&file.display().to_string())
        );

        let response = mcp_response_for_message(&request, &state).unwrap();
        let rendered = json_value_to_string(&response);

        assert!(rendered.contains("hello from mcp"));
        assert!(rendered.contains(r#""isError":false"#));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn mcp_tools_call_executes_rlm_chunk_plan() {
        let state = mcp_state("mcp-rlm-chunk-plan");
        let response = mcp_response_for_message(
            r#"{"jsonrpc":"2.0","id":34,"method":"tools/call","params":{"name":"rlm_chunk_plan","arguments":{"content":"abcdef","max_chars":3,"overlap":0,"include_text":"false"}}}"#,
            &state,
        )
        .unwrap();
        let text = mcp_response_text(&response);

        assert!(text.contains(r#""chunks""#));
        assert!(text.contains(r#""coverage":{"chunks":2"#));
        assert!(text.contains(r#""include_text":false"#));
    }

    #[test]
    fn mcp_tools_call_executes_rlm_recursive_plan() {
        let state = mcp_state("mcp-rlm-recursive-plan");
        let response = mcp_response_for_message(
            r#"{"jsonrpc":"2.0","id":38,"method":"tools/call","params":{"name":"rlm_recursive_plan","arguments":{"task":"summarize","content":"abcdefghij","max_chars":2,"fan_in":2,"include_text":"false"}}}"#,
            &state,
        )
        .unwrap();
        let text = mcp_response_text(&response);

        assert!(text.contains(r#""rounds":["#));
        assert!(text.contains(r#""input_refs":["map:0","map:1"]"#));
        assert!(text.contains(r#""final_output_ref":"round3:group0""#));
    }

    #[test]
    fn mcp_tools_call_executes_web_run_without_network_for_unsupported_actions() {
        let state = mcp_state("mcp-web-run");
        let response = mcp_response_for_message(
            r#"{"jsonrpc":"2.0","id":37,"method":"tools/call","params":{"name":"web_run","arguments":{"weather":[{"location":"San Francisco"}]}}}"#,
            &state,
        )
        .unwrap();
        let rendered = json_value_to_string(&response);

        assert!(rendered.contains("meta.tool=web_run"));
        assert!(rendered.contains("meta.unsupported_actions=weather"));
        assert!(rendered.contains("Unsupported web_run actions in this slice: weather"));
        assert!(rendered.contains(r#""isError":false"#));
    }

    #[test]
    fn mcp_tools_call_executes_read_only_helper_tools() {
        let state = mcp_state("mcp-read-only-helpers");
        fs::write(
            state.workspace.join("src.rs"),
            "pub fn risky(value: Option<String>) -> String {\n    value.unwrap()\n}\n",
        )
        .unwrap();
        let runtime_store =
            RuntimeStore::new(PathBuf::from(&state.config.workspace.config_dir).join("runtime"));
        let session = runtime_store
            .create_session(
                "Recall session".to_string(),
                state.workspace.display().to_string(),
            )
            .unwrap();
        let thread = runtime_store
            .create_thread_for_session(
                &session.id,
                "Recall thread".to_string(),
                state.workspace.display().to_string(),
                "deepseek-coder".to_string(),
                "agent".to_string(),
            )
            .unwrap();
        runtime_store
            .append_turn(
                &thread.id,
                "assistant".to_string(),
                "needle archive context".to_string(),
            )
            .unwrap();

        let review_response = mcp_response_for_message(
            r#"{"jsonrpc":"2.0","id":38,"method":"tools/call","params":{"name":"review","arguments":{"target":"src.rs"}}}"#,
            &state,
        )
        .unwrap();
        let review_text = mcp_response_text(&review_response);
        assert!(review_text.contains("Reviewed file target `src.rs`"));
        assert!(review_text.contains("panic-prone error handling"));

        let comment_plan_request = format!(
            r#"{{"jsonrpc":"2.0","id":41,"method":"tools/call","params":{{"name":"pr_review_comment_plan","arguments":{{"review_output":"{}","number":"7","repo":"owner/repo"}}}}}}"#,
            crate::util::json::json_escape(&review_text)
        );
        let comment_plan_response =
            mcp_response_for_message(&comment_plan_request, &state).unwrap();
        let comment_plan_text = mcp_response_text(&comment_plan_response);
        assert!(comment_plan_text.contains("## Automated PR Review"));
        assert!(comment_plan_text.contains(r#""github_comment_input""#));
        assert!(comment_plan_text.contains(r#""dry_run":"true""#));

        let recall_response = mcp_response_for_message(
            r#"{"jsonrpc":"2.0","id":39,"method":"tools/call","params":{"name":"recall_archive","arguments":{"query":"needle","max_results":1}}}"#,
            &state,
        )
        .unwrap();
        let recall_text = mcp_response_text(&recall_response);
        assert!(recall_text.contains(r#""messages_scanned":1"#));
        assert!(recall_text.contains("needle archive context"));

        let tool_search_response = mcp_response_for_message(
            r#"{"jsonrpc":"2.0","id":40,"method":"tools/call","params":{"name":"tool_search_tool_bm25","arguments":{"query":"market quote","limit":3}}}"#,
            &state,
        )
        .unwrap();
        let tool_search_text = mcp_response_text(&tool_search_response);
        assert!(tool_search_text.contains(r#""tool":"tool_search_tool_bm25""#));
        assert!(tool_search_text.contains(r#""tool_name":"finance""#));
    }

    #[test]
    fn mcp_tools_call_executes_interactive_helper_tools() {
        let mut state = mcp_state("mcp-interactive-helpers");
        let skill_dir = state.workspace.join("skills");
        fs::create_dir_all(&skill_dir).unwrap();
        fs::write(
            skill_dir.join("triage.toml"),
            r#"name = "triage"
description = "Triage code changes"
allowed_tools = ["read_file"]
system_append = "Focus on concrete evidence."
suggested_steps = ["Read context", "Summarize risks"]
triggers = ["triage"]
references = ["docs/runtime.md"]

[policy]
require_write_confirmation = true
require_shell_confirmation = true
shell_allowlist = ["cargo test"]
"#,
        )
        .unwrap();
        state.config.workspace.user_skills_dir = skill_dir.display().to_string();

        let skill_response = mcp_response_for_message(
            r#"{"jsonrpc":"2.0","id":44,"method":"tools/call","params":{"name":"load_skill","arguments":{"name":"triage"}}}"#,
            &state,
        )
        .unwrap();
        let skill_text = mcp_response_text(&skill_response);
        assert!(skill_text.contains("# Skill: triage"));
        assert!(skill_text.contains("Focus on concrete evidence."));

        let input_response = mcp_response_for_message(
            r#"{"jsonrpc":"2.0","id":45,"method":"tools/call","params":{"name":"request_user_input","arguments":{"questions":[{"header":"Mode","id":"mode","question":"Which mode?","options":[{"label":"Plan","description":"Draft first."},{"label":"Apply","description":"Implement now."}]}]}}}"#,
            &state,
        )
        .unwrap();
        let input_text = mcp_response_text(&input_response);
        assert!(input_text.contains("meta.user_input_required=true"));
        assert!(input_text.contains("[mode] Mode"));
        assert!(input_text.contains("- Plan: Draft first."));

        std::env::set_var("DSCODE_NOTIFY", "off");
        let notify_response = mcp_response_for_message(
            r#"{"jsonrpc":"2.0","id":46,"method":"tools/call","params":{"name":"notify","arguments":{"title":"done","body":"tests pass"}}}"#,
            &state,
        )
        .unwrap();
        std::env::remove_var("DSCODE_NOTIFY");
        let notify_text = mcp_response_text(&notify_response);
        assert!(notify_text.contains("notified: done - tests pass"));
    }

    #[test]
    fn mcp_tools_call_handles_document_helpers_before_spawning_binaries() {
        let state = mcp_state("mcp-document-helpers");
        fs::write(state.workspace.join("note.md"), "# hello\n").unwrap();

        let unsupported = mcp_response_for_message(
            r#"{"jsonrpc":"2.0","id":41,"method":"tools/call","params":{"name":"pandoc_convert","arguments":{"source_path":"note.md","target_format":"not-real"}}}"#,
            &state,
        )
        .unwrap();
        let unsupported_rendered = json_value_to_string(&unsupported);
        assert!(unsupported_rendered.contains("unsupported target_format"));
        assert!(unsupported_rendered.contains(r#""isError":true"#));

        let disabled_write = mcp_response_for_message(
            r#"{"jsonrpc":"2.0","id":42,"method":"tools/call","params":{"name":"pandoc_convert","arguments":{"source_path":"note.md","target_format":"html","output_path":"out.html"}}}"#,
            &state,
        )
        .unwrap();
        let disabled_rendered = json_value_to_string(&disabled_write);
        assert!(disabled_rendered.contains("MCP write mode for `pandoc_convert` is disabled"));
        assert!(disabled_rendered.contains(r#""isError":true"#));

        let missing_image = mcp_response_for_message(
            r#"{"jsonrpc":"2.0","id":43,"method":"tools/call","params":{"name":"image_ocr","arguments":{"path":"missing.png"}}}"#,
            &state,
        )
        .unwrap();
        let missing_rendered = json_value_to_string(&missing_image);
        assert!(missing_rendered.contains("image_ocr source path does not exist"));
        assert!(missing_rendered.contains(r#""isError":true"#));
    }

    #[test]
    fn mcp_tools_call_rejects_rlm_python_session_until_side_effects_enabled() {
        let state = mcp_state("mcp-rlm-python-session-disabled");
        let response = mcp_response_for_message(
            r#"{"jsonrpc":"2.0","id":35,"method":"tools/call","params":{"name":"rlm_python_session","arguments":{"session_id":"demo","code":"FINAL(1)"}}}"#,
            &state,
        )
        .unwrap();
        let rendered = json_value_to_string(&response);

        assert!(rendered.contains("MCP RLM state tool `rlm_python_session` is disabled"));
        assert!(rendered.contains(r#""isError":true"#));
    }

    #[test]
    fn mcp_tools_call_rejects_rlm_python_blocked_tokens() {
        let state = mcp_state("mcp-rlm-python-blocked");
        let response = mcp_response_for_message(
            r#"{"jsonrpc":"2.0","id":36,"method":"tools/call","params":{"name":"rlm_python","arguments":{"code":"import os\nFINAL(os.getcwd())"}}}"#,
            &state,
        )
        .unwrap();
        let rendered = json_value_to_string(&response);

        assert!(rendered.contains("rlm_python rejects blocked token"));
        assert!(rendered.contains(r#""isError":true"#));
    }

    #[test]
    fn mcp_tools_call_rejects_model_rlm_until_side_effects_enabled() {
        let state = mcp_state("mcp-model-rlm-disabled");
        let response = mcp_response_for_message(
            r#"{"jsonrpc":"2.0","id":37,"method":"tools/call","params":{"name":"rlm","arguments":{"context":"alpha","question":"summarize"}}}"#,
            &state,
        )
        .unwrap();
        let rendered = json_value_to_string(&response);

        assert!(rendered.contains("MCP model-running RLM tool `rlm` is disabled"));
        assert!(rendered.contains(r#""isError":true"#));
    }

    #[test]
    fn mcp_tools_call_rejects_model_rlm_after_runtime_denial() {
        let state = mcp_state_with_durable_approvals("mcp-model-rlm-denied");
        let thread_id = state.approval_thread_id.clone().unwrap();
        let responder =
            spawn_mcp_permission_responder(state.store.clone(), thread_id.clone(), "denied");
        let response = mcp_response_for_message(
            r#"{"jsonrpc":"2.0","id":38,"method":"tools/call","params":{"name":"rlm","arguments":{"context":"alpha","question":"summarize"}}}"#,
            &state,
        )
        .unwrap();
        responder.join().unwrap();
        let rendered = json_value_to_string(&response);

        assert!(rendered.contains("MCP rlm denied by runtime approval"));
        assert!(rendered.contains(r#""isError":true"#));
        let events = state.store.read_events(&thread_id, 0).unwrap();
        assert!(events.iter().any(|event| {
            event.kind == "permission_request"
                && json_as_object(&event.payload).is_some_and(|payload| {
                    payload.get("tool").and_then(json_as_string) == Some("rlm")
                        && payload.get("kind").and_then(json_as_string) == Some("mcp")
                })
        }));
    }

    #[test]
    fn mcp_tools_call_rejects_image_analyze_until_side_effects_enabled() {
        let state = mcp_state("mcp-image-analyze-disabled");
        let response = mcp_response_for_message(
            r#"{"jsonrpc":"2.0","id":47,"method":"tools/call","params":{"name":"image_analyze","arguments":{"image_path":"image.png"}}}"#,
            &state,
        )
        .unwrap();
        let rendered = json_value_to_string(&response);

        assert!(rendered.contains("MCP model-running vision tool `image_analyze` is disabled"));
        assert!(rendered.contains(r#""isError":true"#));

        let state = mcp_state_with_side_effects("mcp-image-analyze-validation", true);
        let response = mcp_response_for_message(
            r#"{"jsonrpc":"2.0","id":48,"method":"tools/call","params":{"name":"image_analyze","arguments":{"image_path":"../secret.png"}}}"#,
            &state,
        )
        .unwrap();
        let rendered = json_value_to_string(&response);
        assert!(rendered.contains("unsafe image_analyze path outside workspace"));
        assert!(rendered.contains(r#""isError":true"#));
    }

    #[test]
    fn mcp_tools_call_rejects_image_analyze_after_runtime_denial() {
        let state = mcp_state_with_durable_approvals("mcp-image-analyze-denied");
        let thread_id = state.approval_thread_id.clone().unwrap();
        let responder =
            spawn_mcp_permission_responder(state.store.clone(), thread_id.clone(), "denied");
        let response = mcp_response_for_message(
            r#"{"jsonrpc":"2.0","id":49,"method":"tools/call","params":{"name":"image_analyze","arguments":{"image_path":"image.png","prompt":"describe"}}}"#,
            &state,
        )
        .unwrap();
        responder.join().unwrap();
        let rendered = json_value_to_string(&response);

        assert!(rendered.contains("MCP image_analyze denied by runtime approval"));
        assert!(rendered.contains(r#""isError":true"#));
        let events = state.store.read_events(&thread_id, 0).unwrap();
        assert!(events.iter().any(|event| {
            event.kind == "permission_request"
                && json_as_object(&event.payload).is_some_and(|payload| {
                    payload.get("tool").and_then(json_as_string) == Some("image_analyze")
                        && payload.get("kind").and_then(json_as_string) == Some("mcp")
                })
        }));
    }

    #[test]
    fn mcp_tools_call_rejects_memory_writes_until_durable_approvals() {
        let state = mcp_state("mcp-memory-writes-disabled");
        let note_response = mcp_response_for_message(
            r#"{"jsonrpc":"2.0","id":50,"method":"tools/call","params":{"name":"note","arguments":{"content":"keep this"}}}"#,
            &state,
        )
        .unwrap();
        let note_rendered = json_value_to_string(&note_response);
        assert!(note_rendered.contains("MCP write tool `note` is disabled"));
        assert!(note_rendered.contains(r#""isError":true"#));

        let remember_response = mcp_response_for_message(
            r#"{"jsonrpc":"2.0","id":51,"method":"tools/call","params":{"name":"remember","arguments":{"note":"prefer short tests"}}}"#,
            &state,
        )
        .unwrap();
        let remember_rendered = json_value_to_string(&remember_response);
        assert!(remember_rendered.contains("MCP write tool `remember` is disabled"));
        assert!(remember_rendered.contains(r#""isError":true"#));
    }

    #[test]
    fn mcp_tools_call_executes_memory_writes_after_runtime_approval() {
        let mut state = mcp_state_with_durable_approvals("mcp-memory-writes-approval");
        let thread_id = state.approval_thread_id.clone().unwrap();
        let notes_path = state.workspace.join("notes.md");
        let memory_path = state.workspace.join("memory.md");
        state.config.memory.enabled = true;
        state.config.memory.notes_path = notes_path.display().to_string();
        state.config.memory.memory_path = memory_path.display().to_string();

        let list_response = mcp_response_for_message(
            r#"{"jsonrpc":"2.0","id":52,"method":"tools/list","params":{}}"#,
            &state,
        )
        .unwrap();
        let list_rendered = json_value_to_string(&list_response);
        assert!(list_rendered.contains(r#""name":"note""#));
        assert!(list_rendered.contains(r#""name":"remember""#));

        let responder =
            spawn_mcp_permission_responder(state.store.clone(), thread_id.clone(), "approved");
        let note_response = mcp_response_for_message(
            r#"{"jsonrpc":"2.0","id":53,"method":"tools/call","params":{"name":"note","arguments":{"content":"record release decision"}}}"#,
            &state,
        )
        .unwrap();
        responder.join().unwrap();
        let note_text = mcp_response_text(&note_response);
        assert!(note_text.contains("Note appended"));
        assert!(fs::read_to_string(&notes_path)
            .unwrap()
            .contains("record release decision"));

        let responder =
            spawn_mcp_permission_responder(state.store.clone(), thread_id.clone(), "approved");
        let remember_response = mcp_response_for_message(
            r##"{"jsonrpc":"2.0","id":54,"method":"tools/call","params":{"name":"remember","arguments":{"note":"# prefer cargo fmt"}}}"##,
            &state,
        )
        .unwrap();
        responder.join().unwrap();
        let remember_text = mcp_response_text(&remember_response);
        assert!(remember_text.contains("remembered: prefer cargo fmt"));
        let memory_content = fs::read_to_string(&memory_path).unwrap();
        assert!(memory_content.contains("prefer cargo fmt"));
        assert!(!memory_content.contains("# prefer cargo fmt"));

        let events = state.store.read_events(&thread_id, 0).unwrap();
        assert!(events.iter().any(|event| {
            event.kind == "permission_request"
                && json_as_object(&event.payload).is_some_and(|payload| {
                    payload.get("tool").and_then(json_as_string) == Some("note")
                        && payload.get("kind").and_then(json_as_string) == Some("write")
                })
        }));
        assert!(events.iter().any(|event| {
            event.kind == "permission_request"
                && json_as_object(&event.payload).is_some_and(|payload| {
                    payload.get("tool").and_then(json_as_string) == Some("remember")
                        && payload.get("kind").and_then(json_as_string) == Some("write")
                })
        }));
    }

    #[test]
    fn mcp_tools_call_rejects_run_shell_until_side_effects_enabled() {
        let state = mcp_state("mcp-run-shell-disabled");
        let response = mcp_response_for_message(
            r#"{"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"run_shell","arguments":{"command":"pwd"}}}"#,
            &state,
        )
        .unwrap();
        let rendered = json_value_to_string(&response);

        assert!(rendered.contains("MCP side-effect tool `run_shell` is disabled"));
        assert!(rendered.contains(r#""isError":true"#));
    }

    #[test]
    fn mcp_tools_call_executes_run_shell_when_side_effects_enabled() {
        let state = mcp_state_with_side_effects("mcp-run-shell-enabled", true);
        let response = mcp_response_for_message(
            r#"{"jsonrpc":"2.0","id":8,"method":"tools/call","params":{"name":"run_shell","arguments":{"command":"pwd"}}}"#,
            &state,
        )
        .unwrap();
        let rendered = json_value_to_string(&response);

        assert!(rendered.contains("meta.command_kind=other"));
        assert!(rendered.contains(r#""isError":false"#));
    }

    #[test]
    fn mcp_tools_call_rejects_unsafe_shell_session_before_runtime_approval() {
        let state = mcp_state_with_durable_approvals("mcp-shell-session-unsafe");
        let thread_id = state.approval_thread_id.clone().unwrap();
        let response = mcp_response_for_message(
            r#"{"jsonrpc":"2.0","id":33,"method":"tools/call","params":{"name":"task_shell_start","arguments":{"command":"rm -rf ."}}}"#,
            &state,
        )
        .unwrap();
        let rendered = json_value_to_string(&response);
        assert!(rendered.contains("command not allowed: rm -rf ."));
        assert!(rendered.contains(r#""isError":true"#));
        let events = state.store.read_events(&thread_id, 0).unwrap();
        assert!(!events
            .iter()
            .any(|event| event.kind == "permission_request"));
    }

    #[test]
    fn mcp_tools_list_includes_run_shell_when_durable_approvals_enabled() {
        let state = mcp_state_with_durable_approvals("mcp-run-shell-approval-list");
        let response = mcp_response_for_message(
            r#"{"jsonrpc":"2.0","id":9,"method":"tools/list","params":{}}"#,
            &state,
        )
        .unwrap();
        let rendered = json_value_to_string(&response);

        assert!(rendered.contains(r#""name":"run_shell""#));
        assert!(rendered.contains("durable runtime approvals"));
    }

    #[test]
    fn mcp_tools_list_includes_write_tools_only_with_durable_approvals() {
        let state = mcp_state("mcp-apply-patch-hidden");
        let response = mcp_response_for_message(
            r#"{"jsonrpc":"2.0","id":9,"method":"tools/list","params":{}}"#,
            &state,
        )
        .unwrap();
        let rendered = json_value_to_string(&response);
        assert!(!rendered.contains(r#""name":"apply_patch""#));
        assert!(!rendered.contains(r#""name":"write_file""#));
        assert!(!rendered.contains(r#""name":"edit_file""#));
        assert!(!rendered.contains(r#""name":"delete_file""#));
        assert!(!rendered.contains(r#""name":"copy_file""#));
        assert!(!rendered.contains(r#""name":"move_file""#));
        assert!(!rendered.contains(r#""name":"revert_turn""#));
        assert!(!rendered.contains(r#""name":"github_comment""#));
        assert!(!rendered.contains(r#""name":"github_close_issue""#));
        assert!(!rendered.contains(r#""name":"runtime_create_task""#));
        assert!(!rendered.contains(r#""name":"runtime_cancel_task""#));
        assert!(!rendered.contains(r#""name":"runtime_create_automation""#));
        assert!(!rendered.contains(r#""name":"runtime_update_automation""#));
        assert!(!rendered.contains(r#""name":"runtime_pause_automation""#));
        assert!(!rendered.contains(r#""name":"runtime_resume_automation""#));
        assert!(!rendered.contains(r#""name":"runtime_delete_automation""#));
        assert!(!rendered.contains(r#""name":"runtime_trigger_automation""#));
        assert!(!rendered.contains(r#""name":"runtime_spawn_agent""#));
        assert!(!rendered.contains(r#""name":"runtime_cancel_agent""#));
        assert!(!rendered.contains(r#""name":"runtime_close_agent""#));
        assert!(!rendered.contains(r#""name":"runtime_resume_agent""#));
        assert!(!rendered.contains(r#""name":"runtime_send_agent_input""#));
        assert!(!rendered.contains(r#""name":"exec_shell""#));
        assert!(!rendered.contains(r#""name":"task_shell_start""#));
        assert!(!rendered.contains(r#""name":"exec_shell_interact""#));
        assert!(!rendered.contains(r#""name":"exec_interact""#));
        assert!(!rendered.contains(r#""name":"exec_shell_cancel""#));
        assert!(!rendered.contains(r#""name":"rlm_python_session""#));
        assert!(!rendered.contains(r#""name":"rlm""#));
        assert!(!rendered.contains(r#""name":"rlm_query""#));
        assert!(!rendered.contains(r#""name":"llm_query""#));
        assert!(!rendered.contains(r#""name":"rlm_process""#));
        assert!(!rendered.contains(r#""name":"rlm_batch""#));
        assert!(!rendered.contains(r#""name":"rlm_query_batched""#));
        assert!(!rendered.contains(r#""name":"llm_query_batched""#));

        let state = mcp_state_with_side_effects("mcp-apply-patch-side-effects", true);
        let response = mcp_response_for_message(
            r#"{"jsonrpc":"2.0","id":10,"method":"tools/list","params":{}}"#,
            &state,
        )
        .unwrap();
        let rendered = json_value_to_string(&response);
        assert!(rendered.contains(r#""name":"run_shell""#));
        assert!(rendered.contains(r#""name":"exec_shell""#));
        assert!(rendered.contains(r#""name":"task_shell_start""#));
        assert!(rendered.contains(r#""name":"exec_shell_interact""#));
        assert!(rendered.contains(r#""name":"exec_interact""#));
        assert!(rendered.contains(r#""name":"exec_shell_cancel""#));
        assert!(rendered.contains(r#""name":"rlm_python_session""#));
        assert!(rendered.contains(r#""name":"rlm""#));
        assert!(rendered.contains(r#""name":"rlm_query""#));
        assert!(rendered.contains(r#""name":"llm_query""#));
        assert!(rendered.contains(r#""name":"rlm_process""#));
        assert!(rendered.contains(r#""name":"rlm_batch""#));
        assert!(rendered.contains(r#""name":"rlm_query_batched""#));
        assert!(rendered.contains(r#""name":"llm_query_batched""#));
        assert!(!rendered.contains(r#""name":"apply_patch""#));
        assert!(!rendered.contains(r#""name":"write_file""#));
        assert!(!rendered.contains(r#""name":"edit_file""#));
        assert!(!rendered.contains(r#""name":"delete_file""#));
        assert!(!rendered.contains(r#""name":"copy_file""#));
        assert!(!rendered.contains(r#""name":"move_file""#));
        assert!(!rendered.contains(r#""name":"revert_turn""#));
        assert!(!rendered.contains(r#""name":"github_comment""#));
        assert!(!rendered.contains(r#""name":"github_close_issue""#));
        assert!(!rendered.contains(r#""name":"runtime_create_task""#));
        assert!(!rendered.contains(r#""name":"runtime_cancel_task""#));
        assert!(!rendered.contains(r#""name":"runtime_create_automation""#));
        assert!(!rendered.contains(r#""name":"runtime_update_automation""#));
        assert!(!rendered.contains(r#""name":"runtime_pause_automation""#));
        assert!(!rendered.contains(r#""name":"runtime_resume_automation""#));
        assert!(!rendered.contains(r#""name":"runtime_delete_automation""#));
        assert!(!rendered.contains(r#""name":"runtime_trigger_automation""#));
        assert!(!rendered.contains(r#""name":"runtime_spawn_agent""#));
        assert!(!rendered.contains(r#""name":"runtime_cancel_agent""#));
        assert!(!rendered.contains(r#""name":"runtime_close_agent""#));
        assert!(!rendered.contains(r#""name":"runtime_resume_agent""#));
        assert!(!rendered.contains(r#""name":"runtime_send_agent_input""#));

        let state = mcp_state_with_durable_approvals("mcp-apply-patch-visible");
        let response = mcp_response_for_message(
            r#"{"jsonrpc":"2.0","id":11,"method":"tools/list","params":{}}"#,
            &state,
        )
        .unwrap();
        let rendered = json_value_to_string(&response);
        assert!(rendered.contains(r#""name":"apply_patch""#));
        assert!(rendered.contains(r#""name":"write_file""#));
        assert!(rendered.contains(r#""name":"note""#));
        assert!(!rendered.contains(r#""name":"remember""#));
        assert!(rendered.contains(r#""name":"edit_file""#));
        assert!(rendered.contains(r#""name":"delete_file""#));
        assert!(rendered.contains(r#""name":"copy_file""#));
        assert!(rendered.contains(r#""name":"move_file""#));
        assert!(rendered.contains(r#""name":"revert_turn""#));
        assert!(rendered.contains(r#""name":"github_comment""#));
        assert!(rendered.contains(r#""name":"github_close_issue""#));
        assert!(rendered.contains(r#""name":"runtime_create_task""#));
        assert!(rendered.contains(r#""name":"runtime_cancel_task""#));
        assert!(rendered.contains(r#""name":"runtime_create_automation""#));
        assert!(rendered.contains(r#""name":"runtime_update_automation""#));
        assert!(rendered.contains(r#""name":"runtime_pause_automation""#));
        assert!(rendered.contains(r#""name":"runtime_resume_automation""#));
        assert!(rendered.contains(r#""name":"runtime_delete_automation""#));
        assert!(rendered.contains(r#""name":"runtime_trigger_automation""#));
        assert!(rendered.contains(r#""name":"runtime_spawn_agent""#));
        assert!(rendered.contains(r#""name":"runtime_cancel_agent""#));
        assert!(rendered.contains(r#""name":"runtime_close_agent""#));
        assert!(rendered.contains(r#""name":"runtime_resume_agent""#));
        assert!(rendered.contains(r#""name":"runtime_send_agent_input""#));
        assert!(rendered.contains(r#""name":"exec_shell""#));
        assert!(rendered.contains(r#""name":"task_shell_start""#));
        assert!(rendered.contains(r#""name":"exec_shell_interact""#));
        assert!(rendered.contains(r#""name":"exec_interact""#));
        assert!(rendered.contains(r#""name":"exec_shell_cancel""#));
        assert!(rendered.contains(r#""name":"rlm_python_session""#));
        assert!(rendered.contains(r#""name":"rlm""#));
        assert!(rendered.contains(r#""name":"rlm_query""#));
        assert!(rendered.contains(r#""name":"llm_query""#));
        assert!(rendered.contains(r#""name":"rlm_process""#));
        assert!(rendered.contains(r#""name":"rlm_batch""#));
        assert!(rendered.contains(r#""name":"rlm_query_batched""#));
        assert!(rendered.contains(r#""name":"llm_query_batched""#));
        assert!(rendered.contains(r#""name":"image_analyze""#));
        assert!(rendered.contains("durable runtime approvals"));
    }

    #[test]
    fn mcp_tools_call_starts_waits_and_cancels_shell_session_after_runtime_approval() {
        let state = mcp_state_with_durable_approvals("mcp-shell-session-approval");
        let thread_id = state.approval_thread_id.clone().unwrap();
        let responder =
            spawn_mcp_permission_responder(state.store.clone(), thread_id.clone(), "approved");
        let start_request = r#"{"jsonrpc":"2.0","id":30,"method":"tools/call","params":{"name":"task_shell_start","arguments":{"command":"pwd"}}}"#;
        let start_response = mcp_response_for_message(start_request, &state).unwrap();
        responder.join().unwrap();
        let start_text = mcp_response_text(&start_response);
        assert!(start_text.contains("task_id:"));
        assert!(start_text.contains("meta.task_shell=true"));
        let task_id = extract_task_id(&start_text);

        let wait_request = format!(
            r#"{{"jsonrpc":"2.0","id":31,"method":"tools/call","params":{{"name":"task_shell_wait","arguments":{{"task_id":"{}","timeout_ms":5000}}}}}}"#,
            crate::util::json::json_escape(&task_id)
        );
        let wait_response = mcp_response_for_message(&wait_request, &state).unwrap();
        let wait_text = mcp_response_text(&wait_response);
        assert!(wait_text.contains("status: completed"));
        assert!(wait_text.contains("stdout_total_bytes:"));

        let responder =
            spawn_mcp_permission_responder(state.store.clone(), thread_id.clone(), "approved");
        let cancel_request = format!(
            r#"{{"jsonrpc":"2.0","id":32,"method":"tools/call","params":{{"name":"exec_shell_cancel","arguments":{{"task_id":"{}"}}}}}}"#,
            crate::util::json::json_escape(&task_id)
        );
        let cancel_response = mcp_response_for_message(&cancel_request, &state).unwrap();
        responder.join().unwrap();
        let cancel_text = mcp_response_text(&cancel_response);
        assert!(cancel_text.contains("Canceled background shell job"));

        let events = state.store.read_events(&thread_id, 0).unwrap();
        let shell_request_for = |tool: &str| {
            events.iter().any(|event| {
                event.kind == "permission_request"
                    && json_as_object(&event.payload).is_some_and(|payload| {
                        payload.get("tool").and_then(json_as_string) == Some(tool)
                            && payload.get("kind").and_then(json_as_string) == Some("shell")
                    })
            })
        };
        assert!(shell_request_for("task_shell_start"));
        assert!(shell_request_for("exec_shell_cancel"));
    }

    #[test]
    fn mcp_tools_call_executes_revert_turn_after_runtime_approval() {
        let mut state = mcp_state_with_durable_approvals("mcp-revert-turn-approval");
        let repo = temp_git_repo("mcp-revert-turn-approval-repo");
        state.workspace = repo.clone();
        state.rollback = RollbackStore::new(temp_dir("mcp-revert-turn-approval-rollback"));
        fs::write(repo.join("src.txt"), "snapshot\n").unwrap();
        let snapshot = state
            .rollback
            .create_snapshot(&repo, "before mcp revert_turn".to_string())
            .unwrap();
        fs::write(repo.join("src.txt"), "later\n").unwrap();

        let responder = spawn_mcp_permission_responder(
            state.store.clone(),
            state.approval_thread_id.clone().unwrap(),
            "approved",
        );
        let request = format!(
            r#"{{"jsonrpc":"2.0","id":12,"method":"tools/call","params":{{"name":"revert_turn","arguments":{{"snapshot_id":"{}"}}}}}}"#,
            snapshot.id
        );
        let response = mcp_response_for_message(&request, &state).unwrap();
        responder.join().unwrap();
        let rendered = json_value_to_string(&response);

        assert!(rendered.contains("meta.applied=true"));
        assert_eq!(
            fs::read_to_string(repo.join("src.txt")).unwrap(),
            "snapshot\n"
        );
    }

    #[test]
    fn mcp_tools_call_executes_github_comment_after_runtime_approval() {
        let state = mcp_state_with_durable_approvals("mcp-github-comment-approval");
        let responder = spawn_mcp_permission_responder(
            state.store.clone(),
            state.approval_thread_id.clone().unwrap(),
            "approved",
        );
        let request = r#"{"jsonrpc":"2.0","id":13,"method":"tools/call","params":{"name":"github_comment","arguments":{"target":"pr","number":"7","body":"verified","evidence":{"tests_run":["cargo test"]},"dry_run":true}}}"#;
        let response = mcp_response_for_message(request, &state).unwrap();
        responder.join().unwrap();
        let rendered = json_value_to_string(&response);

        assert!(rendered.contains("Dry run: would comment on pr #7"));
        assert!(rendered.contains(r#""isError":false"#));
    }

    #[test]
    fn mcp_tools_call_executes_github_close_issue_after_runtime_approval() {
        let state = mcp_state_with_durable_approvals("mcp-github-close-approval");
        let responder = spawn_mcp_permission_responder(
            state.store.clone(),
            state.approval_thread_id.clone().unwrap(),
            "approved",
        );
        let request = r#"{"jsonrpc":"2.0","id":14,"method":"tools/call","params":{"name":"github_close_issue","arguments":{"number":"9","acceptance_criteria":["implementation complete"],"evidence":{"files_changed":["src/lib.rs"],"tests_run":["cargo test"],"final_status":"completed"},"allow_dirty":true,"dry_run":true}}}"#;
        let response = mcp_response_for_message(request, &state).unwrap();
        responder.join().unwrap();
        let rendered = json_value_to_string(&response);

        assert!(rendered.contains("Dry run: would close issue #9"));
        assert!(rendered.contains(r#""isError":false"#));
    }

    #[test]
    fn mcp_tools_call_creates_runtime_task_after_runtime_approval() {
        let state = mcp_state_with_durable_approvals("mcp-runtime-create-task");
        let responder = spawn_mcp_permission_responder(
            state.store.clone(),
            state.approval_thread_id.clone().unwrap(),
            "approved",
        );
        let request = r#"{"jsonrpc":"2.0","id":15,"method":"tools/call","params":{"name":"runtime_create_task","arguments":{"summary":"run queued parity check","kind":"agent"}}}"#;
        let response = mcp_response_for_message(request, &state).unwrap();
        responder.join().unwrap();
        let rendered = json_value_to_string(&response);

        assert!(rendered.contains("run queued parity check"));
        assert!(rendered.contains(r#""isError":false"#));
        let tasks = state.store.list_tasks(None, None, 10).unwrap();
        assert!(tasks
            .iter()
            .any(|task| { task.summary == "run queued parity check" && task.status == "pending" }));
    }

    #[test]
    fn mcp_tools_call_cancels_runtime_task_after_runtime_approval() {
        let state = mcp_state_with_durable_approvals("mcp-runtime-cancel-task");
        let thread_id = state.approval_thread_id.clone().unwrap();
        let task = state
            .store
            .create_task(
                None,
                Some(&thread_id),
                None,
                "agent".to_string(),
                "pending".to_string(),
                "queued task".to_string(),
            )
            .unwrap();
        let responder = spawn_mcp_permission_responder(state.store.clone(), thread_id, "approved");
        let request = format!(
            r#"{{"jsonrpc":"2.0","id":16,"method":"tools/call","params":{{"name":"runtime_cancel_task","arguments":{{"task_id":"{}","reason":"not needed"}}}}}}"#,
            task.id
        );
        let response = mcp_response_for_message(&request, &state).unwrap();
        responder.join().unwrap();
        let rendered = json_value_to_string(&response);

        assert!(rendered.contains("cancel_requested"));
        assert!(rendered.contains(r#""isError":false"#));
        let cancelled = state.store.load_task(&task.id).unwrap();
        assert_eq!(cancelled.status, "cancelled");
    }

    #[test]
    fn mcp_tools_call_creates_and_triggers_runtime_automation_after_runtime_approval() {
        let state = mcp_state_with_durable_approvals("mcp-runtime-automation-create");
        let responder = spawn_mcp_permission_responder(
            state.store.clone(),
            state.approval_thread_id.clone().unwrap(),
            "approved",
        );
        let request = r#"{"jsonrpc":"2.0","id":17,"method":"tools/call","params":{"name":"runtime_create_automation","arguments":{"name":"Nightly check","prompt":"run diagnostics","schedule":"daily"}}}"#;
        let response = mcp_response_for_message(request, &state).unwrap();
        responder.join().unwrap();
        let rendered = json_value_to_string(&response);

        assert!(rendered.contains("Nightly check"));
        assert!(rendered.contains(r#""isError":false"#));
        let automation = state
            .store
            .list_automations(None, None, 10)
            .unwrap()
            .into_iter()
            .find(|automation| automation.name == "Nightly check")
            .expect("created automation");

        let responder = spawn_mcp_permission_responder(
            state.store.clone(),
            state.approval_thread_id.clone().unwrap(),
            "approved",
        );
        let request = format!(
            r#"{{"jsonrpc":"2.0","id":18,"method":"tools/call","params":{{"name":"runtime_trigger_automation","arguments":{{"automation_id":"{}","prompt":"manual automation run"}}}}}}"#,
            automation.id
        );
        let response = mcp_response_for_message(&request, &state).unwrap();
        responder.join().unwrap();
        let rendered = json_value_to_string(&response);

        assert!(rendered.contains("manual automation run"));
        assert!(rendered.contains(r#""isError":false"#));
        let tasks = state.store.list_tasks(None, None, 10).unwrap();
        assert!(tasks
            .iter()
            .any(|task| { task.summary == "manual automation run" && task.kind == "automation" }));
    }

    #[test]
    fn mcp_tools_call_updates_pauses_resumes_and_deletes_runtime_automation_after_runtime_approval()
    {
        let state = mcp_state_with_durable_approvals("mcp-runtime-automation-update");
        let automation = state
            .store
            .create_automation(
                None,
                None,
                "Weekly check".to_string(),
                "active".to_string(),
                "weekly".to_string(),
                "run checks".to_string(),
                None,
                None,
            )
            .unwrap();

        let responder = spawn_mcp_permission_responder(
            state.store.clone(),
            state.approval_thread_id.clone().unwrap(),
            "approved",
        );
        let request = format!(
            r#"{{"jsonrpc":"2.0","id":19,"method":"tools/call","params":{{"name":"runtime_update_automation","arguments":{{"automation_id":"{}","name":"Updated check","schedule":"daily"}}}}}}"#,
            automation.id
        );
        let response = mcp_response_for_message(&request, &state).unwrap();
        responder.join().unwrap();
        assert!(json_value_to_string(&response).contains(r#""isError":false"#));
        let updated = state.store.load_automation(&automation.id).unwrap();
        assert_eq!(updated.name, "Updated check");
        assert_eq!(updated.schedule, "daily");

        let responder = spawn_mcp_permission_responder(
            state.store.clone(),
            state.approval_thread_id.clone().unwrap(),
            "approved",
        );
        let request = format!(
            r#"{{"jsonrpc":"2.0","id":20,"method":"tools/call","params":{{"name":"runtime_pause_automation","arguments":{{"automation_id":"{}"}}}}}}"#,
            automation.id
        );
        let response = mcp_response_for_message(&request, &state).unwrap();
        responder.join().unwrap();
        assert!(json_value_to_string(&response).contains(r#""isError":false"#));
        assert_eq!(
            state.store.load_automation(&automation.id).unwrap().status,
            "paused"
        );

        let responder = spawn_mcp_permission_responder(
            state.store.clone(),
            state.approval_thread_id.clone().unwrap(),
            "approved",
        );
        let request = format!(
            r#"{{"jsonrpc":"2.0","id":21,"method":"tools/call","params":{{"name":"runtime_resume_automation","arguments":{{"automation_id":"{}"}}}}}}"#,
            automation.id
        );
        let response = mcp_response_for_message(&request, &state).unwrap();
        responder.join().unwrap();
        assert!(json_value_to_string(&response).contains(r#""isError":false"#));
        assert_eq!(
            state.store.load_automation(&automation.id).unwrap().status,
            "active"
        );

        let responder = spawn_mcp_permission_responder(
            state.store.clone(),
            state.approval_thread_id.clone().unwrap(),
            "approved",
        );
        let request = format!(
            r#"{{"jsonrpc":"2.0","id":22,"method":"tools/call","params":{{"name":"runtime_delete_automation","arguments":{{"automation_id":"{}"}}}}}}"#,
            automation.id
        );
        let response = mcp_response_for_message(&request, &state).unwrap();
        responder.join().unwrap();
        assert!(json_value_to_string(&response).contains(r#""isError":false"#));
        assert!(state.store.load_automation(&automation.id).is_err());
    }

    #[test]
    fn mcp_tools_call_spawns_lists_reads_and_sends_runtime_agent_input_after_runtime_approval() {
        let state = mcp_state_with_durable_approvals("mcp-runtime-agent-spawn");
        let responder = spawn_mcp_permission_responder(
            state.store.clone(),
            state.approval_thread_id.clone().unwrap(),
            "approved",
        );
        let request = r#"{"jsonrpc":"2.0","id":24,"method":"tools/call","params":{"name":"runtime_spawn_agent","arguments":{"prompt":"investigate parity gap","title":"Parity helper"}}}"#;
        let response = mcp_response_for_message(request, &state).unwrap();
        responder.join().unwrap();
        let rendered = json_value_to_string(&response);

        assert!(rendered.contains("investigate parity gap"));
        assert!(rendered.contains(r#""isError":false"#));
        let agent = state
            .store
            .list_tasks(None, None, 10)
            .unwrap()
            .into_iter()
            .find(|task| task.kind == "subagent" && task.summary == "investigate parity gap")
            .expect("spawned subagent task");

        let request = format!(
            r#"{{"jsonrpc":"2.0","id":25,"method":"tools/call","params":{{"name":"runtime_agent_result","arguments":{{"agent_id":"{}"}}}}}}"#,
            agent.id
        );
        let response = mcp_response_for_message(&request, &state).unwrap();
        assert!(json_value_to_string(&response).contains(&agent.id));

        let response = mcp_response_for_message(
            r#"{"jsonrpc":"2.0","id":26,"method":"tools/call","params":{"name":"runtime_list_agents","arguments":{"limit":10}}}"#,
            &state,
        )
        .unwrap();
        assert!(json_value_to_string(&response).contains(&agent.id));

        let responder = spawn_mcp_permission_responder(
            state.store.clone(),
            state.approval_thread_id.clone().unwrap(),
            "approved",
        );
        let request = format!(
            r#"{{"jsonrpc":"2.0","id":27,"method":"tools/call","params":{{"name":"runtime_send_agent_input","arguments":{{"agent_id":"{}","message":"extra context"}}}}}}"#,
            agent.id
        );
        let response = mcp_response_for_message(&request, &state).unwrap();
        responder.join().unwrap();
        let rendered = json_value_to_string(&response);

        assert!(rendered.contains("extra context"));
        assert!(rendered.contains(r#""isError":false"#));
        let followups = state.store.list_tasks(None, None, 20).unwrap();
        assert!(followups.iter().any(|task| {
            task.kind == "subagent_input"
                && task.parent_task_id.as_deref() == Some(agent.id.as_str())
                && task.summary == "extra context"
        }));
    }

    #[test]
    fn mcp_tools_call_cancels_closes_and_resumes_runtime_agents_after_runtime_approval() {
        let state = mcp_state_with_durable_approvals("mcp-runtime-agent-control");
        let thread_id = state.approval_thread_id.clone().unwrap();
        let cancel_agent = state
            .store
            .create_task(
                None,
                Some(&thread_id),
                None,
                "subagent".to_string(),
                "pending".to_string(),
                "cancel me".to_string(),
            )
            .unwrap();
        let responder =
            spawn_mcp_permission_responder(state.store.clone(), thread_id.clone(), "approved");
        let request = format!(
            r#"{{"jsonrpc":"2.0","id":28,"method":"tools/call","params":{{"name":"runtime_cancel_agent","arguments":{{"agent_id":"{}"}}}}}}"#,
            cancel_agent.id
        );
        let response = mcp_response_for_message(&request, &state).unwrap();
        responder.join().unwrap();
        assert!(json_value_to_string(&response).contains(r#""isError":false"#));
        assert_eq!(
            state.store.load_task(&cancel_agent.id).unwrap().status,
            "cancelled"
        );

        let close_agent = state
            .store
            .create_task(
                None,
                Some(&thread_id),
                None,
                "subagent".to_string(),
                "pending".to_string(),
                "close me".to_string(),
            )
            .unwrap();
        let responder =
            spawn_mcp_permission_responder(state.store.clone(), thread_id.clone(), "approved");
        let request = format!(
            r#"{{"jsonrpc":"2.0","id":29,"method":"tools/call","params":{{"name":"runtime_close_agent","arguments":{{"agent_id":"{}"}}}}}}"#,
            close_agent.id
        );
        let response = mcp_response_for_message(&request, &state).unwrap();
        responder.join().unwrap();
        assert!(json_value_to_string(&response).contains(r#""isError":false"#));
        assert_eq!(
            state.store.load_task(&close_agent.id).unwrap().status,
            "cancelled"
        );

        let resume_agent = state
            .store
            .create_task(
                None,
                Some(&thread_id),
                None,
                "subagent".to_string(),
                "paused".to_string(),
                "resume me".to_string(),
            )
            .unwrap();
        let responder = spawn_mcp_permission_responder(state.store.clone(), thread_id, "approved");
        let request = format!(
            r#"{{"jsonrpc":"2.0","id":30,"method":"tools/call","params":{{"name":"runtime_resume_agent","arguments":{{"agent_id":"{}","prompt":"resume with context"}}}}}}"#,
            resume_agent.id
        );
        let response = mcp_response_for_message(&request, &state).unwrap();
        responder.join().unwrap();
        assert!(json_value_to_string(&response).contains(r#""isError":false"#));
        let resumed = state.store.load_task(&resume_agent.id).unwrap();
        assert_eq!(resumed.status, "pending");
        assert_eq!(resumed.summary, "resume with context");
    }

    #[test]
    fn mcp_tools_call_executes_run_shell_after_runtime_approval() {
        let state = mcp_state_with_durable_approvals("mcp-run-shell-approval-allow");
        let store = state.store.clone();
        let thread_id = state.approval_thread_id.clone().unwrap();
        let responder_thread_id = thread_id.clone();
        let responder = thread::spawn(move || {
            for _ in 0..100 {
                let events = store.read_events(&responder_thread_id, 0).unwrap();
                if let Some(request) = events
                    .iter()
                    .find(|event| event.kind == "permission_request")
                {
                    store
                        .append_permission_response(
                            &responder_thread_id,
                            None,
                            request.id.clone(),
                            "approved".to_string(),
                        )
                        .unwrap();
                    return;
                }
                thread::sleep(Duration::from_millis(1));
            }
            panic!("permission request was not recorded");
        });
        let response = mcp_response_for_message(
            r#"{"jsonrpc":"2.0","id":10,"method":"tools/call","params":{"name":"run_shell","arguments":{"command":"pwd"}}}"#,
            &state,
        )
        .unwrap();
        responder.join().unwrap();
        let rendered = json_value_to_string(&response);
        let events = state.store.read_events(&thread_id, 0).unwrap();

        assert!(rendered.contains("meta.command_kind=other"));
        assert!(rendered.contains(r#""isError":false"#));
        assert!(events
            .iter()
            .any(|event| event.kind == "permission_request"));
        assert!(events
            .iter()
            .any(|event| event.kind == "permission_response"));
    }

    #[test]
    fn mcp_tools_call_rejects_run_shell_after_runtime_denial() {
        let state = mcp_state_with_durable_approvals("mcp-run-shell-approval-deny");
        let store = state.store.clone();
        let thread_id = state.approval_thread_id.clone().unwrap();
        let responder_thread_id = thread_id.clone();
        let responder = thread::spawn(move || {
            for _ in 0..100 {
                let events = store.read_events(&responder_thread_id, 0).unwrap();
                if let Some(request) = events
                    .iter()
                    .find(|event| event.kind == "permission_request")
                {
                    store
                        .append_permission_response(
                            &responder_thread_id,
                            None,
                            request.id.clone(),
                            "denied".to_string(),
                        )
                        .unwrap();
                    return;
                }
                thread::sleep(Duration::from_millis(1));
            }
            panic!("permission request was not recorded");
        });
        let response = mcp_response_for_message(
            r#"{"jsonrpc":"2.0","id":11,"method":"tools/call","params":{"name":"run_shell","arguments":{"command":"pwd"}}}"#,
            &state,
        )
        .unwrap();
        responder.join().unwrap();
        let rendered = json_value_to_string(&response);

        assert!(rendered.contains("MCP run_shell denied by runtime approval"));
        assert!(rendered.contains(r#""isError":true"#));
    }

    #[test]
    fn mcp_tools_call_executes_apply_patch_after_runtime_approval() {
        let state = mcp_state_with_durable_approvals("mcp-apply-patch-approval-allow");
        let file = state.workspace.join("note.txt");
        fs::write(&file, "alpha\n").unwrap();
        let store = state.store.clone();
        let thread_id = state.approval_thread_id.clone().unwrap();
        let responder = spawn_mcp_permission_responder(store, thread_id.clone(), "approved");
        let patch = "--- note.txt\n+++ note.txt\n@@ -1 +1 @@\n-alpha\n+beta\n";
        let request = format!(
            r#"{{"jsonrpc":"2.0","id":12,"method":"tools/call","params":{{"name":"apply_patch","arguments":{{"patch":"{}"}}}}}}"#,
            crate::util::json::json_escape(patch)
        );

        let response = mcp_response_for_message(&request, &state).unwrap();
        responder.join().unwrap();
        let rendered = json_value_to_string(&response);
        let events = state.store.read_events(&thread_id, 0).unwrap();

        assert!(rendered.contains("Applied unified patch"));
        assert!(rendered.contains(r#""isError":false"#));
        assert_eq!(fs::read_to_string(&file).unwrap(), "beta\n");
        assert!(events.iter().any(|event| {
            event.kind == "permission_request"
                && json_value_to_string(&event.payload).contains(r#""tool":"apply_patch""#)
        }));
        assert!(events
            .iter()
            .any(|event| event.kind == "permission_response"));
    }

    #[test]
    fn mcp_tools_call_rejects_apply_patch_after_runtime_denial() {
        let state = mcp_state_with_durable_approvals("mcp-apply-patch-approval-deny");
        let file = state.workspace.join("note.txt");
        fs::write(&file, "alpha\n").unwrap();
        let store = state.store.clone();
        let thread_id = state.approval_thread_id.clone().unwrap();
        let responder = spawn_mcp_permission_responder(store, thread_id.clone(), "denied");
        let patch = "--- note.txt\n+++ note.txt\n@@ -1 +1 @@\n-alpha\n+beta\n";
        let request = format!(
            r#"{{"jsonrpc":"2.0","id":13,"method":"tools/call","params":{{"name":"apply_patch","arguments":{{"patch":"{}"}}}}}}"#,
            crate::util::json::json_escape(patch)
        );

        let response = mcp_response_for_message(&request, &state).unwrap();
        responder.join().unwrap();
        let rendered = json_value_to_string(&response);

        assert!(rendered.contains("MCP apply_patch denied by runtime approval"));
        assert!(rendered.contains(r#""isError":true"#));
        assert_eq!(fs::read_to_string(&file).unwrap(), "alpha\n");
    }

    #[test]
    fn mcp_tools_call_executes_write_file_after_runtime_approval() {
        let state = mcp_state_with_durable_approvals("mcp-write-file-approval-allow");
        let file = state.workspace.join("nested").join("note.txt");
        let store = state.store.clone();
        let thread_id = state.approval_thread_id.clone().unwrap();
        let responder = spawn_mcp_permission_responder(store, thread_id.clone(), "approved");
        let request = r#"{"jsonrpc":"2.0","id":14,"method":"tools/call","params":{"name":"write_file","arguments":{"path":"nested/note.txt","content":"hello from MCP\n"}}}"#;

        let response = mcp_response_for_message(request, &state).unwrap();
        responder.join().unwrap();
        let rendered = json_value_to_string(&response);
        let events = state.store.read_events(&thread_id, 0).unwrap();

        assert!(rendered.contains("Wrote 15 bytes to nested/note.txt"));
        assert!(rendered.contains(r#""isError":false"#));
        assert_eq!(fs::read_to_string(&file).unwrap(), "hello from MCP\n");
        assert!(events.iter().any(|event| {
            event.kind == "permission_request"
                && json_value_to_string(&event.payload).contains(r#""tool":"write_file""#)
        }));
        assert!(events
            .iter()
            .any(|event| event.kind == "permission_response"));
    }

    #[test]
    fn mcp_tools_call_rejects_write_file_after_runtime_denial() {
        let state = mcp_state_with_durable_approvals("mcp-write-file-approval-deny");
        let file = state.workspace.join("note.txt");
        let store = state.store.clone();
        let thread_id = state.approval_thread_id.clone().unwrap();
        let responder = spawn_mcp_permission_responder(store, thread_id.clone(), "denied");
        let request = r#"{"jsonrpc":"2.0","id":15,"method":"tools/call","params":{"name":"write_file","arguments":{"path":"note.txt","content":"blocked\n"}}}"#;

        let response = mcp_response_for_message(request, &state).unwrap();
        responder.join().unwrap();
        let rendered = json_value_to_string(&response);

        assert!(rendered.contains("MCP write_file denied by runtime approval"));
        assert!(rendered.contains(r#""isError":true"#));
        assert!(!file.exists());
    }

    #[test]
    fn mcp_tools_call_rejects_write_file_unsafe_path() {
        let state = mcp_state_with_durable_approvals("mcp-write-file-unsafe");
        let response = mcp_response_for_message(
            r#"{"jsonrpc":"2.0","id":16,"method":"tools/call","params":{"name":"write_file","arguments":{"path":"../escape.txt","content":"nope\n"}}}"#,
            &state,
        )
        .unwrap();
        let rendered = json_value_to_string(&response);

        assert!(rendered.contains("unsafe write_file path"));
        assert!(rendered.contains(r#""isError":true"#));
    }

    #[test]
    fn mcp_tools_call_executes_edit_file_after_runtime_approval() {
        let state = mcp_state_with_durable_approvals("mcp-edit-file-approval-allow");
        let file = state.workspace.join("note.txt");
        fs::write(&file, "hello world hello").unwrap();
        let store = state.store.clone();
        let thread_id = state.approval_thread_id.clone().unwrap();
        let responder = spawn_mcp_permission_responder(store, thread_id, "approved");
        let request = r#"{"jsonrpc":"2.0","id":17,"method":"tools/call","params":{"name":"edit_file","arguments":{"path":"note.txt","search":"hello","replace":"hi"}}}"#;

        let response = mcp_response_for_message(request, &state).unwrap();
        responder.join().unwrap();
        let rendered = json_value_to_string(&response);

        assert!(rendered.contains("Replaced 2 occurrence"));
        assert!(rendered.contains(r#""isError":false"#));
        assert_eq!(fs::read_to_string(&file).unwrap(), "hi world hi");
    }

    #[test]
    fn mcp_tools_call_executes_delete_file_after_runtime_approval() {
        let state = mcp_state_with_durable_approvals("mcp-delete-file-approval-allow");
        let file = state.workspace.join("note.txt");
        fs::write(&file, "delete me\n").unwrap();
        let store = state.store.clone();
        let thread_id = state.approval_thread_id.clone().unwrap();
        let responder = spawn_mcp_permission_responder(store, thread_id.clone(), "approved");
        let request = r#"{"jsonrpc":"2.0","id":17,"method":"tools/call","params":{"name":"delete_file","arguments":{"path":"note.txt"}}}"#;

        let response = mcp_response_for_message(request, &state).unwrap();
        responder.join().unwrap();
        let rendered = json_value_to_string(&response);
        let events = state.store.read_events(&thread_id, 0).unwrap();

        assert!(rendered.contains("Deleted file note.txt"));
        assert!(rendered.contains(r#""isError":false"#));
        assert!(!file.exists());
        assert!(events.iter().any(|event| {
            event.kind == "permission_request"
                && json_value_to_string(&event.payload).contains(r#""tool":"delete_file""#)
        }));
    }

    #[test]
    fn mcp_tools_call_rejects_delete_file_after_runtime_denial() {
        let state = mcp_state_with_durable_approvals("mcp-delete-file-approval-deny");
        let file = state.workspace.join("note.txt");
        fs::write(&file, "keep me\n").unwrap();
        let store = state.store.clone();
        let thread_id = state.approval_thread_id.clone().unwrap();
        let responder = spawn_mcp_permission_responder(store, thread_id.clone(), "denied");
        let request = r#"{"jsonrpc":"2.0","id":18,"method":"tools/call","params":{"name":"delete_file","arguments":{"path":"note.txt"}}}"#;

        let response = mcp_response_for_message(request, &state).unwrap();
        responder.join().unwrap();
        let rendered = json_value_to_string(&response);

        assert!(rendered.contains("MCP delete_file denied by runtime approval"));
        assert!(rendered.contains(r#""isError":true"#));
        assert_eq!(fs::read_to_string(&file).unwrap(), "keep me\n");
    }

    #[test]
    fn mcp_tools_call_rejects_delete_file_unsafe_path() {
        let state = mcp_state_with_durable_approvals("mcp-delete-file-unsafe");
        let response = mcp_response_for_message(
            r#"{"jsonrpc":"2.0","id":19,"method":"tools/call","params":{"name":"delete_file","arguments":{"path":"../escape.txt"}}}"#,
            &state,
        )
        .unwrap();
        let rendered = json_value_to_string(&response);

        assert!(rendered.contains("unsafe delete_file path"));
        assert!(rendered.contains(r#""isError":true"#));
    }

    #[test]
    fn mcp_tools_call_executes_copy_file_after_runtime_approval() {
        let state = mcp_state_with_durable_approvals("mcp-copy-file-approval-allow");
        let source = state.workspace.join("note.txt");
        let destination = state.workspace.join("nested").join("copy.txt");
        fs::write(&source, "copy me\n").unwrap();
        let store = state.store.clone();
        let thread_id = state.approval_thread_id.clone().unwrap();
        let responder = spawn_mcp_permission_responder(store, thread_id.clone(), "approved");
        let request = r#"{"jsonrpc":"2.0","id":20,"method":"tools/call","params":{"name":"copy_file","arguments":{"source_path":"note.txt","destination_path":"nested/copy.txt"}}}"#;

        let response = mcp_response_for_message(request, &state).unwrap();
        responder.join().unwrap();
        let rendered = json_value_to_string(&response);
        let events = state.store.read_events(&thread_id, 0).unwrap();

        assert!(rendered.contains("Copied file note.txt to nested/copy.txt"));
        assert!(rendered.contains(r#""isError":false"#));
        assert_eq!(fs::read_to_string(&source).unwrap(), "copy me\n");
        assert_eq!(fs::read_to_string(&destination).unwrap(), "copy me\n");
        assert!(events.iter().any(|event| {
            event.kind == "permission_request"
                && json_value_to_string(&event.payload).contains(r#""tool":"copy_file""#)
        }));
    }

    #[test]
    fn mcp_tools_call_rejects_copy_file_after_runtime_denial() {
        let state = mcp_state_with_durable_approvals("mcp-copy-file-approval-deny");
        let source = state.workspace.join("note.txt");
        let destination = state.workspace.join("copy.txt");
        fs::write(&source, "keep source\n").unwrap();
        let store = state.store.clone();
        let thread_id = state.approval_thread_id.clone().unwrap();
        let responder = spawn_mcp_permission_responder(store, thread_id.clone(), "denied");
        let request = r#"{"jsonrpc":"2.0","id":21,"method":"tools/call","params":{"name":"copy_file","arguments":{"source_path":"note.txt","destination_path":"copy.txt"}}}"#;

        let response = mcp_response_for_message(request, &state).unwrap();
        responder.join().unwrap();
        let rendered = json_value_to_string(&response);

        assert!(rendered.contains("MCP copy_file denied by runtime approval"));
        assert!(rendered.contains(r#""isError":true"#));
        assert_eq!(fs::read_to_string(&source).unwrap(), "keep source\n");
        assert!(!destination.exists());
    }

    #[test]
    fn mcp_tools_call_rejects_copy_file_unsafe_path() {
        let state = mcp_state_with_durable_approvals("mcp-copy-file-unsafe");
        let response = mcp_response_for_message(
            r#"{"jsonrpc":"2.0","id":22,"method":"tools/call","params":{"name":"copy_file","arguments":{"source_path":"note.txt","destination_path":"../escape.txt"}}}"#,
            &state,
        )
        .unwrap();
        let rendered = json_value_to_string(&response);

        assert!(rendered.contains("unsafe copy_file destination path"));
        assert!(rendered.contains(r#""isError":true"#));
    }

    #[test]
    fn mcp_tools_call_rejects_copy_file_existing_destination() {
        let state = mcp_state_with_durable_approvals("mcp-copy-file-existing-destination");
        let source = state.workspace.join("note.txt");
        let destination = state.workspace.join("copy.txt");
        fs::write(&source, "keep source\n").unwrap();
        fs::write(&destination, "keep destination\n").unwrap();
        let store = state.store.clone();
        let thread_id = state.approval_thread_id.clone().unwrap();
        let responder = spawn_mcp_permission_responder(store, thread_id, "approved");
        let request = r#"{"jsonrpc":"2.0","id":23,"method":"tools/call","params":{"name":"copy_file","arguments":{"source_path":"note.txt","destination_path":"copy.txt"}}}"#;

        let response = mcp_response_for_message(request, &state).unwrap();
        responder.join().unwrap();
        let rendered = json_value_to_string(&response);

        assert!(rendered.contains("copy_file destination already exists"));
        assert!(rendered.contains(r#""isError":true"#));
        assert_eq!(fs::read_to_string(&source).unwrap(), "keep source\n");
        assert_eq!(
            fs::read_to_string(&destination).unwrap(),
            "keep destination\n"
        );
    }

    #[test]
    fn mcp_tools_call_executes_move_file_after_runtime_approval() {
        let state = mcp_state_with_durable_approvals("mcp-move-file-approval-allow");
        let source = state.workspace.join("note.txt");
        let destination = state.workspace.join("nested").join("moved.txt");
        fs::write(&source, "move me\n").unwrap();
        let store = state.store.clone();
        let thread_id = state.approval_thread_id.clone().unwrap();
        let responder = spawn_mcp_permission_responder(store, thread_id.clone(), "approved");
        let request = r#"{"jsonrpc":"2.0","id":20,"method":"tools/call","params":{"name":"move_file","arguments":{"source_path":"note.txt","destination_path":"nested/moved.txt"}}}"#;

        let response = mcp_response_for_message(request, &state).unwrap();
        responder.join().unwrap();
        let rendered = json_value_to_string(&response);
        let events = state.store.read_events(&thread_id, 0).unwrap();

        assert!(rendered.contains("Moved file note.txt to nested/moved.txt"));
        assert!(rendered.contains(r#""isError":false"#));
        assert!(!source.exists());
        assert_eq!(fs::read_to_string(&destination).unwrap(), "move me\n");
        assert!(events.iter().any(|event| {
            event.kind == "permission_request"
                && json_value_to_string(&event.payload).contains(r#""tool":"move_file""#)
        }));
    }

    #[test]
    fn mcp_tools_call_rejects_move_file_after_runtime_denial() {
        let state = mcp_state_with_durable_approvals("mcp-move-file-approval-deny");
        let source = state.workspace.join("note.txt");
        let destination = state.workspace.join("moved.txt");
        fs::write(&source, "keep me\n").unwrap();
        let store = state.store.clone();
        let thread_id = state.approval_thread_id.clone().unwrap();
        let responder = spawn_mcp_permission_responder(store, thread_id.clone(), "denied");
        let request = r#"{"jsonrpc":"2.0","id":21,"method":"tools/call","params":{"name":"move_file","arguments":{"source_path":"note.txt","destination_path":"moved.txt"}}}"#;

        let response = mcp_response_for_message(request, &state).unwrap();
        responder.join().unwrap();
        let rendered = json_value_to_string(&response);

        assert!(rendered.contains("MCP move_file denied by runtime approval"));
        assert!(rendered.contains(r#""isError":true"#));
        assert_eq!(fs::read_to_string(&source).unwrap(), "keep me\n");
        assert!(!destination.exists());
    }

    #[test]
    fn mcp_tools_call_rejects_move_file_unsafe_path() {
        let state = mcp_state_with_durable_approvals("mcp-move-file-unsafe");
        let response = mcp_response_for_message(
            r#"{"jsonrpc":"2.0","id":22,"method":"tools/call","params":{"name":"move_file","arguments":{"source_path":"note.txt","destination_path":"../escape.txt"}}}"#,
            &state,
        )
        .unwrap();
        let rendered = json_value_to_string(&response);

        assert!(rendered.contains("unsafe move_file destination path"));
        assert!(rendered.contains(r#""isError":true"#));
    }

    #[test]
    fn mcp_tools_call_rejects_move_file_existing_destination() {
        let state = mcp_state_with_durable_approvals("mcp-move-file-existing-destination");
        let source = state.workspace.join("note.txt");
        let destination = state.workspace.join("moved.txt");
        fs::write(&source, "keep source\n").unwrap();
        fs::write(&destination, "keep destination\n").unwrap();
        let store = state.store.clone();
        let thread_id = state.approval_thread_id.clone().unwrap();
        let responder = spawn_mcp_permission_responder(store, thread_id, "approved");
        let request = r#"{"jsonrpc":"2.0","id":23,"method":"tools/call","params":{"name":"move_file","arguments":{"source_path":"note.txt","destination_path":"moved.txt"}}}"#;

        let response = mcp_response_for_message(request, &state).unwrap();
        responder.join().unwrap();
        let rendered = json_value_to_string(&response);

        assert!(rendered.contains("move_file destination already exists"));
        assert!(rendered.contains(r#""isError":true"#));
        assert_eq!(fs::read_to_string(&source).unwrap(), "keep source\n");
        assert_eq!(
            fs::read_to_string(&destination).unwrap(),
            "keep destination\n"
        );
    }

    #[test]
    fn mcp_runtime_tool_lists_sessions() {
        let state = mcp_state("mcp-runtime-sessions");
        state
            .store
            .create_session("MCP visible session".to_string(), ".".to_string())
            .unwrap();

        let response = mcp_response_for_message(
            r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"runtime_list_sessions","arguments":{"limit":5}}}"#,
            &state,
        )
        .unwrap();
        let rendered = json_value_to_string(&response);

        assert!(rendered.contains("MCP visible session"));
        assert!(rendered.contains(r#""isError":false"#));
    }

    #[test]
    fn mcp_resources_list_includes_workspace_and_runtime_records() {
        let state = mcp_state("mcp-resources-list");
        let session = state
            .store
            .create_session(
                "MCP resource session".to_string(),
                state.workspace.display().to_string(),
            )
            .unwrap();
        let thread = state
            .store
            .create_thread_for_session(
                &session.id,
                "MCP resource thread".to_string(),
                state.workspace.display().to_string(),
                "deepseek-coder".to_string(),
                "agent".to_string(),
            )
            .unwrap();
        state
            .store
            .create_task(
                Some(&session.id),
                Some(&thread.id),
                None,
                "agent".to_string(),
                "pending".to_string(),
                "MCP resource task".to_string(),
            )
            .unwrap();

        let response = mcp_response_for_message(
            r#"{"jsonrpc":"2.0","id":5,"method":"resources/list","params":{}}"#,
            &state,
        )
        .unwrap();
        let rendered = json_value_to_string(&response);

        assert!(rendered.contains("file://"));
        assert!(rendered.contains("MCP resource session"));
        assert!(rendered.contains("deepseekcode://runtime/threads/"));
        assert!(rendered.contains("MCP resource task"));
    }

    #[test]
    fn mcp_resource_templates_list_returns_runtime_templates() {
        let state = mcp_state("mcp-resource-templates-list");
        let response = mcp_response_for_message(
            r#"{"jsonrpc":"2.0","id":6,"method":"resources/templates/list","params":{}}"#,
            &state,
        )
        .unwrap();
        let rendered = json_value_to_string(&response);

        assert!(rendered.contains(r#""resourceTemplates":["#));
        assert!(rendered.contains("deepseekcode://runtime/sessions/{id}"));
        assert!(rendered.contains("deepseekcode://runtime/threads/{id}"));
        assert!(rendered.contains("deepseekcode://runtime/tasks/{id}"));
        assert!(rendered.contains(r#""nextCursor":null"#));
    }

    #[test]
    fn mcp_resources_read_returns_runtime_thread_json() {
        let state = mcp_state("mcp-resources-read");
        let session = state
            .store
            .create_session("MCP read session".to_string(), ".".to_string())
            .unwrap();
        let thread = state
            .store
            .create_thread_for_session(
                &session.id,
                "MCP read thread".to_string(),
                ".".to_string(),
                "deepseek-coder".to_string(),
                "agent".to_string(),
            )
            .unwrap();
        let uri = format!("deepseekcode://runtime/threads/{}", thread.id);
        let request = format!(
            r#"{{"jsonrpc":"2.0","id":6,"method":"resources/read","params":{{"uri":"{uri}"}}}}"#
        );

        let response = mcp_response_for_message(&request, &state).unwrap();
        let rendered = json_value_to_string(&response);

        assert!(rendered.contains("MCP read thread"));
        assert!(rendered.contains(r#""contents""#));
        assert!(rendered.contains(r#""mimeType":"application/json""#));
    }

    #[test]
    fn acp_initialize_advertises_baseline_agent() {
        let mut state = acp_state("acp-init");
        let dispatch = acp_dispatch_for_message(
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":1}}"#,
            &mut state,
        );
        let AcpDispatch::Responses(responses) = dispatch else {
            panic!("expected ACP responses");
        };

        assert_eq!(responses.len(), 1);
        let rendered = json_value_to_string(&responses[0]);
        assert!(rendered.contains(r#""id":1"#));
        assert!(rendered.contains(r#""protocolVersion":1"#));
        assert!(rendered.contains(r#""name":"deepseek-code""#));
        assert!(rendered.contains(r#""embeddedContext":true"#));
        assert!(rendered.contains(r#""loadSession":true"#));
        assert!(
            rendered.contains(r#""checkpoints":{"apply":true,"readOnly":false,"restore":true}"#)
        );
        assert!(rendered.contains(r#""tools":{"permissioned":true,"readOnly":true}"#));
    }

    #[test]
    fn acp_session_list_returns_runtime_sessions() {
        let mut state = acp_state("acp-list");
        state
            .store
            .create_session("ACP existing session".to_string(), ".".to_string())
            .unwrap();

        let dispatch = acp_dispatch_for_message(
            r#"{"jsonrpc":"2.0","id":2,"method":"session/list","params":{"limit":10}}"#,
            &mut state,
        );
        let AcpDispatch::Responses(responses) = dispatch else {
            panic!("expected ACP responses");
        };
        let rendered = json_value_to_string(&responses[0]);

        assert!(rendered.contains(r#""id":2"#));
        assert!(rendered.contains("ACP existing session"));
        assert!(rendered.contains(r#""sessions""#));
    }

    #[test]
    fn acp_session_load_maps_active_runtime_thread() {
        let mut state = acp_state("acp-load");
        let workspace = temp_dir("acp-load-workspace");
        let session = state
            .store
            .create_session(
                "ACP durable session".to_string(),
                workspace.display().to_string(),
            )
            .unwrap();
        let thread = state
            .store
            .create_thread_for_session(
                &session.id,
                "ACP durable thread".to_string(),
                workspace.display().to_string(),
                "deepseek-coder".to_string(),
                "agent".to_string(),
            )
            .unwrap();
        let request = format!(
            r#"{{"jsonrpc":"2.0","id":3,"method":"session/load","params":{{"sessionId":"{}"}}}}"#,
            session.id
        );

        let dispatch = acp_dispatch_for_message(&request, &mut state);
        let AcpDispatch::Responses(responses) = dispatch else {
            panic!("expected ACP responses");
        };
        let rendered = json_value_to_string(&responses[0]);
        let root = parse_root_object(&rendered).unwrap();
        let acp_session_id = root
            .get("result")
            .and_then(json_as_object)
            .and_then(|result| result.get("sessionId"))
            .and_then(json_as_string)
            .unwrap();
        let loaded = state.sessions.get(acp_session_id).unwrap();

        assert_eq!(loaded.cwd, workspace);
        assert_eq!(
            loaded.runtime_session_id.as_deref(),
            Some(session.id.as_str())
        );
        assert_eq!(
            loaded.runtime_thread_id.as_deref(),
            Some(thread.id.as_str())
        );
        assert!(rendered.contains("ACP durable thread"));
    }

    #[test]
    fn acp_session_load_rejects_thread_from_another_session() {
        let mut state = acp_state("acp-load-mismatch");
        let session = state
            .store
            .create_session("ACP one".to_string(), ".".to_string())
            .unwrap();
        let other = state
            .store
            .create_session("ACP other".to_string(), ".".to_string())
            .unwrap();
        let thread = state
            .store
            .create_thread_for_session(
                &other.id,
                "Other thread".to_string(),
                ".".to_string(),
                "deepseek-coder".to_string(),
                "agent".to_string(),
            )
            .unwrap();
        let request = format!(
            r#"{{"jsonrpc":"2.0","id":4,"method":"session/load","params":{{"sessionId":"{}","threadId":"{}"}}}}"#,
            session.id, thread.id
        );

        let dispatch = acp_dispatch_for_message(&request, &mut state);
        let AcpDispatch::Responses(responses) = dispatch else {
            panic!("expected ACP responses");
        };
        let rendered = json_value_to_string(&responses[0]);

        assert!(rendered.contains(r#""code":-32602"#));
        assert!(rendered.contains("does not belong"));
    }

    #[test]
    fn acp_session_tools_list_new_session_is_read_only() {
        let mut state = acp_state("acp-tools-list-read-only");
        let workspace = temp_dir("acp-tools-list-read-only-workspace");
        let new_request = format!(
            r#"{{"jsonrpc":"2.0","id":20,"method":"session/new","params":{{"cwd":"{}"}}}}"#,
            crate::util::json::json_escape(&workspace.display().to_string())
        );
        let AcpDispatch::Responses(new_responses) =
            acp_dispatch_for_message(&new_request, &mut state)
        else {
            panic!("expected ACP responses");
        };
        let rendered_new = json_value_to_string(&new_responses[0]);
        let root = parse_root_object(&rendered_new).unwrap();
        let acp_session_id = root
            .get("result")
            .and_then(json_as_object)
            .and_then(|result| result.get("sessionId"))
            .and_then(json_as_string)
            .unwrap();
        let request = format!(
            r#"{{"jsonrpc":"2.0","id":21,"method":"session/tools/list","params":{{"sessionId":"{acp_session_id}"}}}}"#
        );

        let AcpDispatch::Responses(responses) = acp_dispatch_for_message(&request, &mut state)
        else {
            panic!("expected ACP responses");
        };
        let rendered = json_value_to_string(&responses[0]);

        assert!(rendered.contains(r#""name":"read_file""#));
        assert!(rendered.contains(r#""name":"retrieve_tool_result""#));
        assert!(rendered.contains(r#""name":"list_dir""#));
        assert!(rendered.contains(r#""name":"grep_files""#));
        assert!(rendered.contains(r#""name":"file_search""#));
        assert!(rendered.contains(r#""name":"web_search""#));
        assert!(rendered.contains(r#""name":"web_run""#));
        assert!(rendered.contains(r#""name":"fetch_url""#));
        assert!(rendered.contains(r#""name":"pandoc_convert""#));
        assert!(rendered.contains(r#""name":"image_ocr""#));
        assert!(rendered.contains(r#""name":"load_skill""#));
        assert!(rendered.contains(r#""name":"request_user_input""#));
        assert!(rendered.contains(r#""name":"notify""#));
        assert!(rendered.contains(r#""name":"git_status""#));
        assert!(rendered.contains(r#""name":"project_map""#));
        assert!(rendered.contains(r#""name":"validate_data""#));
        assert!(rendered.contains(r#""name":"github_issue_context""#));
        assert!(rendered.contains(r#""name":"github_pr_context""#));
        assert!(rendered.contains(r#""name":"review""#));
        assert!(rendered.contains(r#""name":"pr_review_comment_plan""#));
        assert!(rendered.contains(r#""name":"recall_archive""#));
        assert!(rendered.contains(r#""name":"tool_search_tool_regex""#));
        assert!(rendered.contains(r#""name":"tool_search_tool_bm25""#));
        assert!(rendered.contains(r#""name":"rlm_chunk_plan""#));
        assert!(rendered.contains(r#""name":"rlm_map_reduce_plan""#));
        assert!(rendered.contains(r#""name":"rlm_recursive_plan""#));
        assert!(rendered.contains(r#""name":"rlm_python""#));
        assert!(rendered.contains(r#""name":"rlm_python_sessions""#));
        assert!(!rendered.contains(r#""name":"run_shell""#));
        assert!(!rendered.contains(r#""name":"run_tests""#));
        assert!(!rendered.contains(r#""name":"image_analyze""#));
        assert!(!rendered.contains(r#""name":"rlm_python_session""#));
        assert!(!rendered.contains(r#""name":"apply_patch""#));
        assert!(!rendered.contains(r#""name":"write_file""#));
        assert!(!rendered.contains(r#""name":"edit_file""#));
        assert!(!rendered.contains(r#""name":"github_comment""#));
        assert!(!rendered.contains(r#""name":"github_close_issue""#));
    }

    #[test]
    fn acp_session_tools_call_reads_file_from_session_workspace() {
        let mut state = acp_state("acp-tools-read-file");
        let workspace = temp_dir("acp-tools-read-file-workspace");
        fs::write(workspace.join("note.txt"), "hello acp tool\nsecond\n").unwrap();
        let new_request = format!(
            r#"{{"jsonrpc":"2.0","id":22,"method":"session/new","params":{{"cwd":"{}"}}}}"#,
            crate::util::json::json_escape(&workspace.display().to_string())
        );
        let AcpDispatch::Responses(new_responses) =
            acp_dispatch_for_message(&new_request, &mut state)
        else {
            panic!("expected ACP responses");
        };
        let rendered_new = json_value_to_string(&new_responses[0]);
        let root = parse_root_object(&rendered_new).unwrap();
        let acp_session_id = root
            .get("result")
            .and_then(json_as_object)
            .and_then(|result| result.get("sessionId"))
            .and_then(json_as_string)
            .unwrap();
        let request = format!(
            r#"{{"jsonrpc":"2.0","id":23,"method":"session/tools/call","params":{{"sessionId":"{acp_session_id}","name":"read_file","arguments":{{"path":"note.txt","max_lines":1}}}}}}"#
        );

        let AcpDispatch::Responses(responses) = acp_dispatch_for_message(&request, &mut state)
        else {
            panic!("expected ACP responses");
        };
        assert_eq!(responses.len(), 3);
        let call_update = json_value_to_string(&responses[0]);
        let result_update = json_value_to_string(&responses[1]);
        let rendered = json_value_to_string(&responses[2]);

        assert!(call_update.contains(r#""method":"session/update""#));
        assert!(call_update.contains(r#""sessionUpdate":"tool_call""#));
        assert!(call_update.contains(r#""toolCallId":"tool_call_"#));
        assert!(call_update.contains(r#""title":"Reading file""#));
        assert!(call_update.contains(r#""kind":"read""#));
        assert!(call_update.contains(r#""status":"in_progress""#));
        assert!(call_update.contains(r#""rawInput""#));
        assert!(result_update.contains(r#""sessionUpdate":"tool_call_update""#));
        assert!(result_update.contains(r#""toolCallId":"tool_call_"#));
        assert!(result_update.contains(r#""status":"completed""#));
        assert!(result_update.contains(r#""content":[{"#));
        assert!(result_update.contains(r#""rawOutput""#));
        assert!(result_update.contains("hello acp tool"));
        assert!(result_update.contains(r#""isError":false"#));
        assert!(rendered.contains(r#""id":23"#));
        assert!(rendered.contains("hello acp tool"));
        assert!(rendered.contains(r#""isError":false"#));
    }

    #[test]
    fn acp_session_tools_call_streams_large_tool_output_updates() {
        let mut state = acp_state("acp-tools-large-output");
        let workspace = temp_dir("acp-tools-large-output-workspace");
        fs::write(
            workspace.join("large.txt"),
            format!("{}\n", "large-output-".repeat(600)),
        )
        .unwrap();
        let new_request = format!(
            r#"{{"jsonrpc":"2.0","id":32,"method":"session/new","params":{{"cwd":"{}"}}}}"#,
            crate::util::json::json_escape(&workspace.display().to_string())
        );
        let AcpDispatch::Responses(new_responses) =
            acp_dispatch_for_message(&new_request, &mut state)
        else {
            panic!("expected ACP responses");
        };
        let rendered_new = json_value_to_string(&new_responses[0]);
        let root = parse_root_object(&rendered_new).unwrap();
        let acp_session_id = root
            .get("result")
            .and_then(json_as_object)
            .and_then(|result| result.get("sessionId"))
            .and_then(json_as_string)
            .unwrap();
        let request = format!(
            r#"{{"jsonrpc":"2.0","id":33,"method":"session/tools/call","params":{{"sessionId":"{acp_session_id}","name":"read_file","arguments":{{"path":"large.txt","max_lines":1}}}}}}"#
        );

        let AcpDispatch::Responses(responses) = acp_dispatch_for_message(&request, &mut state)
        else {
            panic!("expected ACP responses");
        };

        assert!(responses.len() > 3);
        let call_update = json_value_to_string(&responses[0]);
        let first_progress_update = json_value_to_string(&responses[1]);
        let final_update = json_value_to_string(&responses[responses.len() - 2]);
        let rendered = json_value_to_string(&responses[responses.len() - 1]);
        assert!(call_update.contains(r#""sessionUpdate":"tool_call""#));
        assert!(first_progress_update.contains(r#""sessionUpdate":"tool_call_update""#));
        assert!(first_progress_update.contains(r#""status":"in_progress""#));
        assert!(first_progress_update.contains(r#""partial":true"#));
        assert!(first_progress_update.contains(r#""chunkIndex":1"#));
        assert!(first_progress_update.contains(r#""chunkCount":4"#));
        assert!(first_progress_update.contains(r#""rawOutput""#));
        assert!(final_update.contains(r#""status":"completed""#));
        assert!(!final_update.contains(r#""partial":true"#));
        assert!(rendered.contains(r#""id":33"#));
        assert!(rendered.contains("large-output-large-output"));
        assert!(rendered.contains(r#""isError":false"#));
    }

    #[test]
    fn acp_session_tools_call_streams_shell_output_while_running() {
        let mut state = acp_state("acp-tools-stream-shell");
        let workspace = temp_dir("acp-tools-stream-shell-workspace");
        let session = state
            .store
            .create_session(
                "ACP stream shell".to_string(),
                workspace.display().to_string(),
            )
            .unwrap();
        let thread = state
            .store
            .create_thread_for_session(
                &session.id,
                "ACP stream shell thread".to_string(),
                workspace.display().to_string(),
                "deepseek-coder".to_string(),
                "agent".to_string(),
            )
            .unwrap();
        let acp_session_id = "stream-shell-session".to_string();
        state.sessions.insert(
            acp_session_id.clone(),
            AcpSession {
                cwd: workspace,
                runtime_session_id: Some(session.id),
                runtime_thread_id: Some(thread.id.clone()),
            },
        );
        let responder =
            spawn_mcp_permission_responder(state.store.clone(), thread.id.clone(), "approved");
        let request = format!(
            r#"{{"jsonrpc":"2.0","id":34,"method":"session/tools/call","params":{{"sessionId":"{acp_session_id}","name":"task_shell_start","arguments":{{"command":"echo acp-stream && sleep 0.1 && echo acp-done","stream":true,"stream_timeout_ms":2000}}}}}}"#
        );
        let mut output = Vec::new();

        let handled = acp_try_streaming_tool_call(&request, &mut state, &mut output).unwrap();
        responder.join().unwrap();

        assert!(handled);
        let rendered = String::from_utf8(output).unwrap();
        let lines = rendered.lines().collect::<Vec<_>>();
        assert!(lines.len() >= 4, "{rendered}");
        assert!(lines[0].contains(r#""sessionUpdate":"tool_call""#));
        assert!(rendered.contains(r#""partial":true"#), "{rendered}");
        assert!(rendered.contains("stdout_delta"), "{rendered}");
        assert!(rendered.contains("acp-stream"), "{rendered}");
        assert!(rendered.contains("acp-done"), "{rendered}");
        assert!(rendered.contains(r#""id":34"#), "{rendered}");
        let progress_index = rendered.find(r#""partial":true"#).unwrap();
        let final_index = rendered.rfind(r#""id":34"#).unwrap();
        assert!(progress_index < final_index, "{rendered}");
        let items = state.store.list_items(&thread.id, None).unwrap();
        assert!(items
            .iter()
            .any(|item| item.item_type == "tool_result" && item.content.contains("acp-done")));
    }

    #[test]
    fn acp_session_tools_call_streaming_shell_requires_loaded_runtime_thread() {
        let mut state = acp_state("acp-tools-stream-shell-gate");
        let workspace = temp_dir("acp-tools-stream-shell-gate-workspace");
        let acp_session_id = "stream-shell-readonly-session".to_string();
        state.sessions.insert(
            acp_session_id.clone(),
            AcpSession {
                cwd: workspace,
                runtime_session_id: None,
                runtime_thread_id: None,
            },
        );
        let request = format!(
            r#"{{"jsonrpc":"2.0","id":35,"method":"session/tools/call","params":{{"sessionId":"{acp_session_id}","name":"task_shell_start","arguments":{{"command":"echo should-not-run","stream":true}}}}}}"#
        );
        let mut output = Vec::new();

        let handled = acp_try_streaming_tool_call(&request, &mut state, &mut output).unwrap();

        assert!(handled);
        let rendered = String::from_utf8(output).unwrap();
        assert!(rendered.contains(r#""sessionUpdate":"tool_call""#));
        assert!(
            rendered.contains("requires a loaded runtime thread"),
            "{rendered}"
        );
        assert!(rendered.contains(r#""isError":true"#), "{rendered}");
        assert!(!rendered.contains("stdout_delta"), "{rendered}");
    }

    #[test]
    fn acp_loaded_session_tools_call_write_file_uses_runtime_approval() {
        let mut state = acp_state("acp-tools-write-file");
        let workspace = temp_dir("acp-tools-write-file-workspace");
        let session = state
            .store
            .create_session(
                "ACP tool session".to_string(),
                workspace.display().to_string(),
            )
            .unwrap();
        let thread = state
            .store
            .create_thread_for_session(
                &session.id,
                "ACP tool thread".to_string(),
                workspace.display().to_string(),
                "deepseek-coder".to_string(),
                "agent".to_string(),
            )
            .unwrap();
        let load_request = format!(
            r#"{{"jsonrpc":"2.0","id":24,"method":"session/load","params":{{"sessionId":"{}","threadId":"{}"}}}}"#,
            session.id, thread.id
        );
        let AcpDispatch::Responses(load_responses) =
            acp_dispatch_for_message(&load_request, &mut state)
        else {
            panic!("expected ACP responses");
        };
        let rendered_load = json_value_to_string(&load_responses[0]);
        let root = parse_root_object(&rendered_load).unwrap();
        let acp_session_id = root
            .get("result")
            .and_then(json_as_object)
            .and_then(|result| result.get("sessionId"))
            .and_then(json_as_string)
            .unwrap();
        let list_request = format!(
            r#"{{"jsonrpc":"2.0","id":25,"method":"session/tools/list","params":{{"sessionId":"{acp_session_id}"}}}}"#
        );
        let AcpDispatch::Responses(list_responses) =
            acp_dispatch_for_message(&list_request, &mut state)
        else {
            panic!("expected ACP responses");
        };
        let rendered_list = json_value_to_string(&list_responses[0]);
        assert!(rendered_list.contains(r#""name":"write_file""#));
        assert!(rendered_list.contains(r#""name":"note""#));
        assert!(!rendered_list.contains(r#""name":"remember""#));
        assert!(rendered_list.contains(r#""name":"edit_file""#));
        assert!(rendered_list.contains(r#""name":"apply_patch""#));
        assert!(rendered_list.contains(r#""name":"image_analyze""#));

        let responder =
            spawn_mcp_permission_responder(state.store.clone(), thread.id.clone(), "approved");
        let call_request = format!(
            r#"{{"jsonrpc":"2.0","id":26,"method":"session/tools/call","params":{{"sessionId":"{acp_session_id}","name":"write_file","arguments":{{"path":"out.txt","content":"from acp\n"}}}}}}"#
        );

        let AcpDispatch::Responses(call_responses) =
            acp_dispatch_for_message(&call_request, &mut state)
        else {
            panic!("expected ACP responses");
        };
        responder.join().unwrap();
        assert_eq!(call_responses.len(), 3);
        let rendered_call = json_value_to_string(&call_responses[2]);
        let turns = state.store.list_turns(&thread.id).unwrap();
        let items = state.store.list_items(&thread.id, None).unwrap();
        let events = state.store.read_events(&thread.id, 0).unwrap();
        let call_update = json_value_to_string(&call_responses[0]);
        let result_update = json_value_to_string(&call_responses[1]);

        assert!(call_update.contains(r#""sessionUpdate":"tool_call""#));
        assert!(call_update.contains(r#""title":"Editing workspace""#));
        assert!(call_update.contains(r#""kind":"edit""#));
        assert!(call_update.contains(r#""status":"in_progress""#));
        assert!(result_update.contains(r#""sessionUpdate":"tool_call_update""#));
        assert!(result_update.contains(r#""status":"completed""#));
        assert!(result_update.contains(r#""rawOutput""#));
        assert!(rendered_call.contains("Wrote 9 bytes to out.txt"));
        assert!(rendered_call.contains(r#""isError":false"#));
        assert!(rendered_call.contains(r#""id":26"#));
        assert_eq!(
            fs::read_to_string(workspace.join("out.txt")).unwrap(),
            "from acp\n"
        );
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].role, "assistant");
        assert_eq!(turns[0].status, "completed");
        assert!(turns[0]
            .content
            .contains("ACP tool call `write_file` completed"));
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].item_type, "tool_call");
        assert_eq!(items[0].status, "completed");
        assert!(items[0].content.contains(r#""tool":"write_file""#));
        assert_eq!(items[1].item_type, "tool_result");
        assert_eq!(items[1].status, "completed");
        assert!(items[1].content.contains("Wrote 9 bytes to out.txt"));
        assert!(call_update.contains(&turns[0].id));
        assert!(call_update.contains(&items[0].id));
        assert!(result_update.contains(&turns[0].id));
        assert!(result_update.contains(&items[0].id));
        assert!(result_update.contains(&items[1].id));
        assert!(events.iter().any(|event| {
            event.kind == "permission_request"
                && event.turn_id.as_deref() == Some(turns[0].id.as_str())
                && json_value_to_string(&event.payload).contains(r#""tool":"write_file""#)
        }));
    }

    #[test]
    fn acp_session_checkpoints_lists_loaded_thread_snapshots() {
        let mut state = acp_state("acp-checkpoints-list");
        let repo = temp_git_repo("acp-checkpoints-list-repo");
        fs::write(repo.join("src.txt"), "checkpoint one\n").unwrap();
        let session = state
            .store
            .create_session(
                "ACP checkpoint session".to_string(),
                repo.display().to_string(),
            )
            .unwrap();
        let thread = state
            .store
            .create_thread_for_session(
                &session.id,
                "ACP checkpoint thread".to_string(),
                repo.display().to_string(),
                "deepseek-coder".to_string(),
                "agent".to_string(),
            )
            .unwrap();
        let snapshot = state
            .rollback
            .create_snapshot(&repo, "visible checkpoint".to_string())
            .unwrap();
        state
            .rollback
            .bind_snapshot_runtime(&snapshot.id, Some(&thread.id), Some("turn-visible"))
            .unwrap();

        let other_repo = temp_git_repo("acp-checkpoints-list-other-repo");
        fs::write(other_repo.join("src.txt"), "checkpoint two\n").unwrap();
        let other_snapshot = state
            .rollback
            .create_snapshot(&other_repo, "hidden checkpoint".to_string())
            .unwrap();
        state
            .rollback
            .bind_snapshot_runtime(
                &other_snapshot.id,
                Some("thread-other"),
                Some("turn-hidden"),
            )
            .unwrap();

        let load_request = format!(
            r#"{{"jsonrpc":"2.0","id":7,"method":"session/load","params":{{"sessionId":"{}","threadId":"{}"}}}}"#,
            session.id, thread.id
        );
        let AcpDispatch::Responses(load_responses) =
            acp_dispatch_for_message(&load_request, &mut state)
        else {
            panic!("expected ACP responses");
        };
        let rendered_load = json_value_to_string(&load_responses[0]);
        let root = parse_root_object(&rendered_load).unwrap();
        let acp_session_id = root
            .get("result")
            .and_then(json_as_object)
            .and_then(|result| result.get("sessionId"))
            .and_then(json_as_string)
            .unwrap();
        let request = format!(
            r#"{{"jsonrpc":"2.0","id":8,"method":"session/checkpoints","params":{{"sessionId":"{acp_session_id}","limit":10}}}}"#
        );

        let AcpDispatch::Responses(responses) = acp_dispatch_for_message(&request, &mut state)
        else {
            panic!("expected ACP responses");
        };
        let rendered = json_value_to_string(&responses[0]);

        assert!(rendered.contains(r#""checkpoints""#));
        assert!(rendered.contains(&snapshot.id));
        assert!(rendered.contains("visible checkpoint"));
        assert!(!rendered.contains(&other_snapshot.id));
        assert!(!rendered.contains("hidden checkpoint"));
    }

    #[test]
    fn acp_checkpoint_read_returns_manifest_and_patch_by_turn_id() {
        let mut state = acp_state("acp-checkpoint-read");
        let repo = temp_git_repo("acp-checkpoint-read-repo");
        fs::write(repo.join("src.txt"), "checkpoint patch\n").unwrap();
        let snapshot = state
            .rollback
            .create_snapshot(&repo, "read checkpoint".to_string())
            .unwrap();
        state
            .rollback
            .bind_snapshot_runtime(&snapshot.id, Some("thread-acp"), Some("turn-acp"))
            .unwrap();

        let dispatch = acp_dispatch_for_message(
            r#"{"jsonrpc":"2.0","id":9,"method":"session/checkpoint/read","params":{"checkpointId":"turn-acp","includePatch":true}}"#,
            &mut state,
        );
        let AcpDispatch::Responses(responses) = dispatch else {
            panic!("expected ACP responses");
        };
        let rendered = json_value_to_string(&responses[0]);

        assert!(rendered.contains(r#""id":9"#));
        assert!(rendered.contains(r#""checkpoint""#));
        assert!(rendered.contains(&snapshot.id));
        assert!(rendered.contains("read checkpoint"));
        assert!(rendered.contains(r#""patch""#));
        assert!(rendered.contains("diff --git"));
        assert!(rendered.contains("checkpoint patch"));
    }

    #[test]
    fn acp_checkpoint_restore_dry_run_does_not_mutate_worktree() {
        let mut state = acp_state("acp-checkpoint-restore-dry-run");
        let repo = temp_git_repo("acp-checkpoint-restore-dry-run-repo");
        fs::write(repo.join("src.txt"), "snapshot version\n").unwrap();
        let snapshot = state
            .rollback
            .create_snapshot(&repo, "restore dry-run checkpoint".to_string())
            .unwrap();
        fs::write(repo.join("src.txt"), "later version\n").unwrap();
        let request = format!(
            r#"{{"jsonrpc":"2.0","id":10,"method":"session/checkpoint/restore","params":{{"checkpointId":"{}"}}}}"#,
            snapshot.id
        );

        let dispatch = acp_dispatch_for_message(&request, &mut state);
        let AcpDispatch::Responses(responses) = dispatch else {
            panic!("expected ACP responses");
        };
        let rendered = json_value_to_string(&responses[0]);

        assert!(rendered.contains(r#""id":10"#));
        assert!(rendered.contains(r#""mode":"dry_run""#));
        assert!(rendered.contains(r#""applied":false"#));
        assert!(rendered.contains(&snapshot.id));
        assert_eq!(
            fs::read_to_string(repo.join("src.txt")).unwrap(),
            "later version\n"
        );
    }

    #[test]
    fn acp_checkpoint_restore_apply_restores_loaded_session_turn() {
        let mut state = acp_state("acp-checkpoint-restore-apply");
        let repo = temp_git_repo("acp-checkpoint-restore-apply-repo");
        fs::write(repo.join("src.txt"), "snapshot version\n").unwrap();
        let session = state
            .store
            .create_session(
                "ACP restore session".to_string(),
                repo.display().to_string(),
            )
            .unwrap();
        let thread = state
            .store
            .create_thread_for_session(
                &session.id,
                "ACP restore thread".to_string(),
                repo.display().to_string(),
                "deepseek-coder".to_string(),
                "agent".to_string(),
            )
            .unwrap();
        let snapshot = state
            .rollback
            .create_snapshot(&repo, "restore apply checkpoint".to_string())
            .unwrap();
        state
            .rollback
            .bind_snapshot_runtime(&snapshot.id, Some(&thread.id), Some("turn-acp-restore"))
            .unwrap();
        fs::write(repo.join("src.txt"), "later version\n").unwrap();
        let load_request = format!(
            r#"{{"jsonrpc":"2.0","id":11,"method":"session/load","params":{{"sessionId":"{}","threadId":"{}"}}}}"#,
            session.id, thread.id
        );
        let AcpDispatch::Responses(load_responses) =
            acp_dispatch_for_message(&load_request, &mut state)
        else {
            panic!("expected ACP responses");
        };
        let rendered_load = json_value_to_string(&load_responses[0]);
        let root = parse_root_object(&rendered_load).unwrap();
        let acp_session_id = root
            .get("result")
            .and_then(json_as_object)
            .and_then(|result| result.get("sessionId"))
            .and_then(json_as_string)
            .unwrap();
        let request = format!(
            r#"{{"jsonrpc":"2.0","id":12,"method":"session/checkpoint/restore","params":{{"sessionId":"{acp_session_id}","checkpointId":"turn-acp-restore","apply":true}}}}"#
        );

        let dispatch = acp_dispatch_for_message(&request, &mut state);
        let AcpDispatch::Responses(responses) = dispatch else {
            panic!("expected ACP responses");
        };
        let rendered = json_value_to_string(&responses[0]);

        assert!(rendered.contains(r#""id":12"#));
        assert!(rendered.contains(r#""mode":"applied""#));
        assert!(rendered.contains(r#""applied":true"#));
        assert!(rendered.contains(r#""changed_files":["src.txt"]"#));
        assert_eq!(
            fs::read_to_string(repo.join("src.txt")).unwrap(),
            "snapshot version\n"
        );
    }

    #[test]
    fn acp_loaded_session_prompt_records_durable_turns_and_items() {
        let mut state = acp_state("acp-loaded-prompt");
        let workspace = temp_dir("acp-loaded-prompt-workspace");
        let session = state
            .store
            .create_session(
                "ACP durable prompt".to_string(),
                workspace.display().to_string(),
            )
            .unwrap();
        let thread = state
            .store
            .create_thread_for_session(
                &session.id,
                "ACP prompt thread".to_string(),
                workspace.display().to_string(),
                "deepseek-coder".to_string(),
                "agent".to_string(),
            )
            .unwrap();
        let load_request = format!(
            r#"{{"jsonrpc":"2.0","id":5,"method":"session/load","params":{{"sessionId":"{}","threadId":"{}"}}}}"#,
            session.id, thread.id
        );
        let dispatch = acp_dispatch_for_message(&load_request, &mut state);
        let AcpDispatch::Responses(load_responses) = dispatch else {
            panic!("expected ACP responses");
        };
        let rendered_load = json_value_to_string(&load_responses[0]);
        let root = parse_root_object(&rendered_load).unwrap();
        let acp_session_id = root
            .get("result")
            .and_then(json_as_object)
            .and_then(|result| result.get("sessionId"))
            .and_then(json_as_string)
            .unwrap();
        let prompt_request = format!(
            r#"{{"jsonrpc":"2.0","id":6,"method":"session/prompt","params":{{"sessionId":"{acp_session_id}","prompt":"Summarize ACP durable prompt"}}}}"#
        );

        let dispatch = acp_dispatch_for_message(&prompt_request, &mut state);
        let AcpDispatch::Responses(prompt_responses) = dispatch else {
            panic!("expected ACP responses");
        };
        let turns = state.store.list_turns(&thread.id).unwrap();
        let items = state.store.list_items(&thread.id, None).unwrap();

        assert_eq!(prompt_responses.len(), 2);
        assert_eq!(turns.len(), 2);
        assert_eq!(turns[0].role, "user");
        assert_eq!(turns[1].role, "assistant");
        assert_eq!(items.len(), 2);
        assert!(items.iter().any(|item| item.role.as_deref() == Some("user")
            && item.content.contains("Summarize ACP durable prompt")));
        assert!(items.iter().any(
            |item| item.role.as_deref() == Some("assistant") && !item.content.trim().is_empty()
        ));
    }

    #[test]
    fn acp_extract_prompt_text_accepts_text_and_resource_blocks() {
        let prompt = JsonValue::Array(vec![
            object([
                ("type", JsonValue::String("text".to_string())),
                ("text", JsonValue::String("Review this file".to_string())),
            ]),
            object([
                ("type", JsonValue::String("resource".to_string())),
                (
                    "resource",
                    object([
                        ("uri", JsonValue::String("file:///tmp/app.rs".to_string())),
                        ("mimeType", JsonValue::String("text/rust".to_string())),
                        ("text", JsonValue::String("fn main() {}".to_string())),
                    ]),
                ),
            ]),
            object([
                ("type", JsonValue::String("resource_link".to_string())),
                ("uri", JsonValue::String("file:///tmp/lib.rs".to_string())),
            ]),
        ]);

        let text = acp_extract_prompt_text(&prompt).expect("prompt text");

        assert!(text.contains("Review this file"));
        assert!(text.contains("fn main() {}"));
        assert!(text.contains("@file:///tmp/lib.rs"));
    }

    #[test]
    fn acp_session_prompt_emits_update_and_end_turn() {
        let mut state = acp_state("acp-prompt");
        let dispatch = acp_dispatch_for_message(
            r#"{"jsonrpc":"2.0","id":1,"method":"session/new","params":{}}"#,
            &mut state,
        );
        let AcpDispatch::Responses(responses) = dispatch else {
            panic!("expected ACP responses");
        };
        let rendered = json_value_to_string(&responses[0]);
        let root = parse_root_object(&rendered).unwrap();
        let session_id = root
            .get("result")
            .and_then(json_as_object)
            .and_then(|result| result.get("sessionId"))
            .and_then(json_as_string)
            .unwrap()
            .to_string();
        let request = format!(
            r#"{{"jsonrpc":"2.0","id":2,"method":"session/prompt","params":{{"sessionId":"{session_id}","prompt":"Say hello from ACP"}}}}"#
        );

        let dispatch = acp_dispatch_for_message(&request, &mut state);
        let AcpDispatch::Responses(responses) = dispatch else {
            panic!("expected ACP responses");
        };

        assert_eq!(responses.len(), 2);
        let update = json_value_to_string(&responses[0]);
        let result = json_value_to_string(&responses[1]);
        assert!(update.contains(r#""method":"session/update""#));
        let update_root = parse_root_object(&update).unwrap();
        let text = update_root
            .get("params")
            .and_then(json_as_object)
            .and_then(|params| params.get("update"))
            .and_then(json_as_object)
            .and_then(|update| update.get("content"))
            .and_then(json_as_object)
            .and_then(|content| content.get("text"))
            .and_then(json_as_string)
            .unwrap();
        assert!(!text.trim().is_empty());
        assert!(result.contains(r#""id":2"#));
        assert!(result.contains(r#""stopReason":"end_turn""#));
    }

    #[test]
    fn health_endpoint_returns_stable_json() {
        let store = temp_store("health");
        let response =
            response_for_request("GET /health HTTP/1.1\r\nHost: localhost\r\n\r\n", &store);
        assert_eq!(response.status, 200);
        let root = parse_root_object(&response.body).unwrap();
        assert_eq!(root.get("status").and_then(json_as_string), Some("ok"));
        assert_eq!(
            root.get("schema").and_then(json_as_string),
            Some("deepseek.runtime.health.v1")
        );
    }

    #[test]
    fn runtime_endpoint_advertises_incomplete_contract_truthfully() {
        let store = temp_store("runtime");
        let response =
            response_for_request("GET /runtime HTTP/1.1\r\nHost: localhost\r\n\r\n", &store);
        assert_eq!(response.status, 200);
        let root = parse_root_object(&response.body).unwrap();
        let capabilities = root
            .get("capabilities")
            .and_then(json_as_object)
            .expect("capabilities should be an object");
        assert!(matches!(
            capabilities.get("health"),
            Some(JsonValue::Bool(true))
        ));
        assert!(matches!(
            capabilities.get("sessions"),
            Some(JsonValue::Bool(true))
        ));
        assert!(matches!(
            capabilities.get("threads"),
            Some(JsonValue::Bool(true))
        ));
        assert!(matches!(
            capabilities.get("thread_compaction"),
            Some(JsonValue::Bool(true))
        ));
        assert!(matches!(
            capabilities.get("turns"),
            Some(JsonValue::Bool(true))
        ));
        assert!(matches!(
            capabilities.get("items"),
            Some(JsonValue::Bool(true))
        ));
        assert!(matches!(
            capabilities.get("events"),
            Some(JsonValue::Bool(true))
        ));
        assert!(matches!(
            capabilities.get("events_sse"),
            Some(JsonValue::Bool(true))
        ));
        assert!(matches!(
            capabilities.get("events_sse_wait"),
            Some(JsonValue::Bool(true))
        ));
        assert!(matches!(
            capabilities.get("events_sse_follow"),
            Some(JsonValue::Bool(true))
        ));
        assert!(matches!(
            capabilities.get("diagnostics"),
            Some(JsonValue::Bool(true))
        ));
        assert!(matches!(
            capabilities.get("diagnostics_broker"),
            Some(JsonValue::Bool(true))
        ));
        assert!(matches!(
            capabilities.get("tasks"),
            Some(JsonValue::Bool(true))
        ));
        assert!(matches!(
            capabilities.get("task_claim"),
            Some(JsonValue::Bool(true))
        ));
        assert!(matches!(
            capabilities.get("task_cancel"),
            Some(JsonValue::Bool(true))
        ));
        assert!(matches!(
            capabilities.get("task_pause"),
            Some(JsonValue::Bool(true))
        ));
        assert!(matches!(
            capabilities.get("task_resume"),
            Some(JsonValue::Bool(true))
        ));
        assert!(matches!(
            capabilities.get("automations"),
            Some(JsonValue::Bool(true))
        ));
        assert!(matches!(
            capabilities.get("automation_trigger"),
            Some(JsonValue::Bool(true))
        ));
        assert!(matches!(
            capabilities.get("usage"),
            Some(JsonValue::Bool(true))
        ));
        assert!(matches!(
            capabilities.get("usage_summary"),
            Some(JsonValue::Bool(true))
        ));
        assert!(response.body.contains("/v1/automations"));
        assert!(response.body.contains("/v1/automations/{id}/trigger"));
        assert!(response.body.contains("/v1/diagnostics"));
        assert!(response.body.contains("/v1/threads/{id}/automations"));
        assert!(response.body.contains("/v1/threads/{id}/compact"));
        assert!(response.body.contains("/v1/threads/{id}/items"));
        assert!(response
            .body
            .contains("/v1/threads/{id}/turns/{turn_id}/items"));
        assert!(response.body.contains("/v1/threads/{id}/events/stream"));
        assert!(response.body.contains("/v1/threads/{id}/usage"));
        assert!(response.body.contains("/v1/usage/summary"));
        assert!(response.body.contains("/v1/usage"));
        assert!(response.body.contains("/v1/sessions"));
        assert!(response.body.contains("/v1/sessions/{id}/threads"));
        assert!(response.body.contains("/v1/tasks"));
        assert!(response.body.contains("/v1/tasks/{id}/claim"));
        assert!(response.body.contains("/v1/tasks/{id}/cancel"));
        assert!(response.body.contains("/v1/tasks/{id}/pause"));
        assert!(response.body.contains("/v1/tasks/{id}/resume"));
        assert!(response.body.contains("/v1/threads/{id}/tasks"));
    }

    #[test]
    fn diagnostics_endpoint_runs_via_runtime_broker() {
        let store = temp_store("diagnostics");
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "deepseek-serve-diagnostics-{}-{nanos}",
            std::process::id()
        ));
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("README.md"), "# docs\n").unwrap();
        let body = format!(
            "{{\"cwd\":\"{}\",\"paths\":[\"README.md\"]}}",
            root.display()
        );
        let response = response_for_request(
            &format!(
                "POST /v1/diagnostics HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n{}",
                body.len(),
                body
            ),
            &store,
        );

        assert_eq!(response.status, 200);
        let root = parse_root_object(&response.body).unwrap();
        assert_eq!(
            root.get("schema").and_then(json_as_string),
            Some("deepseek.runtime.diagnostics.v1")
        );
        assert!(matches!(root.get("skipped"), Some(JsonValue::Bool(false))));
        let report = root
            .get("report")
            .and_then(json_as_object)
            .expect("diagnostic report");
        assert_eq!(
            report.get("status").and_then(json_as_string),
            Some("unavailable")
        );
    }

    #[test]
    fn unknown_endpoint_returns_json_404() {
        let store = temp_store("missing");
        let response =
            response_for_request("GET /missing HTTP/1.1\r\nHost: localhost\r\n\r\n", &store);
        assert_eq!(response.status, 404);
        assert!(response.body.contains("not_found"));
    }

    #[test]
    fn threads_endpoint_creates_lists_and_shows_thread() {
        let store = temp_store("threads");
        let create = response_for_request(
            "POST /v1/threads HTTP/1.1\r\nHost: localhost\r\nContent-Length: 68\r\n\r\n{\"title\":\"Runtime parity\",\"workspace\":\".\",\"model\":\"deepseek-coder\"}",
            &store,
        );
        assert_eq!(create.status, 201);
        let create_root = parse_root_object(&create.body).unwrap();
        let thread = create_root
            .get("thread")
            .and_then(json_as_object)
            .expect("thread object");
        let thread_id = thread
            .get("id")
            .and_then(json_as_string)
            .expect("thread id")
            .to_string();

        let list = response_for_request(
            "GET /v1/threads?limit=10 HTTP/1.1\r\nHost: localhost\r\n\r\n",
            &store,
        );
        assert_eq!(list.status, 200);
        assert!(list.body.contains("Runtime parity"));

        let show = response_for_request(
            &format!("GET /v1/threads/{thread_id} HTTP/1.1\r\nHost: localhost\r\n\r\n"),
            &store,
        );
        assert_eq!(show.status, 200);
        assert!(show.body.contains("\"turns\":[]"));
    }

    #[test]
    fn sessions_endpoint_creates_lists_shows_and_links_threads() {
        let store = temp_store("sessions");
        let create = response_for_request(
            "POST /v1/sessions HTTP/1.1\r\nHost: localhost\r\nContent-Length: 43\r\n\r\n{\"title\":\"Daily work\",\"workspace\":\"/tmp/ws\"}",
            &store,
        );
        assert_eq!(create.status, 201);
        let create_root = parse_root_object(&create.body).unwrap();
        let session = create_root
            .get("session")
            .and_then(json_as_object)
            .expect("session object");
        let session_id = session
            .get("id")
            .and_then(json_as_string)
            .expect("session id")
            .to_string();

        let list = response_for_request(
            "GET /v1/sessions?limit=10 HTTP/1.1\r\nHost: localhost\r\n\r\n",
            &store,
        );
        assert_eq!(list.status, 200);
        assert!(list.body.contains("Daily work"));

        let thread = response_for_request(
            &format!(
                "POST /v1/sessions/{session_id}/threads HTTP/1.1\r\nHost: localhost\r\nContent-Length: 58\r\n\r\n{{\"title\":\"Follow up\",\"workspace\":\"/tmp/ws\",\"mode\":\"agent\"}}"
            ),
            &store,
        );
        assert_eq!(thread.status, 201);
        assert!(thread.body.contains(&session_id));

        let show = response_for_request(
            &format!("GET /v1/sessions/{session_id} HTTP/1.1\r\nHost: localhost\r\n\r\n"),
            &store,
        );
        assert_eq!(show.status, 200);
        assert!(show.body.contains("\"thread_count\":1"));
        assert!(show.body.contains("\"threads\":["));
        assert!(show.body.contains("Follow up"));

        let linked_thread = response_for_request(
            &format!(
                "POST /v1/threads HTTP/1.1\r\nHost: localhost\r\nContent-Length: 64\r\n\r\n{{\"title\":\"Linked\",\"workspace\":\"/tmp/ws\",\"session_id\":\"{session_id}\"}}"
            ),
            &store,
        );
        assert_eq!(linked_thread.status, 201);
        assert!(linked_thread.body.contains(&session_id));
    }

    #[test]
    fn automation_endpoints_create_filter_and_show_automations() {
        let store = temp_store("automations");
        let session = store
            .create_session("Daily work".to_string(), ".".to_string())
            .unwrap();
        let thread = store
            .create_thread_for_session(
                &session.id,
                "Runtime parity".to_string(),
                ".".to_string(),
                "deepseek-coder".to_string(),
                "agent".to_string(),
            )
            .unwrap();

        let create = response_for_request(
            &format!(
                "POST /v1/threads/{}/automations HTTP/1.1\r\nHost: localhost\r\nContent-Length: 106\r\n\r\n{{\"name\":\"Nightly check\",\"status\":\"active\",\"schedule\":\"daily\",\"prompt\":\"run diagnostics\"}}",
                thread.id
            ),
            &store,
        );
        assert_eq!(create.status, 201);
        assert!(create.body.contains("deepseek.runtime.automation.v1"));
        assert!(create.body.contains("\"status\":\"active\""));
        let create_root = parse_root_object(&create.body).unwrap();
        let automation_id = create_root
            .get("automation")
            .and_then(json_as_object)
            .and_then(|automation| automation.get("id"))
            .and_then(json_as_string)
            .expect("automation id")
            .to_string();

        let list = response_for_request(
            &format!(
                "GET /v1/automations?session_id={}&thread_id={} HTTP/1.1\r\nHost: localhost\r\n\r\n",
                session.id, thread.id
            ),
            &store,
        );
        assert_eq!(list.status, 200);
        assert!(list.body.contains("deepseek.runtime.automations.v1"));
        assert!(list.body.contains(&automation_id));

        let show = response_for_request(
            &format!("GET /v1/automations/{automation_id} HTTP/1.1\r\nHost: localhost\r\n\r\n"),
            &store,
        );
        assert_eq!(show.status, 200);
        assert!(show.body.contains("\"name\":\"Nightly check\""));

        let trigger = response_for_request(
            &format!(
                "POST /v1/automations/{automation_id}/trigger HTTP/1.1\r\nHost: localhost\r\nContent-Length: 29\r\n\r\n{{\"prompt\":\"manual run now\"}}"
            ),
            &store,
        );
        assert_eq!(trigger.status, 201);
        assert!(trigger
            .body
            .contains("deepseek.runtime.automation_trigger.v1"));
        assert!(trigger.body.contains("\"summary\":\"manual run now\""));

        let events = response_for_request(
            &format!(
                "GET /v1/threads/{}/events?since_seq=1 HTTP/1.1\r\nHost: localhost\r\n\r\n",
                thread.id
            ),
            &store,
        );
        assert!(events.body.contains("automation_recorded"));
        assert!(events.body.contains("automation_triggered"));
    }

    #[test]
    fn task_endpoints_create_filter_and_show_tasks() {
        let store = temp_store("tasks");
        let session = store
            .create_session("Daily work".to_string(), ".".to_string())
            .unwrap();
        let thread = store
            .create_thread_for_session(
                &session.id,
                "Runtime parity".to_string(),
                ".".to_string(),
                "deepseek-coder".to_string(),
                "agent".to_string(),
            )
            .unwrap();

        let create = response_for_request(
            &format!(
                "POST /v1/threads/{}/tasks HTTP/1.1\r\nHost: localhost\r\nContent-Length: 62\r\n\r\n{{\"kind\":\"exec\",\"status\":\"completed\",\"summary\":\"done\"}}",
                thread.id
            ),
            &store,
        );
        assert_eq!(create.status, 201);
        assert!(create.body.contains("deepseek.runtime.task.v1"));
        assert!(create.body.contains("\"status\":\"completed\""));
        let create_root = parse_root_object(&create.body).unwrap();
        let task_id = create_root
            .get("task")
            .and_then(json_as_object)
            .and_then(|task| task.get("id"))
            .and_then(json_as_string)
            .expect("task id")
            .to_string();

        let list = response_for_request(
            &format!(
                "GET /v1/tasks?session_id={}&thread_id={} HTTP/1.1\r\nHost: localhost\r\n\r\n",
                session.id, thread.id
            ),
            &store,
        );
        assert_eq!(list.status, 200);
        assert!(list.body.contains("deepseek.runtime.tasks.v1"));
        assert!(list.body.contains(&task_id));

        let show = response_for_request(
            &format!("GET /v1/tasks/{task_id} HTTP/1.1\r\nHost: localhost\r\n\r\n"),
            &store,
        );
        assert_eq!(show.status, 200);
        assert!(show.body.contains("\"summary\":\"done\""));

        let update = response_for_request(
            &format!(
                "PATCH /v1/tasks/{task_id} HTTP/1.1\r\nHost: localhost\r\nContent-Length: 49\r\n\r\n{{\"status\":\"cancelled\",\"summary\":\"cancelled\"}}"
            ),
            &store,
        );
        assert_eq!(update.status, 200);
        assert!(update.body.contains("\"status\":\"cancelled\""));

        let events = response_for_request(
            &format!(
                "GET /v1/threads/{}/events?since_seq=1 HTTP/1.1\r\nHost: localhost\r\n\r\n",
                thread.id
            ),
            &store,
        );
        assert!(events.body.contains("task_recorded"));
        assert!(events.body.contains("task_updated"));
    }

    #[test]
    fn task_claim_endpoint_marks_pending_task_running() {
        let store = temp_store("task-claim");
        let thread = store
            .create_thread(
                "Runtime parity".to_string(),
                ".".to_string(),
                "deepseek-coder".to_string(),
                "agent".to_string(),
            )
            .unwrap();
        let task = store
            .create_task(
                None,
                Some(&thread.id),
                None,
                "agent".to_string(),
                "pending".to_string(),
                "queued task".to_string(),
            )
            .unwrap();

        let claim = response_for_request(
            &format!(
                "POST /v1/tasks/{}/claim HTTP/1.1\r\nHost: localhost\r\nContent-Length: 28\r\n\r\n{{\"runner_id\":\"worker-1\"}}",
                task.id
            ),
            &store,
        );

        assert_eq!(claim.status, 200);
        assert!(claim.body.contains("deepseek.runtime.task_claim.v1"));
        assert!(claim.body.contains("\"status\":\"running\""));
        let events = response_for_request(
            &format!(
                "GET /v1/threads/{}/events?since_seq=1 HTTP/1.1\r\nHost: localhost\r\n\r\n",
                thread.id
            ),
            &store,
        );
        assert!(events.body.contains("task_claimed"));
        assert!(events.body.contains("worker-1"));
    }

    #[test]
    fn task_cancel_endpoint_marks_task_and_appends_cancel_event() {
        let store = temp_store("task-cancel");
        let thread = store
            .create_thread(
                "Runtime parity".to_string(),
                ".".to_string(),
                "deepseek-coder".to_string(),
                "agent".to_string(),
            )
            .unwrap();
        let task = store
            .create_task(
                None,
                Some(&thread.id),
                None,
                "agent".to_string(),
                "running".to_string(),
                "queued task".to_string(),
            )
            .unwrap();

        let cancel = response_for_request(
            &format!(
                "POST /v1/tasks/{}/cancel HTTP/1.1\r\nHost: localhost\r\nContent-Length: 31\r\n\r\n{{\"reason\":\"stop requested\"}}",
                task.id
            ),
            &store,
        );

        assert_eq!(cancel.status, 200);
        assert!(cancel.body.contains("deepseek.runtime.task_cancel.v1"));
        assert!(cancel.body.contains("\"status\":\"cancelled\""));
        assert!(cancel.body.contains("\"kind\":\"cancel_requested\""));
        assert!(cancel.body.contains("\"task_id\""));
        let events = response_for_request(
            &format!(
                "GET /v1/threads/{}/events?since_seq=1 HTTP/1.1\r\nHost: localhost\r\n\r\n",
                thread.id
            ),
            &store,
        );
        assert!(events.body.contains("task_updated"));
        assert!(events.body.contains("cancel_requested"));
        assert!(events.body.contains(&task.id));
    }

    #[test]
    fn task_pause_and_resume_endpoints_control_pending_queue_state() {
        let store = temp_store("task-pause-resume");
        let thread = store
            .create_thread(
                "Runtime parity".to_string(),
                ".".to_string(),
                "deepseek-coder".to_string(),
                "agent".to_string(),
            )
            .unwrap();
        let task = store
            .create_task(
                None,
                Some(&thread.id),
                None,
                "agent".to_string(),
                "pending".to_string(),
                "queued task".to_string(),
            )
            .unwrap();

        let pause = response_for_request(
            &format!(
                "POST /v1/tasks/{}/pause HTTP/1.1\r\nHost: localhost\r\nContent-Length: 33\r\n\r\n{{\"summary\":\"paused for review\"}}",
                task.id
            ),
            &store,
        );
        assert_eq!(pause.status, 200);
        assert!(pause.body.contains("deepseek.runtime.task_pause.v1"));
        assert!(pause.body.contains("\"status\":\"paused\""));

        let claim = response_for_request(
            &format!(
                "POST /v1/tasks/{}/claim HTTP/1.1\r\nHost: localhost\r\nContent-Length: 28\r\n\r\n{{\"runner_id\":\"worker-1\"}}",
                task.id
            ),
            &store,
        );
        assert_eq!(claim.status, 500);
        assert!(claim.body.contains("cannot be claimed"));

        let resume = response_for_request(
            &format!(
                "POST /v1/tasks/{}/resume HTTP/1.1\r\nHost: localhost\r\nContent-Length: 0\r\n\r\n",
                task.id
            ),
            &store,
        );
        assert_eq!(resume.status, 200);
        assert!(resume.body.contains("deepseek.runtime.task_resume.v1"));
        assert!(resume.body.contains("\"status\":\"pending\""));

        let events = response_for_request(
            &format!(
                "GET /v1/threads/{}/events?since_seq=1 HTTP/1.1\r\nHost: localhost\r\n\r\n",
                thread.id
            ),
            &store,
        );
        assert!(events.body.contains("task_updated"));
        assert!(events.body.contains("\"status\":\"paused\""));
        assert!(events.body.contains("\"status\":\"pending\""));
    }

    #[test]
    fn turn_endpoint_records_turn_and_event_stream() {
        let store = temp_store("turns");
        let thread = store
            .create_thread(
                "Runtime parity".to_string(),
                ".".to_string(),
                "deepseek-coder".to_string(),
                "agent".to_string(),
            )
            .unwrap();
        let turn = response_for_request(
            &format!(
                "POST /v1/threads/{}/turns HTTP/1.1\r\nHost: localhost\r\nContent-Length: 33\r\n\r\n{{\"role\":\"user\",\"content\":\"hello\"}}",
                thread.id
            ),
            &store,
        );
        assert_eq!(turn.status, 201);
        assert!(turn.body.contains("\"content\":\"hello\""));

        let events = response_for_request(
            &format!(
                "GET /v1/threads/{}/events?since_seq=1 HTTP/1.1\r\nHost: localhost\r\n\r\n",
                thread.id
            ),
            &store,
        );
        assert_eq!(events.status, 200);
        assert!(events.body.contains("turn_recorded"));
    }

    #[test]
    fn compact_endpoint_appends_summary_and_compaction_event() {
        let store = temp_store("compact");
        let thread = store
            .create_thread(
                "Runtime parity".to_string(),
                ".".to_string(),
                "deepseek-coder".to_string(),
                "agent".to_string(),
            )
            .unwrap();
        for index in 1..=4 {
            store
                .append_turn(
                    &thread.id,
                    "user".to_string(),
                    format!("historical turn {index}"),
                )
                .unwrap();
        }

        let compact = response_for_request(
            &format!(
                "POST /v1/threads/{}/compact HTTP/1.1\r\nHost: localhost\r\nContent-Length: 21\r\n\r\n{{\"keep_tail_turns\":2}}",
                thread.id
            ),
            &store,
        );

        assert_eq!(compact.status, 201);
        assert!(compact
            .body
            .contains("deepseek.runtime.thread_compaction.v1"));
        assert!(compact.body.contains("\"summarized_turn_count\":2"));
        assert!(compact.body.contains("\"summary_source\":\"extractive\""));
        assert!(compact.body.contains("\"item_type\":\"summary\""));

        let events = response_for_request(
            &format!(
                "GET /v1/threads/{}/events?since_seq=1 HTTP/1.1\r\nHost: localhost\r\n\r\n",
                thread.id
            ),
            &store,
        );
        assert_eq!(events.status, 200);
        assert!(events.body.contains("thread_compacted"));
    }

    #[test]
    fn item_endpoints_create_filter_and_show_items() {
        let store = temp_store("items");
        let thread = store
            .create_thread(
                "Runtime parity".to_string(),
                ".".to_string(),
                "deepseek-coder".to_string(),
                "agent".to_string(),
            )
            .unwrap();
        let turn = store
            .append_turn(&thread.id, "assistant".to_string(), "done".to_string())
            .unwrap();

        let create = response_for_request(
            &format!(
                "POST /v1/threads/{}/turns/{}/items HTTP/1.1\r\nHost: localhost\r\nContent-Length: 86\r\n\r\n{{\"item_type\":\"message\",\"role\":\"assistant\",\"content\":\"done\",\"status\":\"completed\"}}",
                thread.id, turn.id
            ),
            &store,
        );
        assert_eq!(create.status, 201);
        assert!(create.body.contains("deepseek.runtime.item.v1"));
        assert!(create.body.contains("\"content\":\"done\""));
        let create_root = parse_root_object(&create.body).unwrap();
        let item_id = create_root
            .get("item")
            .and_then(json_as_object)
            .and_then(|item| item.get("id"))
            .and_then(json_as_string)
            .expect("item id")
            .to_string();

        let list_thread = response_for_request(
            &format!(
                "GET /v1/threads/{}/items HTTP/1.1\r\nHost: localhost\r\n\r\n",
                thread.id
            ),
            &store,
        );
        assert_eq!(list_thread.status, 200);
        assert!(list_thread.body.contains("deepseek.runtime.items.v1"));
        assert!(list_thread.body.contains(&item_id));

        let list_turn = response_for_request(
            &format!(
                "GET /v1/threads/{}/turns/{}/items?limit=1 HTTP/1.1\r\nHost: localhost\r\n\r\n",
                thread.id, turn.id
            ),
            &store,
        );
        assert_eq!(list_turn.status, 200);
        assert!(list_turn.body.contains("\"turn_id\":\""));
        assert!(list_turn.body.contains(&turn.id));

        let show = response_for_request(
            &format!(
                "GET /v1/threads/{}/items/{} HTTP/1.1\r\nHost: localhost\r\n\r\n",
                thread.id, item_id
            ),
            &store,
        );
        assert_eq!(show.status, 200);
        assert!(show.body.contains("\"role\":\"assistant\""));

        let thread_show = response_for_request(
            &format!(
                "GET /v1/threads/{} HTTP/1.1\r\nHost: localhost\r\n\r\n",
                thread.id
            ),
            &store,
        );
        assert_eq!(thread_show.status, 200);
        assert!(thread_show.body.contains("\"items\""));
        assert!(thread_show.body.contains(&item_id));

        let events = response_for_request(
            &format!(
                "GET /v1/threads/{}/events?since_seq=2 HTTP/1.1\r\nHost: localhost\r\n\r\n",
                thread.id
            ),
            &store,
        );
        assert_eq!(events.status, 200);
        assert!(events.body.contains("item_recorded"));
    }

    #[test]
    fn event_stream_endpoint_replays_sse_frames() {
        let store = temp_store("sse");
        let thread = store
            .create_thread(
                "Runtime parity".to_string(),
                ".".to_string(),
                "deepseek-coder".to_string(),
                "agent".to_string(),
            )
            .unwrap();
        store
            .append_turn(&thread.id, "user".to_string(), "hello".to_string())
            .unwrap();

        let stream = response_for_request(
            &format!(
                "GET /v1/threads/{}/events/stream?since_seq=0 HTTP/1.1\r\nHost: localhost\r\n\r\n",
                thread.id
            ),
            &store,
        );
        assert_eq!(stream.status, 200);
        assert_eq!(stream.content_type, "text/event-stream; charset=utf-8");
        assert!(stream.body.contains("id: 1\n"));
        assert!(stream.body.contains("event: thread_created\n"));
        assert!(stream.body.contains("event: turn_recorded\n"));
        assert!(stream.body.contains("data: {"));

        let replay = response_for_request(
            &format!(
                "GET /v1/threads/{}/events/stream?since_seq=1 HTTP/1.1\r\nHost: localhost\r\n\r\n",
                thread.id
            ),
            &store,
        );
        assert!(!replay.body.contains("event: thread_created\n"));
        assert!(replay.body.contains("event: turn_recorded\n"));
    }

    #[test]
    fn event_stream_endpoint_waits_for_new_sse_frames() {
        let store = temp_store("sse-wait");
        let thread = store
            .create_thread(
                "Runtime parity".to_string(),
                ".".to_string(),
                "deepseek-coder".to_string(),
                "agent".to_string(),
            )
            .unwrap();
        let worker_store = store.clone();
        let thread_id = thread.id.clone();

        let handle = thread::spawn(move || {
            response_for_request(
                &format!(
                    "GET /v1/threads/{thread_id}/events/stream?since_seq=1&wait_ms=1000&poll_ms=10 HTTP/1.1\r\nHost: localhost\r\n\r\n",
                ),
                &worker_store,
            )
        });
        thread::sleep(Duration::from_millis(50));
        store
            .append_turn(
                &thread.id,
                "user".to_string(),
                "hello while waiting".to_string(),
            )
            .unwrap();

        let stream = handle.join().unwrap();
        assert_eq!(stream.status, 200);
        assert!(stream.body.contains("id: 2\n"));
        assert!(stream.body.contains("event: turn_recorded\n"));
        assert!(!stream
            .body
            .contains("no runtime events after since_seq before wait timeout"));
    }

    #[test]
    fn global_event_stream_endpoint_replays_events_across_threads() {
        let store = temp_store("global-sse");
        let first = store
            .create_thread(
                "First runtime".to_string(),
                ".".to_string(),
                "deepseek-coder".to_string(),
                "agent".to_string(),
            )
            .unwrap();
        let second = store
            .create_thread(
                "Second runtime".to_string(),
                ".".to_string(),
                "deepseek-coder".to_string(),
                "agent".to_string(),
            )
            .unwrap();

        let stream = response_for_request(
            "GET /v1/events/stream?since_seq=0 HTTP/1.1\r\nHost: localhost\r\n\r\n",
            &store,
        );

        assert_eq!(stream.status, 200);
        assert_eq!(stream.content_type, "text/event-stream; charset=utf-8");
        assert!(stream.body.contains(&format!("id: {}:1\n", first.id)));
        assert!(stream.body.contains(&format!("id: {}:1\n", second.id)));
        assert_eq!(stream.body.matches("event: thread_created\n").count(), 2);
    }

    #[test]
    fn global_event_stream_endpoint_waits_for_new_threads() {
        let store = temp_store("global-sse-wait");
        let existing = store
            .create_thread(
                "Existing runtime".to_string(),
                ".".to_string(),
                "deepseek-coder".to_string(),
                "agent".to_string(),
            )
            .unwrap();
        let worker_store = store.clone();
        let existing_id = existing.id.clone();

        let handle = thread::spawn(move || {
            response_for_request(
                &format!(
                    "GET /v1/events/stream?since={existing_id}:1&wait_ms=1000&poll_ms=10 HTTP/1.1\r\nHost: localhost\r\n\r\n",
                ),
                &worker_store,
            )
        });
        thread::sleep(Duration::from_millis(50));
        let created = store
            .create_thread(
                "Created while waiting".to_string(),
                ".".to_string(),
                "deepseek-coder".to_string(),
                "agent".to_string(),
            )
            .unwrap();

        let stream = handle.join().unwrap();
        assert_eq!(stream.status, 200);
        assert!(stream.body.contains(&format!("id: {}:1\n", created.id)));
        assert!(stream.body.contains("event: thread_created\n"));
        assert!(!stream
            .body
            .contains("no runtime events after cursor before wait timeout"));
    }

    #[test]
    fn event_endpoint_appends_permission_request_events() {
        let store = temp_store("permission-event");
        let thread = store
            .create_thread(
                "Runtime parity".to_string(),
                ".".to_string(),
                "deepseek-coder".to_string(),
                "agent".to_string(),
            )
            .unwrap();
        let create = response_for_request(
            &format!(
                "POST /v1/threads/{}/events HTTP/1.1\r\nHost: localhost\r\nContent-Length: 112\r\n\r\n{{\"type\":\"permission_request\",\"tool\":\"run_shell\",\"kind\":\"shell\",\"target\":\"cargo test\",\"input\":{{\"command\":\"cargo test\"}}}}",
                thread.id
            ),
            &store,
        );
        assert_eq!(create.status, 201);
        assert!(create.body.contains("deepseek.runtime.event.v1"));
        assert!(create.body.contains("\"kind\":\"permission_request\""));
        assert!(create.body.contains("\"tool\":\"run_shell\""));
        assert!(create.body.contains("\"target\":\"cargo test\""));
        let create_root = parse_root_object(&create.body).unwrap();
        let request_id = create_root
            .get("event")
            .and_then(json_as_object)
            .and_then(|event| event.get("id"))
            .and_then(json_as_string)
            .expect("permission request id")
            .to_string();

        let events = response_for_request(
            &format!(
                "GET /v1/threads/{}/events?since_seq=1 HTTP/1.1\r\nHost: localhost\r\n\r\n",
                thread.id
            ),
            &store,
        );
        assert_eq!(events.status, 200);
        assert!(events.body.contains("\"kind\":\"permission_request\""));
        assert!(events.body.contains("\"command\":\"cargo test\""));

        let stream = response_for_request(
            &format!(
                "GET /v1/threads/{}/events/stream?since_seq=1 HTTP/1.1\r\nHost: localhost\r\n\r\n",
                thread.id
            ),
            &store,
        );
        assert!(stream.body.contains("event: permission_request\n"));

        let response = response_for_request(
            &format!(
                "POST /v1/threads/{}/events HTTP/1.1\r\nHost: localhost\r\nContent-Length: 86\r\n\r\n{{\"type\":\"permission_response\",\"request_id\":\"{}\",\"decision\":\"approved\"}}",
                thread.id, request_id
            ),
            &store,
        );
        assert_eq!(response.status, 201);
        assert!(response.body.contains("\"kind\":\"permission_response\""));
        assert!(response.body.contains("\"decision\":\"approved\""));
        assert!(response.body.contains(&request_id));

        let response_stream = response_for_request(
            &format!(
                "GET /v1/threads/{}/events/stream?since_seq=2 HTTP/1.1\r\nHost: localhost\r\n\r\n",
                thread.id
            ),
            &store,
        );
        assert!(response_stream
            .body
            .contains("event: permission_response\n"));
    }

    #[test]
    fn event_endpoint_appends_user_input_events() {
        let store = temp_store("user-input-event");
        let thread = store
            .create_thread(
                "Runtime parity".to_string(),
                ".".to_string(),
                "deepseek-coder".to_string(),
                "agent".to_string(),
            )
            .unwrap();
        let request_body = r#"{"type":"user_input_request","questions":[{"header":"Mode","id":"mode","question":"Which mode?","options":[{"label":"Plan","description":"Plan first."},{"label":"Apply","description":"Implement directly."}]}]}"#;
        let create = response_for_request(
            &format!(
                "POST /v1/threads/{}/events HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n{}",
                thread.id,
                request_body.len(),
                request_body
            ),
            &store,
        );
        assert_eq!(create.status, 201);
        assert!(create.body.contains("\"kind\":\"user_input_request\""));
        assert!(create.body.contains("\"question\":\"Which mode?\""));
        let create_root = parse_root_object(&create.body).unwrap();
        let request_id = create_root
            .get("event")
            .and_then(json_as_object)
            .and_then(|event| event.get("id"))
            .and_then(json_as_string)
            .expect("user input request id")
            .to_string();

        let response_body = format!(
            r#"{{"type":"user_input_response","request_id":"{}","answers":{{"mode":"Plan"}}}}"#,
            request_id
        );
        let response = response_for_request(
            &format!(
                "POST /v1/threads/{}/events HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n{}",
                thread.id,
                response_body.len(),
                response_body
            ),
            &store,
        );

        assert_eq!(response.status, 201);
        assert!(response.body.contains("\"kind\":\"user_input_response\""));
        assert!(response.body.contains("\"mode\":\"Plan\""));
    }

    #[test]
    fn event_endpoint_appends_cancel_requests() {
        let store = temp_store("cancel-event");
        let thread = store
            .create_thread(
                "Runtime parity".to_string(),
                ".".to_string(),
                "deepseek-coder".to_string(),
                "agent".to_string(),
            )
            .unwrap();
        let turn = store
            .append_turn(&thread.id, "assistant".to_string(), "running".to_string())
            .unwrap();

        let response = response_for_request(
            &format!(
                "POST /v1/threads/{}/events HTTP/1.1\r\nHost: localhost\r\nContent-Length: 98\r\n\r\n{{\"type\":\"cancel_requested\",\"turn_id\":\"{}\",\"reason\":\"user requested cancellation\"}}",
                thread.id, turn.id
            ),
            &store,
        );

        assert_eq!(response.status, 201);
        assert!(response.body.contains("\"kind\":\"cancel_requested\""));
        assert!(response
            .body
            .contains("\"reason\":\"user requested cancellation\""));
        let stream = response_for_request(
            &format!(
                "GET /v1/threads/{}/events/stream?since_seq=2 HTTP/1.1\r\nHost: localhost\r\n\r\n",
                thread.id
            ),
            &store,
        );
        assert!(stream.body.contains("event: cancel_requested\n"));
    }

    #[test]
    fn event_endpoint_rejects_unknown_event_kind() {
        let store = temp_store("bad-event");
        let thread = store
            .create_thread(
                "Runtime parity".to_string(),
                ".".to_string(),
                "deepseek-coder".to_string(),
                "agent".to_string(),
            )
            .unwrap();

        let response = response_for_request(
            &format!(
                "POST /v1/threads/{}/events HTTP/1.1\r\nHost: localhost\r\nContent-Length: 22\r\n\r\n{{\"type\":\"custom_event\"}}",
                thread.id
            ),
            &store,
        );

        assert_eq!(response.status, 400);
        assert!(response.body.contains("only permission_request"));
        assert!(response.body.contains("cancel_requested"));
    }

    #[test]
    fn missing_thread_returns_json_404() {
        let store = temp_store("missing-thread");
        let response = response_for_request(
            "GET /v1/threads/thread-missing HTTP/1.1\r\nHost: localhost\r\n\r\n",
            &store,
        );
        assert_eq!(response.status, 404);
        assert!(response.body.contains("not_found"));
    }

    #[test]
    fn missing_thread_events_return_json_404() {
        let store = temp_store("missing-thread-events");
        let response = response_for_request(
            "GET /v1/threads/thread-missing/events HTTP/1.1\r\nHost: localhost\r\n\r\n",
            &store,
        );
        assert_eq!(response.status, 404);
        assert!(response.body.contains("not_found"));

        let stream = response_for_request(
            "GET /v1/threads/thread-missing/events/stream HTTP/1.1\r\nHost: localhost\r\n\r\n",
            &store,
        );
        assert_eq!(stream.status, 404);
        assert!(stream.body.contains("not_found"));
    }

    #[test]
    fn usage_endpoints_return_global_and_thread_usage() {
        let store = temp_store("usage");
        let thread = store
            .create_thread(
                "Runtime parity".to_string(),
                ".".to_string(),
                "deepseek-coder".to_string(),
                "agent".to_string(),
            )
            .unwrap();
        let turn = store
            .append_turn(&thread.id, "assistant".to_string(), "done".to_string())
            .unwrap();
        store
            .append_usage(
                &thread.id,
                Some(&turn.id),
                "deepseek-coder".to_string(),
                "exec".to_string(),
                12,
                3,
            )
            .unwrap();

        let thread_usage = response_for_request(
            &format!(
                "GET /v1/threads/{}/usage HTTP/1.1\r\nHost: localhost\r\n\r\n",
                thread.id
            ),
            &store,
        );
        assert_eq!(thread_usage.status, 200);
        assert!(thread_usage.body.contains("deepseek.runtime.usage.v1"));
        assert!(thread_usage.body.contains("\"prompt_tokens\":12"));
        assert!(thread_usage.body.contains("\"completion_tokens\":3"));
        assert!(thread_usage
            .body
            .contains("\"prompt_cache_miss_tokens\":12"));

        let global_usage = response_for_request(
            &format!(
                "GET /v1/usage?thread_id={} HTTP/1.1\r\nHost: localhost\r\n\r\n",
                thread.id
            ),
            &store,
        );
        assert_eq!(global_usage.status, 200);
        assert!(global_usage.body.contains(&thread.id));
        assert!(global_usage.body.contains("\"total_tokens\":15"));
    }

    #[test]
    fn usage_summary_reports_accounting_and_context_policy() {
        let store = temp_store("usage-summary");
        let thread = store
            .create_thread(
                "Runtime parity".to_string(),
                ".".to_string(),
                "deepseek-v4-flash".to_string(),
                "agent".to_string(),
            )
            .unwrap();
        let first_turn = store
            .append_turn(&thread.id, "assistant".to_string(), "done".to_string())
            .unwrap();
        store
            .append_usage_with_cache(
                &thread.id,
                Some(&first_turn.id),
                "deepseek-v4-flash".to_string(),
                "exec".to_string(),
                12,
                3,
                7,
                5,
            )
            .unwrap();
        let second_turn = store
            .append_turn(&thread.id, "assistant".to_string(), "large".to_string())
            .unwrap();
        store
            .append_usage_with_cache(
                &thread.id,
                Some(&second_turn.id),
                "deepseek-v4-flash".to_string(),
                "exec".to_string(),
                850_000,
                100,
                250_000,
                600_000,
            )
            .unwrap();

        let summary = response_for_request(
            &format!(
                "GET /v1/usage/summary?thread_id={} HTTP/1.1\r\nHost: localhost\r\n\r\n",
                thread.id
            ),
            &store,
        );
        assert_eq!(summary.status, 200);
        assert!(summary.body.contains("deepseek.runtime.usage_summary.v1"));
        assert!(summary.body.contains("\"record_count\":2"));
        assert!(summary.body.contains("\"total_tokens\":850115"));
        assert!(summary.body.contains("\"latest_total_tokens\":850100"));
        assert!(summary.body.contains("\"prompt_cache_hit_tokens\":250007"));
        assert!(summary.body.contains("\"prompt_cache_miss_tokens\":600005"));
        assert!(summary.body.contains("\"unpriced_record_count\":0"));
        assert!(summary.body.contains("\"context_window_tokens\":1000000"));
        assert!(summary
            .body
            .contains("\"context_strategy\":\"prepare_compaction\""));
        assert!(summary.body.contains("\"compaction_recommended\":true"));
        assert!(summary.body.contains(&format!(
            "\"compaction_endpoint\":\"/v1/threads/{}/compact\"",
            thread.id
        )));
    }

    #[test]
    fn serve_http_listener_handles_one_health_request() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let store = temp_store("listener");
        let handle = thread::spawn(move || serve_http_listener(listener, true, &store).unwrap());

        let mut stream = TcpStream::connect(addr).unwrap();
        stream
            .write_all(b"GET /health HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        handle.join().unwrap();

        assert!(response.starts_with("HTTP/1.1 200 OK"));
        assert!(response.contains("\"status\":\"ok\""));
    }

    #[test]
    fn serve_http_listener_handles_waiting_sse_and_writer_concurrently() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let store = temp_store("listener-concurrent");
        let thread_record = store
            .create_thread(
                "Runtime parity".to_string(),
                ".".to_string(),
                "deepseek-coder".to_string(),
                "agent".to_string(),
            )
            .unwrap();
        let server_store = store.clone();
        let server = thread::spawn(move || {
            serve_http_listener_with_limit(listener, Some(2), &server_store).unwrap()
        });

        let mut sse_stream = TcpStream::connect(addr).unwrap();
        sse_stream
            .write_all(
                format!(
                    "GET /v1/threads/{}/events/stream?since_seq=1&wait_ms=1000&poll_ms=10 HTTP/1.1\r\nHost: localhost\r\n\r\n",
                    thread_record.id
                )
                .as_bytes(),
            )
            .unwrap();
        thread::sleep(Duration::from_millis(50));

        let body = "{\"role\":\"user\",\"content\":\"hello from concurrent writer\"}";
        let mut writer_stream = TcpStream::connect(addr).unwrap();
        writer_stream
            .write_all(
                format!(
                    "POST /v1/threads/{}/turns HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n{}",
                    thread_record.id,
                    body.len(),
                    body
                )
                .as_bytes(),
            )
            .unwrap();
        let mut writer_response = String::new();
        writer_stream.read_to_string(&mut writer_response).unwrap();
        assert!(writer_response.starts_with("HTTP/1.1 201 Created"));

        let mut sse_response = String::new();
        sse_stream.read_to_string(&mut sse_response).unwrap();
        server.join().unwrap();

        assert!(sse_response.starts_with("HTTP/1.1 200 OK"));
        assert!(sse_response.contains("event: turn_recorded\n"));
        assert!(sse_response.contains("id: 2\n"));
    }

    #[test]
    fn serve_http_listener_follow_streams_multiple_events_without_reconnect() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let store = temp_store("listener-follow");
        let thread_record = store
            .create_thread(
                "Runtime parity".to_string(),
                ".".to_string(),
                "deepseek-coder".to_string(),
                "agent".to_string(),
            )
            .unwrap();
        let server_store = store.clone();
        let server = thread::spawn(move || {
            serve_http_listener_with_limit(listener, Some(3), &server_store).unwrap()
        });

        let mut sse_stream = TcpStream::connect(addr).unwrap();
        sse_stream
            .write_all(
                format!(
                    "GET /v1/threads/{}/events/stream?since_seq=1&follow=1&max_events=2&poll_ms=10 HTTP/1.1\r\nHost: localhost\r\n\r\n",
                    thread_record.id
                )
                .as_bytes(),
            )
            .unwrap();
        thread::sleep(Duration::from_millis(50));

        for content in ["first streamed turn", "second streamed turn"] {
            let body = format!("{{\"role\":\"user\",\"content\":\"{content}\"}}");
            let mut writer_stream = TcpStream::connect(addr).unwrap();
            writer_stream
                .write_all(
                    format!(
                        "POST /v1/threads/{}/turns HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n{}",
                        thread_record.id,
                        body.len(),
                        body
                    )
                    .as_bytes(),
                )
                .unwrap();
            let mut writer_response = String::new();
            writer_stream.read_to_string(&mut writer_response).unwrap();
            assert!(writer_response.starts_with("HTTP/1.1 201 Created"));
        }

        let mut sse_response = String::new();
        sse_stream.read_to_string(&mut sse_response).unwrap();
        server.join().unwrap();

        assert!(sse_response.starts_with("HTTP/1.1 200 OK"));
        assert!(sse_response.contains("Content-Type: text/event-stream; charset=utf-8"));
        assert!(sse_response.contains("id: 2\n"));
        assert!(sse_response.contains("id: 3\n"));
        assert_eq!(sse_response.matches("event: turn_recorded\n").count(), 2);
        assert!(!sse_response.contains("Content-Length:"));
    }
}
