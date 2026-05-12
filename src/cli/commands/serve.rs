use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crate::cli::app::{ServeAction, ServeArgs, ServeHttpArgs};
use crate::config::load::load_or_default;
use crate::core::runtime::{
    automation_to_json, event_to_json, item_to_json, json_array, json_object, json_string_field,
    parse_json_object_body, session_to_json, task_to_json, thread_compaction_to_json,
    thread_to_json, turn_to_json, usage_to_json, validate_record_id, RuntimeEvent, RuntimeStore,
};
use crate::error::{app_error, AppResult};
use crate::util::json::{
    json_as_array, json_as_object, json_as_string, json_as_u64, json_value_to_string, JsonValue,
};

pub fn run(args: ServeArgs) -> AppResult<()> {
    match args.action {
        ServeAction::Http(http) => run_http(http),
        ServeAction::Mcp => Err(app_error(
            "serve --mcp is not implemented yet; use `deepseek mcp ...` for MCP client operations",
        )),
        ServeAction::Acp => Err(app_error(
            "serve --acp is not implemented yet; the HTTP runtime contract is available with `serve --http`",
        )),
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
    json_object([
        ("language", JsonValue::String(report.language.clone())),
        ("engine", JsonValue::String(report.engine.clone())),
        ("lsp_server", JsonValue::String(report.lsp_server.clone())),
        ("lsp_available", JsonValue::Bool(report.lsp_available)),
        ("command", JsonValue::String(report.command.clone())),
        ("cwd", JsonValue::String(report.cwd.clone())),
        (
            "checked_files",
            json_array(
                report
                    .checked_files
                    .iter()
                    .cloned()
                    .map(JsonValue::String)
                    .collect(),
            ),
        ),
        (
            "status",
            JsonValue::String(report.status.as_str().to_string()),
        ),
        ("stdout", JsonValue::String(report.stdout.clone())),
        ("stderr", JsonValue::String(report.stderr.clone())),
        (
            "note",
            report
                .note
                .clone()
                .map(JsonValue::String)
                .unwrap_or(JsonValue::Null),
        ),
    ])
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
        "cancel_requested" => store.append_cancel_request(
            thread_id,
            turn_id.as_deref(),
            json_optional_string_field(payload, "task_id")?.as_deref(),
            json_string_field(payload, "reason", "user requested cancellation")?,
        )?,
        _ => {
            return Ok(bad_request(
                "only permission_request, permission_response, or cancel_requested events can be appended through this endpoint",
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

const SSE_MAX_WAIT_MS: u64 = 30_000;
const SSE_MIN_POLL_MS: u64 = 10;
const SSE_MAX_POLL_MS: u64 = 1_000;

fn event_stream_thread_id(path: &str) -> Option<&str> {
    let rest = path.strip_prefix("/v1/threads/")?;
    let parts = rest.split('/').collect::<Vec<_>>();
    match parts.as_slice() {
        [thread_id, "events", "stream"] => Some(thread_id),
        _ => None,
    }
}

fn sse_event_frame(event: &RuntimeEvent) -> String {
    let mut frame = String::new();
    frame.push_str("id: ");
    frame.push_str(&event.seq.to_string());
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
    use crate::util::json::{json_as_object, json_as_string, parse_root_object};
    use std::fs;
    use std::io::{Read, Write};
    use std::net::TcpStream;
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
