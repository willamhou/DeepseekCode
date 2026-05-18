use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::rc::Rc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use flate2::read::GzDecoder;

use crate::cli::app::{McpConfigScope, TuiArgs};
use crate::cli::commands::config::{
    diagnostics_config_summary_at, logout_credentials_at, model_config_summary_at,
    network_policy_summary_at, persist_auth_secret_at, profile_config_summary_at,
    provider_config_summary_at, provider_model_completion_values_for_base_url,
    remove_network_rule_at, set_diagnostics_post_edit_at, set_model_at, set_network_default_at,
    set_network_rule_at, set_provider_at, switch_profile_at, DiagnosticsConfigSummary,
    LogoutCredentialSummary, ModelConfigSummary, NetworkPolicySummary, NetworkRuleTarget,
    ProfileConfigSummary, ProviderConfigSummary,
};
use crate::cli::commands::mcp::{
    add_mcp_server_at, init_mcp_config_at, list_remote_prompts_summary,
    list_remote_resource_templates_summary, list_remote_resources_summary,
    list_remote_tools_summary, list_servers_summary, mcp_status_summary, remove_mcp_server_at,
    set_mcp_server_enabled_at, validate_servers_summary, McpServerConfigSpec,
};
use crate::config::load::load_or_default;
use crate::config::types::AppConfig;
use crate::core::context::TaskContext;
use crate::core::hooks::HookEvent;
use crate::core::instructions::init_project_instructions_at;
use crate::core::loop_runtime::{
    preview_system_prompt_for_workspace, AgentApprovalDecision, AgentApprovalRequest,
    AgentApprovalResolver, AgentCancelCheck, AgentLoop, AgentLoopOptions, AgentRunEvents,
    AgentUserInputRequest, AgentUserInputResolver, AgentUserInputResponse, RunResult,
    SharedAgentApprovalResolver, SharedAgentCancelCheck, SharedAgentRunEvents,
    SharedAgentUserInputResolver, SystemPromptPreview, ToolEvent,
};
use crate::core::rollback::{RestorePlan, RollbackStore, SnapshotRecord};
use crate::core::runtime::{
    item_to_json, json_object, parse_automation_record, parse_item_record, parse_runtime_event,
    parse_session_record, parse_task_record, parse_thread_record, parse_turn_record,
    parse_usage_record, session_to_json, thread_to_json, turn_to_json, ItemRecord, RuntimeEvent,
    RuntimeStore, SessionRecord, TaskRecord, ThreadForkRecord, ThreadRecord, TurnRecord,
};
use crate::error::{app_error, AppResult};
use crate::model::deepseek::DeepSeekClient;
use crate::model::protocol::ObservationStatus;
use crate::repl::slash::load_custom_slash_command_from_config;
use crate::skills::loader::load_skill;
use crate::skills::paths::resolve_repo_skills_dir;
use crate::skills::registry::SkillRegistry;
use crate::skills::schema::SkillSpec;
use crate::skills::tilde::expand_tilde;
use crate::tools::exec_shell::{
    run_trusted_background_shell, ExecShellAttachTool, ExecShellCancelTool, ExecShellInteractTool,
    ExecShellListTool, ExecShellResizeTool, ExecShellShowTool, ExecShellSupervisorStatusTool,
    ExecShellTool, ExecShellWaitTool,
};
use crate::tools::recall_archive::RecallArchiveTool;
use crate::tools::review::ReviewTool;
use crate::tools::types::{Tool, ToolInput};
use crate::tools::web::{fetch_url_bytes, fetch_url_text};
use crate::tui::{
    discover_custom_slash_commands_dir, render_once, run_interactive,
    run_interactive_with_refresh_actions_and_live, TuiAction, TuiAnchorCommand, TuiApp,
    TuiApprovalRequest, TuiAutomationRecord, TuiHooksCommand, TuiItem, TuiLiveEvent, TuiLspCommand,
    TuiMcpConfigScope, TuiMcpDetailKind, TuiMemoryCommand, TuiMode, TuiModelCommand,
    TuiNetworkCommand, TuiNoteCommand, TuiProfileCommand, TuiProviderCommand, TuiSession,
    TuiSkillsCommand, TuiTaskRecord, TuiThread, TuiTrustCommand, TuiUsageSummary,
    TuiUserInputRequest,
};
use crate::ui::stream::StreamEvents;
use crate::util::json::{
    json_as_array, json_as_object, json_as_string, json_value_to_string, parse_json_value,
    parse_root_object, JsonValue,
};
use crate::util::sse;
use crate::workspace_trust::{
    add as add_workspace_trust_path, remove as remove_workspace_trust_path, render_trust_file_hint,
    resolve_trust_command_path, set_trust_mode, WorkspaceTrust,
};

pub fn run(args: TuiArgs) -> AppResult<()> {
    if args.entrypoint_smoke {
        return run_entrypoint_smoke(args.smoke_bin.as_deref());
    }

    if args.demo {
        let app = TuiApp::demo();
        if args.once {
            print!("{}", render_once(&app, 120, 36)?);
            return Ok(());
        }
        return run_interactive(app);
    }

    if let Some(runtime_url) = args.runtime_url.as_deref() {
        return run_http_runtime_tui(runtime_url, args.once);
    }

    let config = load_or_default()?;
    let runtime_root = PathBuf::from(&config.workspace.config_dir).join("runtime");
    let runtime_store = RuntimeStore::new(runtime_root);
    let mut app = app_from_store(&runtime_store)?;
    configure_tui_slash_completions(&mut app, &config);
    app.enable_theme_preferences(
        PathBuf::from(&config.workspace.config_dir).join("tui/theme.json"),
    );
    app.enable_reasoning_replay_preferences(
        PathBuf::from(&config.workspace.config_dir).join("tui/reasoning-replay.json"),
    );
    app.enable_composer_stash(
        PathBuf::from(&config.workspace.config_dir).join("tui/composer-stash.json"),
    );

    if args.once {
        print!("{}", render_once(&app, 120, 36)?);
        return Ok(());
    }

    let (live_tx, live_rx) = mpsc::channel();
    let _runtime_watcher = start_runtime_live_watcher(
        runtime_store.clone(),
        live_tx.clone(),
        Duration::from_millis(250),
    );
    let refresh_store = runtime_store.clone();
    let action_store = runtime_store.clone();
    run_interactive_with_refresh_actions_and_live(
        app,
        Duration::from_secs(1),
        move |app| refresh_app_from_store(&refresh_store, app),
        move |app, action| {
            handle_tui_action_with_live(
                &action_store,
                Some(&config),
                app,
                action,
                Some(live_tx.clone()),
            )
        },
        move |app| drain_tui_live_events(&live_rx, app),
    )
}

#[derive(Debug)]
struct EntrypointSmokeOutput {
    backend: String,
    status_code: Option<i32>,
    timed_out: bool,
    stdout: String,
    stderr: String,
}

#[derive(Debug)]
struct EntrypointSmokeReport {
    ok: bool,
    backend: String,
    bin: String,
    status_code: Option<i32>,
    timed_out: bool,
    entered_alternate_screen: bool,
    left_alternate_screen: bool,
    rendered_tui: bool,
    sent_quit: bool,
    stdout_bytes: usize,
    stderr_bytes: usize,
    stdout_preview: String,
    stderr_preview: String,
}

fn run_entrypoint_smoke(smoke_bin: Option<&str>) -> AppResult<()> {
    let bin = match smoke_bin {
        Some(path) => PathBuf::from(path),
        None => std::env::current_exe()
            .map_err(|err| app_error(format!("failed to resolve current executable: {err}")))?,
    };
    let output = run_entrypoint_smoke_process(&bin, Duration::from_secs(6))?;
    let report = entrypoint_smoke_report(&bin, output);
    println!("{}", json_value_to_string(&entrypoint_smoke_json(&report)));
    if report.ok {
        Ok(())
    } else {
        Err(app_error(format!(
            "TUI entrypoint smoke failed for {}",
            bin.display()
        )))
    }
}

#[cfg(unix)]
fn run_entrypoint_smoke_process(bin: &Path, timeout: Duration) -> AppResult<EntrypointSmokeOutput> {
    let linux = vec![
        "-q".to_string(),
        "-e".to_string(),
        "-c".to_string(),
        entrypoint_smoke_shell_command(bin),
        "/dev/null".to_string(),
    ];
    let first = run_script_smoke(bin, &linux, timeout, "script-linux")?;
    if first.status_code == Some(0)
        || (!first.stderr.contains("invalid option") && !first.stderr.contains("illegal option"))
    {
        return Ok(first);
    }

    let bsd = vec![
        "-q".to_string(),
        "/dev/null".to_string(),
        "sh".to_string(),
        "-c".to_string(),
        entrypoint_smoke_shell_command(bin),
    ];
    run_script_smoke(bin, &bsd, timeout, "script-bsd")
}

#[cfg(not(unix))]
fn run_entrypoint_smoke_process(
    _bin: &Path,
    _timeout: Duration,
) -> AppResult<EntrypointSmokeOutput> {
    Err(app_error(
        "TUI entrypoint smoke currently requires a Unix `script` command",
    ))
}

#[cfg(unix)]
fn run_script_smoke(
    bin: &Path,
    args: &[String],
    timeout: Duration,
    backend: &str,
) -> AppResult<EntrypointSmokeOutput> {
    if !bin.exists() {
        return Err(app_error(format!(
            "TUI entrypoint smoke binary does not exist: {}",
            bin.display()
        )));
    }
    let mut child = Command::new("script")
        .args(args)
        .env("TERM", "xterm-256color")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| app_error(format!("failed to start `script` for TUI smoke: {err}")))?;

    if let Some(mut stdin) = child.stdin.take() {
        thread::sleep(Duration::from_millis(800));
        let _ = stdin.write_all(b"q");
        let _ = stdin.flush();
    }

    let started = Instant::now();
    let mut timed_out = false;
    let status = loop {
        if let Some(status) = child
            .try_wait()
            .map_err(|err| app_error(format!("failed to poll TUI smoke child: {err}")))?
        {
            break status;
        }
        if started.elapsed() >= timeout {
            timed_out = true;
            let _ = child.kill();
            break child
                .wait()
                .map_err(|err| app_error(format!("failed to reap timed-out TUI smoke: {err}")))?;
        }
        thread::sleep(Duration::from_millis(20));
    };

    let mut stdout = Vec::new();
    if let Some(mut out) = child.stdout.take() {
        out.read_to_end(&mut stdout)
            .map_err(|err| app_error(format!("failed to read TUI smoke stdout: {err}")))?;
    }
    let mut stderr = Vec::new();
    if let Some(mut err) = child.stderr.take() {
        err.read_to_end(&mut stderr)
            .map_err(|err| app_error(format!("failed to read TUI smoke stderr: {err}")))?;
    }

    Ok(EntrypointSmokeOutput {
        backend: backend.to_string(),
        status_code: status.code(),
        timed_out,
        stdout: String::from_utf8_lossy(&stdout).into_owned(),
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
    })
}

fn entrypoint_smoke_report(bin: &Path, output: EntrypointSmokeOutput) -> EntrypointSmokeReport {
    let entered_alternate_screen = output.stdout.contains("\x1b[?1049h");
    let left_alternate_screen = output.stdout.contains("\x1b[?1049l");
    let rendered_tui = output.stdout.contains("DeepSeekCode") && output.stdout.contains("TUI");
    let ok = output.status_code == Some(0)
        && !output.timed_out
        && entered_alternate_screen
        && left_alternate_screen
        && rendered_tui;
    EntrypointSmokeReport {
        ok,
        backend: output.backend,
        bin: bin.display().to_string(),
        status_code: output.status_code,
        timed_out: output.timed_out,
        entered_alternate_screen,
        left_alternate_screen,
        rendered_tui,
        sent_quit: true,
        stdout_bytes: output.stdout.len(),
        stderr_bytes: output.stderr.len(),
        stdout_preview: clipped_smoke_preview(&output.stdout),
        stderr_preview: clipped_smoke_preview(&output.stderr),
    }
}

fn entrypoint_smoke_json(report: &EntrypointSmokeReport) -> JsonValue {
    json_object([
        (
            "schema",
            JsonValue::String("deepseek.tui.entrypoint_smoke.v1".to_string()),
        ),
        ("ok", JsonValue::Bool(report.ok)),
        ("backend", JsonValue::String(report.backend.clone())),
        ("bin", JsonValue::String(report.bin.clone())),
        (
            "status_code",
            report
                .status_code
                .map(|code| JsonValue::Number(code.to_string()))
                .unwrap_or(JsonValue::Null),
        ),
        ("timed_out", JsonValue::Bool(report.timed_out)),
        (
            "entered_alternate_screen",
            JsonValue::Bool(report.entered_alternate_screen),
        ),
        (
            "left_alternate_screen",
            JsonValue::Bool(report.left_alternate_screen),
        ),
        ("rendered_tui", JsonValue::Bool(report.rendered_tui)),
        ("sent_quit", JsonValue::Bool(report.sent_quit)),
        (
            "stdout_bytes",
            JsonValue::Number(report.stdout_bytes.to_string()),
        ),
        (
            "stderr_bytes",
            JsonValue::Number(report.stderr_bytes.to_string()),
        ),
        (
            "stdout_preview",
            JsonValue::String(report.stdout_preview.clone()),
        ),
        (
            "stderr_preview",
            JsonValue::String(report.stderr_preview.clone()),
        ),
    ])
}

fn clipped_smoke_preview(value: &str) -> String {
    value.chars().take(240).collect()
}

#[cfg(unix)]
fn shell_quote(path: &Path) -> String {
    let value = path.to_string_lossy();
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(unix)]
fn entrypoint_smoke_shell_command(bin: &Path) -> String {
    format!("stty rows 36 cols 120; exec {}", shell_quote(bin))
}

struct RuntimeSnapshot {
    sessions: Vec<TuiSession>,
    threads: Vec<TuiThread>,
    items: Vec<TuiItem>,
    tasks: Vec<TuiTaskRecord>,
    automations: Vec<TuiAutomationRecord>,
    usage_summaries: Vec<TuiUsageSummary>,
    approvals: Vec<TuiApprovalRequest>,
    user_inputs: Vec<TuiUserInputRequest>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RuntimeSnapshotSignature {
    sessions: Vec<(String, String, Option<String>, u64)>,
    threads: Vec<(String, String, String, Option<String>, u64)>,
    item_count: usize,
    tasks: Vec<(String, String, String)>,
    automations: Vec<(String, String, Option<String>, Option<String>)>,
    usage_summaries: Vec<(String, usize, u64, u64)>,
    approvals: Vec<(String, String)>,
    user_inputs: Vec<(String, String)>,
}

struct RuntimeLiveWatcher {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl Drop for RuntimeLiveWatcher {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

#[derive(Debug, Clone)]
struct RuntimeHttpClient {
    authority: String,
    host: String,
    port: u16,
    path_prefix: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RuntimeHttpSubscription {
    thread_id: String,
    since_seq: u64,
}

struct RuntimeHttpLiveWatcher {
    stop: Arc<AtomicBool>,
    handles: Vec<JoinHandle<()>>,
}

impl Drop for RuntimeHttpLiveWatcher {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        for handle in self.handles.drain(..) {
            let _ = handle.join();
        }
    }
}

impl RuntimeHttpClient {
    fn from_url(url: &str) -> AppResult<Self> {
        let trimmed = url.trim().trim_end_matches('/');
        let rest = trimmed
            .strip_prefix("http://")
            .ok_or_else(|| app_error("tui --runtime-url currently supports http:// URLs only"))?;
        let (authority, path_prefix) = match rest.split_once('/') {
            Some((authority, path)) => (authority, format!("/{path}")),
            None => (rest, String::new()),
        };
        if authority.is_empty() {
            return Err(app_error("tui --runtime-url is missing a host"));
        }
        let (host, port) = match authority.rsplit_once(':') {
            Some((host, port)) if !host.is_empty() => {
                let port = port.parse::<u16>().map_err(|_| {
                    app_error(format!("tui --runtime-url has invalid port `{port}`"))
                })?;
                (host.to_string(), port)
            }
            _ => (authority.to_string(), 80),
        };
        Ok(Self {
            authority: authority.to_string(),
            host,
            port,
            path_prefix,
        })
    }

    fn request_target(&self, path: &str) -> String {
        if self.path_prefix.is_empty() {
            path.to_string()
        } else {
            format!("{}{}", self.path_prefix, path)
        }
    }

    fn connect(&self) -> AppResult<TcpStream> {
        TcpStream::connect((self.host.as_str(), self.port)).map_err(|error| {
            app_error(format!(
                "failed to connect to HTTP runtime at {}: {error}",
                self.authority
            ))
        })
    }

    fn get_json(&self, path: &str) -> AppResult<JsonValue> {
        self.request_json("GET", path, None)
    }

    fn post_json(&self, path: &str, body: JsonValue) -> AppResult<JsonValue> {
        self.request_json("POST", path, Some(json_value_to_string(&body)))
    }

    fn request_json(&self, method: &str, path: &str, body: Option<String>) -> AppResult<JsonValue> {
        let mut stream = self.connect()?;
        let target = self.request_target(path);
        let body = body.unwrap_or_default();
        let content_headers = if body.is_empty() {
            String::new()
        } else {
            format!(
                "Content-Type: application/json; charset=utf-8\r\nContent-Length: {}\r\n",
                body.len()
            )
        };
        let request = format!(
            "{method} {target} HTTP/1.1\r\nHost: {}\r\nAccept: application/json\r\n{}Connection: close\r\n\r\n{}",
            self.authority, content_headers, body
        );
        stream.write_all(request.as_bytes())?;
        stream.flush()?;
        let mut raw = String::new();
        stream.read_to_string(&mut raw)?;
        let (status, response_body) = parse_http_response(&raw)?;
        if !(200..300).contains(&status) {
            return Err(app_error(format!(
                "HTTP runtime request {method} {path} failed with {status}: {}",
                response_body.trim()
            )));
        }
        parse_json_value(response_body)
    }

    fn open_sse(&self, path: &str) -> AppResult<BufReader<TcpStream>> {
        let mut stream = self.connect()?;
        let target = self.request_target(path);
        let request = format!(
            "GET {target} HTTP/1.1\r\nHost: {}\r\nAccept: text/event-stream\r\nConnection: close\r\n\r\n",
            self.authority
        );
        stream.write_all(request.as_bytes())?;
        stream.flush()?;
        let mut reader = BufReader::new(stream);
        let mut status_line = String::new();
        reader.read_line(&mut status_line)?;
        let status = parse_status_code(&status_line)?;
        let mut header = String::new();
        loop {
            header.clear();
            let read = reader.read_line(&mut header)?;
            if read == 0 || header == "\r\n" || header == "\n" {
                break;
            }
        }
        if !(200..300).contains(&status) {
            return Err(app_error(format!(
                "HTTP runtime SSE request {path} failed with {status}"
            )));
        }
        Ok(reader)
    }
}

fn run_http_runtime_tui(runtime_url: &str, once: bool) -> AppResult<()> {
    let client = RuntimeHttpClient::from_url(runtime_url)?;
    let snapshot = runtime_http_snapshot(&client)?;
    let subscriptions = runtime_http_subscriptions(&snapshot);
    let app = app_from_snapshot(snapshot);

    if once {
        print!("{}", render_once(&app, 120, 36)?);
        return Ok(());
    }

    let (live_tx, live_rx) = mpsc::channel();
    let _runtime_watcher =
        start_runtime_http_live_watcher(client.clone(), subscriptions, live_tx.clone(), 250);
    let refresh_client = client.clone();
    let action_client = client.clone();
    run_interactive_with_refresh_actions_and_live(
        app,
        Duration::from_secs(1),
        move |app| refresh_app_from_http(&refresh_client, app),
        move |app, action| handle_tui_http_action(&action_client, app, action),
        move |app| drain_tui_live_events(&live_rx, app),
    )
}

fn app_from_snapshot(snapshot: RuntimeSnapshot) -> TuiApp {
    TuiApp::with_runtime_usage_tasks_automations_approvals_and_user_inputs(
        snapshot.sessions,
        snapshot.threads,
        snapshot.items,
        snapshot.tasks,
        snapshot.automations,
        snapshot.usage_summaries,
        snapshot.approvals,
        snapshot.user_inputs,
    )
}

fn refresh_app_from_http(client: &RuntimeHttpClient, app: &mut TuiApp) -> AppResult<()> {
    app.apply_live_event(snapshot_live_event(runtime_http_snapshot(client)?));
    Ok(())
}

fn runtime_http_subscriptions(snapshot: &RuntimeSnapshot) -> Vec<RuntimeHttpSubscription> {
    snapshot
        .threads
        .iter()
        .map(|thread| RuntimeHttpSubscription {
            thread_id: thread.id.clone(),
            since_seq: thread.event_seq,
        })
        .collect()
}

fn start_runtime_http_live_watcher(
    client: RuntimeHttpClient,
    subscriptions: Vec<RuntimeHttpSubscription>,
    tx: Sender<TuiLiveEvent>,
    follow_max_ms: u64,
) -> RuntimeHttpLiveWatcher {
    let stop = Arc::new(AtomicBool::new(false));
    let worker_stop = stop.clone();
    let handle = thread::spawn(move || {
        follow_runtime_global_events(client, subscriptions, tx, worker_stop, follow_max_ms)
    });
    RuntimeHttpLiveWatcher {
        stop,
        handles: vec![handle],
    }
}

fn follow_runtime_global_events(
    client: RuntimeHttpClient,
    subscriptions: Vec<RuntimeHttpSubscription>,
    tx: Sender<TuiLiveEvent>,
    stop: Arc<AtomicBool>,
    follow_max_ms: u64,
) {
    let mut cursor = subscriptions
        .into_iter()
        .map(|subscription| (subscription.thread_id, subscription.since_seq))
        .collect::<BTreeMap<_, _>>();
    while !stop.load(Ordering::Relaxed) {
        let path = runtime_global_sse_path(&cursor, follow_max_ms);
        match client.open_sse(&path) {
            Ok(mut reader) => loop {
                if stop.load(Ordering::Relaxed) {
                    break;
                }
                match sse::read_frame(&mut reader) {
                    Ok(Some(frame)) => {
                        if let Ok(event) = runtime_event_from_sse_frame(&frame) {
                            cursor.insert(event.thread_id.clone(), event.seq);
                            if let Some(status) = rlm_live_status_from_runtime_event(&event) {
                                if tx.send(TuiLiveEvent::Status(status)).is_err() {
                                    return;
                                }
                            }
                            match runtime_http_snapshot(&client) {
                                Ok(snapshot) => {
                                    if tx.send(snapshot_live_event(snapshot)).is_err() {
                                        return;
                                    }
                                }
                                Err(error) => {
                                    if tx
                                        .send(TuiLiveEvent::Status(format!(
                                            "runtime HTTP refresh failed: {error}"
                                        )))
                                        .is_err()
                                    {
                                        return;
                                    }
                                }
                            }
                        }
                    }
                    Ok(None) => break,
                    Err(error) => {
                        if tx
                            .send(TuiLiveEvent::Status(format!(
                                "runtime SSE read failed: {error}"
                            )))
                            .is_err()
                        {
                            return;
                        }
                        break;
                    }
                }
            },
            Err(error) => {
                if tx
                    .send(TuiLiveEvent::Status(format!(
                        "runtime SSE connect failed: {error}"
                    )))
                    .is_err()
                {
                    return;
                }
                thread::sleep(Duration::from_millis(follow_max_ms.min(1_000)));
            }
        }
    }
}

fn runtime_global_sse_path(cursor: &BTreeMap<String, u64>, follow_max_ms: u64) -> String {
    let mut path =
        format!("/v1/events/stream?follow=1&poll_ms=100&max_events=1&max_ms={follow_max_ms}");
    if !cursor.is_empty() {
        let since = cursor
            .iter()
            .map(|(thread_id, seq)| format!("{thread_id}:{seq}"))
            .collect::<Vec<_>>()
            .join(",");
        path.push_str("&since=");
        path.push_str(&since);
    }
    path
}

fn runtime_event_from_sse_frame(frame: &sse::SseFrame) -> AppResult<RuntimeEvent> {
    let value = parse_json_value(&frame.data)?;
    let root = json_object_value(value, "runtime event SSE frame")?;
    parse_runtime_event(&root)
}

fn rlm_live_status_from_runtime_event(event: &RuntimeEvent) -> Option<String> {
    if event.kind != "rlm_live_event" {
        return None;
    }
    let payload = json_as_object(&event.payload)?;
    let session_id = payload.get("session_id").and_then(json_as_string)?;
    let rlm_event = payload.get("event").and_then(json_as_object)?;
    let kind = rlm_event.get("kind").and_then(json_as_string)?;
    let task_id = rlm_event
        .get("task_id")
        .and_then(json_as_string)
        .unwrap_or("-");
    Some(format!("rlm {session_id}: {kind} task={task_id}"))
}

fn runtime_http_snapshot(client: &RuntimeHttpClient) -> AppResult<RuntimeSnapshot> {
    let sessions_root = json_object_value(client.get_json("/v1/sessions?limit=50")?, "sessions")?;
    let session_records = parse_record_array(&sessions_root, "sessions", parse_session_record)?;
    let mut threads = Vec::new();
    let mut items = Vec::new();
    let mut tasks = Vec::new();
    let mut automations = Vec::new();
    let mut usage_summaries = Vec::new();
    let mut approvals = Vec::new();
    let mut user_inputs = Vec::new();

    for session in &session_records {
        let session_root = json_object_value(
            client.get_json(&format!("/v1/sessions/{}", session.id))?,
            "session",
        )?;
        let session_threads = parse_record_array(&session_root, "threads", parse_thread_record)?;
        for thread in session_threads {
            let thread_id = thread.id.clone();
            let thread_root = json_object_value(
                client.get_json(&format!("/v1/threads/{thread_id}"))?,
                "thread",
            )?;
            let detail_thread = parse_record_object(&thread_root, "thread", parse_thread_record)?;
            items.extend(
                parse_record_array(&thread_root, "items", parse_item_record)?
                    .into_iter()
                    .map(TuiItem::from),
            );

            let task_root = json_object_value(
                client.get_json(&format!("/v1/threads/{thread_id}/tasks?limit=20"))?,
                "tasks",
            )?;
            tasks.extend(
                parse_record_array(&task_root, "tasks", parse_task_record)?
                    .into_iter()
                    .map(TuiTaskRecord::from),
            );

            let automation_root = json_object_value(
                client.get_json(&format!("/v1/threads/{thread_id}/automations?limit=20"))?,
                "automations",
            )?;
            automations.extend(
                parse_record_array(&automation_root, "automations", parse_automation_record)?
                    .into_iter()
                    .map(TuiAutomationRecord::from),
            );

            let usage_root = json_object_value(
                client.get_json(&format!("/v1/threads/{thread_id}/usage?limit=200"))?,
                "usage",
            )?;
            let usage = parse_record_array(&usage_root, "usage", parse_usage_record)?;
            if !usage.is_empty() {
                usage_summaries.push(TuiUsageSummary::from_usage_records(&thread_id, &usage));
            }

            let events_root = json_object_value(
                client.get_json(&format!("/v1/threads/{thread_id}/events?since_seq=0"))?,
                "events",
            )?;
            let events = parse_record_array(&events_root, "events", parse_runtime_event)?;
            let resolved_approval_ids = events
                .iter()
                .filter_map(TuiApprovalRequest::response_request_id)
                .collect::<BTreeSet<_>>();
            let resolved_user_input_ids = events
                .iter()
                .filter_map(TuiUserInputRequest::response_request_id)
                .collect::<BTreeSet<_>>();
            approvals.extend(events.iter().filter_map(|event| {
                let approval = TuiApprovalRequest::from_runtime_event(event)?;
                if resolved_approval_ids.contains(&approval.id) {
                    None
                } else {
                    Some(approval)
                }
            }));
            user_inputs.extend(events.iter().filter_map(|event| {
                let request = TuiUserInputRequest::from_runtime_event(event)?;
                if resolved_user_input_ids.contains(&request.id) {
                    None
                } else {
                    Some(request)
                }
            }));

            threads.push(TuiThread::from(detail_thread));
        }
    }

    Ok(RuntimeSnapshot {
        sessions: session_records.into_iter().map(TuiSession::from).collect(),
        threads,
        items,
        tasks,
        automations,
        usage_summaries,
        approvals,
        user_inputs,
    })
}

fn handle_tui_http_action(
    client: &RuntimeHttpClient,
    app: &mut TuiApp,
    action: TuiAction,
) -> AppResult<()> {
    match action {
        TuiAction::SubmitUserMessage { thread_id, content } => {
            client.post_json(
                &format!("/v1/threads/{thread_id}/turns"),
                json_object([
                    ("role", JsonValue::String("user".to_string())),
                    ("content", JsonValue::String(content)),
                ]),
            )?;
            app.set_status(format!(
                "submitted user message to remote runtime {thread_id}"
            ));
        }
        TuiAction::RunCustomSlashCommand { .. } => {
            app.set_status("custom slash commands require local file-backed TUI".to_string());
        }
        TuiAction::RenameSession { .. } => {
            app.set_status("session rename requires local file-backed TUI".to_string());
        }
        TuiAction::InitProjectInstructions { .. } => {
            app.set_status("project instructions init requires local file-backed TUI".to_string());
        }
        TuiAction::Network { .. } => {
            app.set_status("network commands require local file-backed TUI".to_string());
        }
        TuiAction::Lsp { .. } => {
            app.set_status("lsp commands require local file-backed TUI".to_string());
        }
        TuiAction::ShowSystemPrompt { .. } => {
            app.set_status("system prompt preview requires local file-backed TUI".to_string());
        }
        TuiAction::Model { .. } => {
            app.set_status("model commands require local file-backed TUI".to_string());
        }
        TuiAction::Provider { .. } => {
            app.set_status("provider commands require local file-backed TUI".to_string());
        }
        TuiAction::Profile { .. } => {
            app.set_status("profile commands require local file-backed TUI".to_string());
        }
        TuiAction::Trust { .. } => {
            app.set_status("trust commands require local file-backed TUI".to_string());
        }
        TuiAction::Logout { .. } => {
            app.set_status("logout requires local file-backed TUI".to_string());
        }
        TuiAction::AuthCredential { .. } => {
            app.set_status("auth credential wizard requires local file-backed TUI".to_string());
        }
        TuiAction::Skills { .. } => {
            app.set_status("skills commands require local file-backed TUI".to_string());
        }
        TuiAction::RespondApproval {
            thread_id,
            turn_id,
            request_id,
            decision,
            scope,
        } => {
            let mut body = BTreeMap::new();
            body.insert(
                "type".to_string(),
                JsonValue::String("permission_response".to_string()),
            );
            body.insert(
                "request_id".to_string(),
                JsonValue::String(request_id.clone()),
            );
            body.insert("decision".to_string(), JsonValue::String(decision.clone()));
            if let Some(scope) = scope {
                body.insert("scope".to_string(), JsonValue::String(scope));
            }
            if let Some(turn_id) = turn_id {
                body.insert("turn_id".to_string(), JsonValue::String(turn_id));
            }
            client.post_json(
                &format!("/v1/threads/{thread_id}/events"),
                JsonValue::Object(body),
            )?;
            app.set_status(format!(
                "recorded remote approval response: {request_id} {decision}"
            ));
        }
        TuiAction::RespondUserInput {
            thread_id,
            turn_id,
            request_id,
            answers,
        } => {
            let mut body = BTreeMap::new();
            body.insert(
                "type".to_string(),
                JsonValue::String("user_input_response".to_string()),
            );
            body.insert(
                "request_id".to_string(),
                JsonValue::String(request_id.clone()),
            );
            body.insert(
                "answers".to_string(),
                JsonValue::Object(
                    answers
                        .into_iter()
                        .map(|(key, value)| (key, JsonValue::String(value)))
                        .collect(),
                ),
            );
            if let Some(turn_id) = turn_id {
                body.insert("turn_id".to_string(), JsonValue::String(turn_id));
            }
            client.post_json(
                &format!("/v1/threads/{thread_id}/events"),
                JsonValue::Object(body),
            )?;
            app.set_status(format!("recorded remote user input response: {request_id}"));
        }
        TuiAction::CancelRun { thread_id, turn_id } => {
            let mut body = BTreeMap::new();
            body.insert(
                "type".to_string(),
                JsonValue::String("cancel_requested".to_string()),
            );
            body.insert(
                "reason".to_string(),
                JsonValue::String("user requested cancellation from TUI".to_string()),
            );
            if let Some(turn_id) = turn_id {
                body.insert("turn_id".to_string(), JsonValue::String(turn_id));
            }
            client.post_json(
                &format!("/v1/threads/{thread_id}/events"),
                JsonValue::Object(body),
            )?;
            app.set_status(format!("remote cancel event recorded for {thread_id}"));
        }
        TuiAction::CreateTask { thread_id, summary } => {
            client.post_json(
                &format!("/v1/threads/{thread_id}/tasks"),
                json_object([
                    ("kind", JsonValue::String("agent".to_string())),
                    ("status", JsonValue::String("pending".to_string())),
                    ("summary", JsonValue::String(summary)),
                ]),
            )?;
            app.set_status(format!("created remote pending task for {thread_id}"));
        }
        TuiAction::CreateSubagentTask {
            thread_id,
            task,
            max_depth,
        } => {
            client.post_json(
                &format!("/v1/threads/{thread_id}/tasks"),
                json_object([
                    ("kind", JsonValue::String("subagent".to_string())),
                    ("status", JsonValue::String("pending".to_string())),
                    (
                        "summary",
                        JsonValue::String(tui_subagent_task_summary(max_depth, &task)),
                    ),
                ]),
            )?;
            app.set_status(format!(
                "created remote subagent task for {thread_id} (depth={max_depth})"
            ));
        }
        TuiAction::PauseTask { task_id } => {
            client.post_json(
                &format!("/v1/tasks/{task_id}/pause"),
                JsonValue::Object(BTreeMap::new()),
            )?;
            app.set_status(format!("paused remote task {task_id}"));
        }
        TuiAction::ResumeTask { task_id } => {
            client.post_json(
                &format!("/v1/tasks/{task_id}/resume"),
                JsonValue::Object(BTreeMap::new()),
            )?;
            app.set_status(format!("resumed remote task {task_id}"));
        }
        TuiAction::CancelTask { task_id } => {
            client.post_json(
                &format!("/v1/tasks/{task_id}/cancel"),
                json_object([(
                    "reason",
                    JsonValue::String("cancelled from TUI task panel".to_string()),
                )]),
            )?;
            app.set_status(format!("cancelled remote task {task_id}"));
        }
        TuiAction::RunDiagnostics { changed, paths } => {
            run_remote_tui_diagnostics(client, app, changed, paths);
        }
        TuiAction::RunShell { .. }
        | TuiAction::RunApprovedShell { .. }
        | TuiAction::ListShell
        | TuiAction::ShowShell { .. }
        | TuiAction::AttachShell { .. }
        | TuiAction::ShellSupervisorStatus
        | TuiAction::SendShellStdin { .. }
        | TuiAction::WaitShell { .. }
        | TuiAction::ResizeShell { .. }
        | TuiAction::CancelShell { .. } => {
            app.set_status("shell commands require local file-backed TUI".to_string());
        }
        TuiAction::AppendMemory { .. } | TuiAction::Memory { .. } => {
            app.set_status("memory commands require local file-backed TUI".to_string());
        }
        TuiAction::UndoConversation { .. }
        | TuiAction::RetryUserMessage { .. }
        | TuiAction::SubmitEditedUserMessage { .. }
        | TuiAction::RecallArchive { .. }
        | TuiAction::ReviewTarget { .. } => {
            app.set_status(
                "undo/retry/recall/review commands require local file-backed TUI".to_string(),
            );
        }
        TuiAction::Note { .. } => {
            app.set_status("note commands require local file-backed TUI".to_string());
        }
        TuiAction::Anchor { .. } => {
            app.set_status("anchor commands require local file-backed TUI".to_string());
        }
        TuiAction::ShareSession { .. } => {
            app.set_status("share commands require local file-backed TUI".to_string());
        }
        TuiAction::ExportThread { .. } => {
            app.set_status("export commands require local file-backed TUI".to_string());
        }
        TuiAction::SaveSession { .. } => {
            app.set_status("save commands require local file-backed TUI".to_string());
        }
        TuiAction::PruneSessions { .. } => {
            app.set_status("session prune requires local file-backed TUI".to_string());
        }
        TuiAction::LoadSession { .. } => {
            app.set_status("load commands require local file-backed TUI".to_string());
        }
        TuiAction::ClearConversation { .. } => {
            app.set_status("clear conversation requires local file-backed TUI".to_string());
        }
        TuiAction::ShowDiff { .. } => {
            app.set_status("diff commands require local file-backed TUI".to_string());
        }
        TuiAction::Hooks { .. } => {
            app.set_status("hooks commands require local file-backed TUI".to_string());
        }
        TuiAction::McpManager
        | TuiAction::McpList
        | TuiAction::McpInit { .. }
        | TuiAction::McpAddStdio { .. }
        | TuiAction::McpAddRemote { .. }
        | TuiAction::McpRemove { .. }
        | TuiAction::McpSetEnabled { .. }
        | TuiAction::McpDetails { .. }
        | TuiAction::McpManagerDetails { .. }
        | TuiAction::McpValidate => {
            app.set_status("mcp commands require local file-backed TUI".to_string());
        }
        TuiAction::CreateRollbackSnapshot { .. }
        | TuiAction::ListRollbackSnapshots { .. }
        | TuiAction::ShowRollbackSnapshot { .. }
        | TuiAction::ShowRollbackHunk { .. }
        | TuiAction::RestoreRollbackHunk { .. }
        | TuiAction::RevertTurn { .. } => {
            app.set_status("rollback commands require local file-backed TUI".to_string());
        }
        TuiAction::TriggerAutomation {
            automation_id,
            prompt_override,
        } => {
            let body = match prompt_override {
                Some(prompt) => json_object([("prompt", JsonValue::String(prompt))]),
                None => JsonValue::Object(BTreeMap::new()),
            };
            client.post_json(&format!("/v1/automations/{automation_id}/trigger"), body)?;
            app.set_status(format!("triggered remote automation {automation_id}"));
        }
        TuiAction::CompactThread {
            thread_id,
            keep_tail_turns,
        } => {
            client.post_json(
                &format!("/v1/threads/{thread_id}/compact"),
                json_object([(
                    "keep_tail_turns",
                    JsonValue::Number(keep_tail_turns.to_string()),
                )]),
            )?;
            app.set_status(format!("compacted remote thread {thread_id}"));
        }
    }
    Ok(())
}

fn parse_http_response(raw: &str) -> AppResult<(u16, &str)> {
    let Some((head, body)) = raw.split_once("\r\n\r\n") else {
        return Err(app_error("malformed HTTP runtime response"));
    };
    let status_line = head.lines().next().unwrap_or("");
    Ok((parse_status_code(status_line)?, body))
}

fn parse_status_code(status_line: &str) -> AppResult<u16> {
    status_line
        .split_whitespace()
        .nth(1)
        .and_then(|value| value.parse::<u16>().ok())
        .ok_or_else(|| app_error(format!("malformed HTTP status line `{status_line}`")))
}

fn json_object_value(value: JsonValue, context: &str) -> AppResult<BTreeMap<String, JsonValue>> {
    let JsonValue::Object(root) = value else {
        return Err(app_error(format!(
            "{context} response root must be an object"
        )));
    };
    Ok(root)
}

fn parse_record_object<T>(
    root: &BTreeMap<String, JsonValue>,
    key: &str,
    parse: fn(&BTreeMap<String, JsonValue>) -> AppResult<T>,
) -> AppResult<T> {
    let value = root
        .get(key)
        .ok_or_else(|| app_error(format!("HTTP runtime response missing `{key}`")))?;
    let object = json_as_object(value)
        .ok_or_else(|| app_error(format!("HTTP runtime response `{key}` must be an object")))?;
    parse(object)
}

fn parse_record_array<T>(
    root: &BTreeMap<String, JsonValue>,
    key: &str,
    parse: fn(&BTreeMap<String, JsonValue>) -> AppResult<T>,
) -> AppResult<Vec<T>> {
    let value = root
        .get(key)
        .ok_or_else(|| app_error(format!("HTTP runtime response missing `{key}`")))?;
    let array = json_as_array(value)
        .ok_or_else(|| app_error(format!("HTTP runtime response `{key}` must be an array")))?;
    array
        .iter()
        .map(|item| {
            let object = json_as_object(item).ok_or_else(|| {
                app_error(format!(
                    "HTTP runtime response `{key}` array item must be an object"
                ))
            })?;
            parse(object)
        })
        .collect()
}

fn runtime_snapshot(store: &RuntimeStore) -> AppResult<RuntimeSnapshot> {
    let session_records = store.list_sessions(50)?;
    let mut threads = Vec::new();
    let mut items = Vec::new();
    let mut tasks = Vec::new();
    let mut automations = Vec::new();
    let mut usage_summaries = Vec::new();
    let mut approvals = Vec::new();
    let mut user_inputs = Vec::new();
    for session in &session_records {
        for thread in store.list_session_threads(&session.id, 50)? {
            let events = store.read_events(&thread.id, 0)?;
            let resolved_approval_ids = events
                .iter()
                .filter_map(TuiApprovalRequest::response_request_id)
                .collect::<BTreeSet<_>>();
            let resolved_user_input_ids = events
                .iter()
                .filter_map(TuiUserInputRequest::response_request_id)
                .collect::<BTreeSet<_>>();
            items.extend(
                store
                    .list_items(&thread.id, None)?
                    .into_iter()
                    .map(TuiItem::from),
            );
            tasks.extend(
                store
                    .list_tasks(Some(&session.id), Some(&thread.id), 20)?
                    .into_iter()
                    .map(TuiTaskRecord::from),
            );
            automations.extend(
                store
                    .list_automations(Some(&session.id), Some(&thread.id), 20)?
                    .into_iter()
                    .map(TuiAutomationRecord::from),
            );
            let usage = store.list_usage(Some(&thread.id), usize::MAX)?;
            if !usage.is_empty() {
                usage_summaries.push(TuiUsageSummary::from_usage_records(&thread.id, &usage));
            }
            approvals.extend(events.iter().filter_map(|event| {
                let approval = TuiApprovalRequest::from_runtime_event(event)?;
                if resolved_approval_ids.contains(&approval.id) {
                    None
                } else {
                    Some(approval)
                }
            }));
            user_inputs.extend(events.iter().filter_map(|event| {
                let request = TuiUserInputRequest::from_runtime_event(event)?;
                if resolved_user_input_ids.contains(&request.id) {
                    None
                } else {
                    Some(request)
                }
            }));
            threads.push(TuiThread::from(thread));
        }
    }
    let sessions = session_records
        .into_iter()
        .map(TuiSession::from)
        .collect::<Vec<_>>();
    Ok(RuntimeSnapshot {
        sessions,
        threads,
        items,
        tasks,
        automations,
        usage_summaries,
        approvals,
        user_inputs,
    })
}

fn runtime_snapshot_signature(snapshot: &RuntimeSnapshot) -> RuntimeSnapshotSignature {
    let mut sessions = snapshot
        .sessions
        .iter()
        .map(|session| {
            (
                session.id.clone(),
                session.status.clone(),
                session.active_thread_id.clone(),
                session.thread_count,
            )
        })
        .collect::<Vec<_>>();
    sessions.sort();

    let mut threads = snapshot
        .threads
        .iter()
        .map(|thread| {
            (
                thread.id.clone(),
                thread.status.clone(),
                thread.mode.clone(),
                thread.latest_turn_id.clone(),
                thread.event_seq,
            )
        })
        .collect::<Vec<_>>();
    threads.sort();

    let mut tasks = snapshot
        .tasks
        .iter()
        .map(|task| {
            (
                task.id.clone(),
                task.status.clone(),
                task.updated_at.clone(),
            )
        })
        .collect::<Vec<_>>();
    tasks.sort();

    let mut automations = snapshot
        .automations
        .iter()
        .map(|automation| {
            (
                automation.id.clone(),
                automation.status.clone(),
                automation.last_run_at.clone(),
                automation.next_run_at.clone(),
            )
        })
        .collect::<Vec<_>>();
    automations.sort();

    let mut usage_summaries = snapshot
        .usage_summaries
        .iter()
        .map(|usage| {
            (
                usage.thread_id.clone(),
                usage.record_count,
                usage.total_tokens,
                usage.latest_total_tokens,
            )
        })
        .collect::<Vec<_>>();
    usage_summaries.sort();

    let mut approvals = snapshot
        .approvals
        .iter()
        .map(|approval| (approval.id.clone(), approval.status.clone()))
        .collect::<Vec<_>>();
    approvals.sort();

    let mut user_inputs = snapshot
        .user_inputs
        .iter()
        .map(|request| (request.id.clone(), request.status.clone()))
        .collect::<Vec<_>>();
    user_inputs.sort();

    RuntimeSnapshotSignature {
        sessions,
        threads,
        item_count: snapshot.items.len(),
        tasks,
        automations,
        usage_summaries,
        approvals,
        user_inputs,
    }
}

fn snapshot_live_event(snapshot: RuntimeSnapshot) -> TuiLiveEvent {
    TuiLiveEvent::ReplaceRuntime {
        sessions: snapshot.sessions,
        threads: snapshot.threads,
        items: snapshot.items,
        tasks: snapshot.tasks,
        automations: snapshot.automations,
        usage_summaries: snapshot.usage_summaries,
        approvals: snapshot.approvals,
        user_inputs: snapshot.user_inputs,
    }
}

fn start_runtime_live_watcher(
    store: RuntimeStore,
    tx: Sender<TuiLiveEvent>,
    interval: Duration,
) -> RuntimeLiveWatcher {
    let stop = Arc::new(AtomicBool::new(false));
    let worker_stop = stop.clone();
    let mut last_signature = runtime_snapshot(&store)
        .ok()
        .map(|snapshot| runtime_snapshot_signature(&snapshot));
    let handle = thread::spawn(move || {
        while !worker_stop.load(Ordering::Relaxed) {
            thread::sleep(interval);
            if worker_stop.load(Ordering::Relaxed) {
                break;
            }
            match runtime_snapshot(&store) {
                Ok(snapshot) => {
                    let signature = runtime_snapshot_signature(&snapshot);
                    if last_signature.as_ref() == Some(&signature) {
                        continue;
                    }
                    last_signature = Some(signature);
                    if tx.send(snapshot_live_event(snapshot)).is_err() {
                        break;
                    }
                }
                Err(error) => {
                    if tx
                        .send(TuiLiveEvent::Status(format!(
                            "runtime live watcher failed: {error}"
                        )))
                        .is_err()
                    {
                        break;
                    }
                }
            }
        }
    });
    RuntimeLiveWatcher {
        stop,
        handle: Some(handle),
    }
}

fn app_from_store(store: &RuntimeStore) -> AppResult<TuiApp> {
    let snapshot = runtime_snapshot(store)?;
    Ok(
        TuiApp::with_runtime_usage_tasks_automations_approvals_and_user_inputs(
            snapshot.sessions,
            snapshot.threads,
            snapshot.items,
            snapshot.tasks,
            snapshot.automations,
            snapshot.usage_summaries,
            snapshot.approvals,
            snapshot.user_inputs,
        ),
    )
}

fn configure_tui_slash_completions(app: &mut TuiApp, config: &AppConfig) {
    let provider_models = provider_model_completion_values_for_base_url(&config.model.base_url);
    let mut completions = discover_custom_slash_commands_dir(&config.workspace.user_commands_dir());
    completions.extend(
        provider_models
            .iter()
            .flat_map(|model| [format!("/model {model}"), format!("/config model {model}")]),
    );
    let repo_dir = resolve_repo_skills_dir();
    let user_dir = expand_tilde(&config.workspace.user_skills_dir);
    if let Ok((registry, _stats)) =
        SkillRegistry::load_dirs(&[repo_dir.as_path(), user_dir.as_path()])
    {
        completions.extend(
            registry
                .iter()
                .flat_map(|skill| [format!("/skill {}", skill.name), format!("/{}", skill.name)]),
        );
    }
    app.set_extra_slash_completions(completions);
    app.set_extra_command_completions(
        provider_models
            .iter()
            .flat_map(|model| [format!("model {model}"), format!("config model {model}")]),
    );
}

fn refresh_app_from_store(store: &RuntimeStore, app: &mut TuiApp) -> AppResult<()> {
    let snapshot = runtime_snapshot(store)?;
    app.replace_runtime_with_usage_tasks_automations_approvals_and_user_inputs(
        snapshot.sessions,
        snapshot.threads,
        snapshot.items,
        snapshot.tasks,
        snapshot.automations,
        snapshot.usage_summaries,
        snapshot.approvals,
        snapshot.user_inputs,
    );
    Ok(())
}

fn drain_tui_live_events(rx: &Receiver<TuiLiveEvent>, app: &mut TuiApp) -> AppResult<()> {
    while let Ok(event) = rx.try_recv() {
        app.apply_live_event(event);
    }
    Ok(())
}

#[cfg(test)]
fn handle_tui_action(
    store: &RuntimeStore,
    config: Option<&AppConfig>,
    app: &mut TuiApp,
    action: TuiAction,
) -> AppResult<()> {
    handle_tui_action_with_live(store, config, app, action, None)
}

fn mcp_detail_summary(
    config: &AppConfig,
    kind: &TuiMcpDetailKind,
    server: Option<&str>,
) -> AppResult<String> {
    match kind {
        TuiMcpDetailKind::Manager => mcp_manager_summary(config),
        TuiMcpDetailKind::Tools => list_remote_tools_summary(config, server),
        TuiMcpDetailKind::Prompts => list_remote_prompts_summary(config, server),
        TuiMcpDetailKind::Resources => list_remote_resources_summary(config, server),
        TuiMcpDetailKind::ResourceTemplates => {
            list_remote_resource_templates_summary(config, server)
        }
        TuiMcpDetailKind::Health => validate_servers_summary(config),
        TuiMcpDetailKind::Shell => Err(app_error("shell details are not MCP details")),
        TuiMcpDetailKind::Memory => Err(app_error("memory details are not MCP details")),
        TuiMcpDetailKind::Network => Err(app_error("network details are not MCP details")),
        TuiMcpDetailKind::Lsp => Err(app_error("lsp details are not MCP details")),
        TuiMcpDetailKind::Change => Err(app_error("change details are not MCP details")),
        TuiMcpDetailKind::System => Err(app_error("system details are not MCP details")),
        TuiMcpDetailKind::Edit => Err(app_error("edit details are not MCP details")),
        TuiMcpDetailKind::Undo => Err(app_error("undo details are not MCP details")),
        TuiMcpDetailKind::Retry => Err(app_error("retry details are not MCP details")),
        TuiMcpDetailKind::Cycles => Err(app_error("cycle details are not MCP details")),
        TuiMcpDetailKind::Recall => Err(app_error("recall details are not MCP details")),
        TuiMcpDetailKind::Review => Err(app_error("review details are not MCP details")),
        TuiMcpDetailKind::Status => Err(app_error("status details are not MCP details")),
        TuiMcpDetailKind::Tokens => Err(app_error("token details are not MCP details")),
        TuiMcpDetailKind::Translate => Err(app_error("translate details are not MCP details")),
        TuiMcpDetailKind::Cost => Err(app_error("cost details are not MCP details")),
        TuiMcpDetailKind::Cache => Err(app_error("cache details are not MCP details")),
        TuiMcpDetailKind::Diff => Err(app_error("diff details are not MCP details")),
        TuiMcpDetailKind::Clear => Err(app_error("clear details are not MCP details")),
        TuiMcpDetailKind::Model => Err(app_error("model details are not MCP details")),
        TuiMcpDetailKind::Provider => Err(app_error("provider details are not MCP details")),
        TuiMcpDetailKind::Profile => Err(app_error("profile details are not MCP details")),
        TuiMcpDetailKind::Trust => Err(app_error("trust details are not MCP details")),
        TuiMcpDetailKind::Logout => Err(app_error("logout details are not MCP details")),
        TuiMcpDetailKind::Skills => Err(app_error("skill details are not MCP details")),
        TuiMcpDetailKind::Feedback => Err(app_error("feedback details are not MCP details")),
        TuiMcpDetailKind::Links => Err(app_error("link details are not MCP details")),
        TuiMcpDetailKind::Home => Err(app_error("home details are not MCP details")),
        TuiMcpDetailKind::Note => Err(app_error("note details are not MCP details")),
        TuiMcpDetailKind::Subagents => Err(app_error("subagent details are not MCP details")),
        TuiMcpDetailKind::Rlm => Err(app_error("rlm details are not MCP details")),
        TuiMcpDetailKind::Relay => Err(app_error("relay details are not MCP details")),
        TuiMcpDetailKind::Anchor => Err(app_error("anchor details are not MCP details")),
        TuiMcpDetailKind::Queue => Err(app_error("queue details are not MCP details")),
        TuiMcpDetailKind::Share => Err(app_error("share details are not MCP details")),
        TuiMcpDetailKind::Export => Err(app_error("export details are not MCP details")),
        TuiMcpDetailKind::Save => Err(app_error("save details are not MCP details")),
        TuiMcpDetailKind::Load => Err(app_error("load details are not MCP details")),
        TuiMcpDetailKind::Attach => Err(app_error("attach details are not MCP details")),
        TuiMcpDetailKind::Hooks => Err(app_error("hooks details are not MCP details")),
        TuiMcpDetailKind::Goal => Err(app_error("goal details are not MCP details")),
        TuiMcpDetailKind::Mode => Err(app_error("mode details are not MCP details")),
        TuiMcpDetailKind::Help => Err(app_error("help details are not MCP details")),
        TuiMcpDetailKind::Settings => Err(app_error("settings details are not MCP details")),
        TuiMcpDetailKind::Setup => Err(app_error("setup details are not MCP details")),
        TuiMcpDetailKind::Theme => Err(app_error("theme details are not MCP details")),
        TuiMcpDetailKind::StatusLine => Err(app_error("statusline details are not MCP details")),
        TuiMcpDetailKind::Verbose => Err(app_error("verbose details are not MCP details")),
        TuiMcpDetailKind::Context => Err(app_error("context details are not MCP details")),
        TuiMcpDetailKind::Rollback => Err(app_error("rollback details are not MCP details")),
        TuiMcpDetailKind::Reasoning => Err(app_error("reasoning details are not MCP details")),
        TuiMcpDetailKind::ComposerStash => {
            Err(app_error("composer stash details are not MCP details"))
        }
    }
}

const REMOTE_SKILL_REGISTRY_MAX_BYTES: usize = 1_000_000;
const REMOTE_SKILL_DOWNLOAD_MAX_BYTES: usize = 5 * 1024 * 1024;
const REMOTE_SKILL_REGISTRY_TIMEOUT_MS: u64 = 15_000;

fn format_skills_summary(config: &AppConfig, command: &TuiSkillsCommand) -> AppResult<String> {
    match command {
        TuiSkillsCommand::Remote => return Ok(format_remote_skills_summary(config)),
        TuiSkillsCommand::Sync => return Ok(sync_remote_skills_summary(config)),
        TuiSkillsCommand::Install { source } => return install_user_skill(config, source),
        TuiSkillsCommand::Update { name } => return update_user_skill(config, name),
        TuiSkillsCommand::Uninstall { name } => return uninstall_user_skill(config, name),
        TuiSkillsCommand::Trust { name } => return trust_user_skill(config, name),
        TuiSkillsCommand::List { .. } | TuiSkillsCommand::Show { .. } => {}
    }

    let repo_dir = resolve_repo_skills_dir();
    let user_dir = expand_tilde(&config.workspace.user_skills_dir);
    let search_dirs = [repo_dir.as_path(), user_dir.as_path()];
    let (registry, stats) = SkillRegistry::load_dirs(&search_dirs)?;
    let searched = stats
        .by_path
        .iter()
        .map(|(path, count)| format!("- {}: {} skill(s)", path.display(), count))
        .collect::<Vec<_>>()
        .join("\n");
    match command {
        TuiSkillsCommand::List { prefix } => {
            let mut skills = registry.iter().collect::<Vec<_>>();
            if let Some(prefix) = prefix.as_deref() {
                let prefix = prefix.to_ascii_lowercase();
                skills.retain(|skill| skill.name.to_ascii_lowercase().starts_with(&prefix));
            }
            let mut detail = match prefix {
                Some(prefix) => format!(
                    "Available skills matching `{prefix}` ({} of {})\n\n",
                    skills.len(),
                    stats.total
                ),
                None => format!("Available skills ({})\n\n", stats.total),
            };
            if skills.is_empty() {
                detail.push_str("No matching skills found.\n");
            } else {
                for skill in skills {
                    detail.push_str(&format!(
                        "- {}: {}\n",
                        skill.name,
                        empty_as_placeholder(&skill.description)
                    ));
                }
            }
            detail.push_str("\nSearch paths:\n");
            detail.push_str(if searched.is_empty() {
                "- none"
            } else {
                &searched
            });
            if !stats.overridden.is_empty() {
                detail.push_str("\n\nOverrides:\n");
                detail.push_str(&stats.overridden.join(", "));
            }
            detail.push_str(
                "\n\nUse skill <name> to inspect a skill, /skill trust <name> to trust a user skill, or /skill uninstall <name> to remove a user skill.",
            );
            Ok(detail)
        }
        TuiSkillsCommand::Show { name } => {
            let Some(skill) = registry.find(name) else {
                let available = registry
                    .iter()
                    .map(|skill| skill.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                let mut detail = format!("Skill `{name}` not found.\n\n");
                detail.push_str("Available skills: ");
                detail.push_str(if available.is_empty() {
                    "none"
                } else {
                    &available
                });
                detail.push_str("\n\nSearch paths:\n");
                detail.push_str(if searched.is_empty() {
                    "- none"
                } else {
                    &searched
                });
                return Ok(detail);
            };
            Ok(format_skill_detail(skill, &searched))
        }
        TuiSkillsCommand::Remote
        | TuiSkillsCommand::Sync
        | TuiSkillsCommand::Install { .. }
        | TuiSkillsCommand::Update { .. }
        | TuiSkillsCommand::Uninstall { .. }
        | TuiSkillsCommand::Trust { .. } => {
            unreachable!("non-local-list skill command handled before local load")
        }
    }
}

fn format_remote_skills_summary(config: &AppConfig) -> String {
    let registry_url = config.skills.registry_url.trim();
    let registry_url = if registry_url.is_empty() {
        crate::config::types::DEFAULT_SKILL_REGISTRY_URL
    } else {
        registry_url
    };
    let body = match fetch_url_text(
        registry_url,
        REMOTE_SKILL_REGISTRY_MAX_BYTES,
        REMOTE_SKILL_REGISTRY_TIMEOUT_MS,
        false,
    ) {
        Ok(body) => body,
        Err(error) => {
            return format!(
                "Remote skill registry unavailable.\n\nRegistry: {registry_url}\nError: {error}\n\nConfigure `skills.registry_url` or allow the registry host with `network allow <host>` before retrying."
            );
        }
    };

    match parse_remote_skill_entries(&body) {
        Ok(entries) => render_remote_skill_entries(registry_url, &entries),
        Err(error) => format!(
            "Remote skill registry could not be parsed.\n\nRegistry: {registry_url}\nError: {error}\n\nExpected JSON with a top-level `skills` object."
        ),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RemoteSkillEntry {
    name: String,
    description: String,
    source: String,
}

fn parse_remote_skill_entries(body: &str) -> AppResult<Vec<RemoteSkillEntry>> {
    let root = parse_root_object(body)?;
    let Some(skills) = root.get("skills") else {
        return Err(app_error("remote skill registry missing `skills`"));
    };
    let mut entries = Vec::new();
    match skills {
        JsonValue::Object(map) => {
            for (name, value) in map {
                let object = json_as_object(value).ok_or_else(|| {
                    app_error(format!("remote skill `{name}` entry must be an object"))
                })?;
                let source = object
                    .get("source")
                    .and_then(json_as_string)
                    .unwrap_or("")
                    .trim()
                    .to_string();
                if source.is_empty() {
                    return Err(app_error(format!(
                        "remote skill `{name}` missing required `source`"
                    )));
                }
                let description = object
                    .get("description")
                    .and_then(json_as_string)
                    .unwrap_or("")
                    .trim()
                    .to_string();
                entries.push(RemoteSkillEntry {
                    name: name.to_string(),
                    description,
                    source,
                });
            }
        }
        JsonValue::Array(items) => {
            for item in items {
                let object = json_as_object(item)
                    .ok_or_else(|| app_error("remote skill array entries must be objects"))?;
                let name = object
                    .get("name")
                    .and_then(json_as_string)
                    .unwrap_or("")
                    .trim()
                    .to_string();
                let source = object
                    .get("source")
                    .and_then(json_as_string)
                    .unwrap_or("")
                    .trim()
                    .to_string();
                if name.is_empty() || source.is_empty() {
                    return Err(app_error(
                        "remote skill array entries require `name` and `source`",
                    ));
                }
                let description = object
                    .get("description")
                    .and_then(json_as_string)
                    .unwrap_or("")
                    .trim()
                    .to_string();
                entries.push(RemoteSkillEntry {
                    name,
                    description,
                    source,
                });
            }
        }
        _ => return Err(app_error("remote `skills` must be an object or array")),
    }
    entries.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(entries)
}

fn render_remote_skill_entries(registry_url: &str, entries: &[RemoteSkillEntry]) -> String {
    let mut detail = format!("Available remote skills ({})\n\n", entries.len());
    if entries.is_empty() {
        detail.push_str("Remote registry is empty.\n");
    } else {
        for entry in entries {
            detail.push_str(&format!(
                "- {}: {}\n  source: {}\n",
                entry.name,
                empty_as_placeholder(&entry.description),
                entry.source
            ));
        }
    }
    detail.push_str("\nRegistry: ");
    detail.push_str(registry_url);
    detail.push_str(
        "\n\nUse /skill install <name|url> for direct TOML, SKILL.md, GitHub, tar.gz, or zip skill sources, or /skills sync to cache supported registry entries.",
    );
    detail
}

fn sync_remote_skills_summary(config: &AppConfig) -> String {
    let registry_url = config.skills.registry_url.trim();
    let registry_url = if registry_url.is_empty() {
        crate::config::types::DEFAULT_SKILL_REGISTRY_URL
    } else {
        registry_url
    };
    let body = match fetch_url_text(
        registry_url,
        REMOTE_SKILL_REGISTRY_MAX_BYTES,
        REMOTE_SKILL_REGISTRY_TIMEOUT_MS,
        false,
    ) {
        Ok(body) => body,
        Err(error) => {
            return format!(
                "Remote skill registry sync failed.\n\nRegistry: {registry_url}\nError: {error}\n\nConfigure `skills.registry_url` or allow the registry host with `network allow <host>` before retrying."
            );
        }
    };
    let entries = match parse_remote_skill_entries(&body) {
        Ok(entries) => entries,
        Err(error) => {
            return format!(
                "Remote skill registry sync failed.\n\nRegistry: {registry_url}\nError: {error}\n\nExpected JSON with a top-level `skills` object."
            );
        }
    };
    let cache_dir = expand_tilde(&config.skills.cache_dir);
    if let Err(error) = std::fs::create_dir_all(&cache_dir) {
        return format!(
            "Remote skill registry sync failed.\n\nCache dir: {}\nError: {error}",
            cache_dir.display()
        );
    }

    let mut downloaded = 0usize;
    let mut fresh = 0usize;
    let mut skipped = 0usize;
    let mut failed = 0usize;
    let mut lines = Vec::new();
    for entry in &entries {
        match sync_remote_skill_entry(entry, &cache_dir) {
            SkillSyncEntryOutcome::Downloaded { name, path } => {
                downloaded += 1;
                lines.push(format!("[+] {name} downloaded to {}", path.display()));
            }
            SkillSyncEntryOutcome::Fresh { name } => {
                fresh += 1;
                lines.push(format!("[=] {name} already up to date"));
            }
            SkillSyncEntryOutcome::Skipped { name, reason } => {
                skipped += 1;
                lines.push(format!("[-] {name} skipped: {reason}"));
            }
            SkillSyncEntryOutcome::Failed { name, reason } => {
                failed += 1;
                lines.push(format!("[!] {name} failed: {reason}"));
            }
        }
    }

    let mut detail = format!(
        "Remote skill registry sync complete.\n\nRegistry: {registry_url}\nCache dir: {}\n\n",
        cache_dir.display()
    );
    if lines.is_empty() {
        detail.push_str("Registry is empty.\n");
    } else {
        for line in lines {
            detail.push_str("- ");
            detail.push_str(&line);
            detail.push('\n');
        }
    }
    detail.push_str(&format!(
        "\n{} skill(s) processed: {downloaded} downloaded, {fresh} up-to-date, {skipped} skipped, {failed} failed.",
        entries.len()
    ));
    detail
}

enum SkillSyncEntryOutcome {
    Downloaded { name: String, path: PathBuf },
    Fresh { name: String },
    Skipped { name: String, reason: String },
    Failed { name: String, reason: String },
}

fn sync_remote_skill_entry(entry: &RemoteSkillEntry, cache_dir: &Path) -> SkillSyncEntryOutcome {
    let install_source = match resolve_remote_skill_entry_source(&entry.source) {
        Ok(source) => source,
        Err(message) => {
            return SkillSyncEntryOutcome::Skipped {
                name: entry.name.clone(),
                reason: message,
            };
        }
    };
    let downloaded = match fetch_first_skill_source(&install_source.candidate_urls) {
        Ok(downloaded) => downloaded,
        Err(error) => {
            return SkillSyncEntryOutcome::Failed {
                name: entry.name.clone(),
                reason: error.to_string(),
            };
        }
    };
    let content = match skill_source_bytes_to_toml(&downloaded.url, &downloaded.bytes) {
        Ok(content) => content,
        Err(error) => {
            return SkillSyncEntryOutcome::Failed {
                name: entry.name.clone(),
                reason: error.to_string(),
            };
        }
    };
    let temp_path = temporary_skill_path(cache_dir);
    if let Err(error) = std::fs::write(&temp_path, &content) {
        return SkillSyncEntryOutcome::Failed {
            name: entry.name.clone(),
            reason: error.to_string(),
        };
    }
    let skill = match validate_downloaded_skill_toml(&temp_path, &content) {
        Ok(skill) => skill,
        Err(error) => {
            let _ = std::fs::remove_file(&temp_path);
            return SkillSyncEntryOutcome::Failed {
                name: entry.name.clone(),
                reason: error.to_string(),
            };
        }
    };
    let final_path = match user_skill_path(cache_dir, &skill.name) {
        Ok(path) => path,
        Err(error) => {
            let _ = std::fs::remove_file(&temp_path);
            return SkillSyncEntryOutcome::Failed {
                name: entry.name.clone(),
                reason: error.to_string(),
            };
        }
    };
    let checksum = checksum_hex(&content);
    let marker = skill_sync_marker_path(&final_path);
    if final_path.exists()
        && std::fs::read_to_string(&marker)
            .ok()
            .and_then(|body| read_marker_value(&body, "checksum"))
            .as_deref()
            == Some(checksum.as_str())
    {
        let _ = std::fs::remove_file(&temp_path);
        return SkillSyncEntryOutcome::Fresh { name: skill.name };
    }
    if let Err(error) = std::fs::rename(&temp_path, &final_path) {
        let _ = std::fs::remove_file(&temp_path);
        return SkillSyncEntryOutcome::Failed {
            name: skill.name,
            reason: error.to_string(),
        };
    }
    if let Err(error) = write_skill_sync_marker(&marker, &entry.name, &downloaded.url, &checksum) {
        return SkillSyncEntryOutcome::Failed {
            name: skill.name,
            reason: error.to_string(),
        };
    }
    SkillSyncEntryOutcome::Downloaded {
        name: skill.name,
        path: final_path,
    }
}

fn skill_sync_marker_path(skill_path: &Path) -> PathBuf {
    skill_path.with_extension("sync-meta")
}

fn write_skill_sync_marker(
    marker: &Path,
    registry_name: &str,
    source: &str,
    checksum: &str,
) -> AppResult<()> {
    std::fs::write(
        marker,
        format!(
            "registry_name = \"{}\"\nsource = \"{}\"\nchecksum = \"{}\"\n",
            toml_escape(registry_name),
            toml_escape(source),
            toml_escape(checksum)
        ),
    )?;
    Ok(())
}

fn install_user_skill(config: &AppConfig, source: &str) -> AppResult<String> {
    let source = source.trim();
    if source.is_empty() {
        return Ok("Usage: /skill install <registry-name|url>".to_string());
    }
    let install_source = match resolve_skill_install_source(config, source) {
        Ok(source) => source,
        Err(message) => return Ok(message),
    };
    let downloaded = match fetch_first_skill_source(&install_source.candidate_urls) {
        Ok(downloaded) => downloaded,
        Err(error) => {
            return Ok(format!(
                "Skill install failed.\n\nSource: {source}\nError: {error}"
            ));
        }
    };
    let content = match skill_source_bytes_to_toml(&downloaded.url, &downloaded.bytes) {
        Ok(content) => content,
        Err(error) => {
            return Ok(format!(
                "Skill install failed.\n\nSource: {source}\nURL: {}\nError: {error}",
                downloaded.url
            ));
        }
    };
    let user_dir = expand_tilde(&config.workspace.user_skills_dir);
    std::fs::create_dir_all(&user_dir)?;
    let temp_path = temporary_skill_path(&user_dir);
    std::fs::write(&temp_path, &content)?;
    let skill = match validate_downloaded_skill_toml(&temp_path, &content) {
        Ok(skill) => skill,
        Err(error) => {
            let _ = std::fs::remove_file(&temp_path);
            return Ok(format!(
                "Skill install failed.\n\nSource: {source}\nURL: {}\nError: {error}",
                downloaded.url
            ));
        }
    };
    let final_path = user_skill_path(&user_dir, &skill.name)?;
    if final_path.exists() {
        let _ = std::fs::remove_file(&temp_path);
        return Ok(format!(
            "Skill `{}` is already installed at {}.\n\nUse /skill update {} to refresh it or /skill uninstall {} before reinstalling.",
            skill.name,
            final_path.display(),
            skill.name,
            skill.name
        ));
    }
    std::fs::rename(&temp_path, &final_path)?;
    let checksum = checksum_hex(&content);
    write_installed_from_marker(
        &installed_from_marker_path(&final_path),
        source,
        &downloaded.url,
        &checksum,
    )?;
    Ok(format!(
        "Skill `{}` installed.\n\nSource: {source}\nURL: {}\nSkill file: {}\nInstall marker: {}\n\nRun /skills to refresh the list.",
        skill.name,
        downloaded.url,
        final_path.display(),
        installed_from_marker_path(&final_path).display()
    ))
}

fn update_user_skill(config: &AppConfig, name: &str) -> AppResult<String> {
    let name = name.trim();
    if name.is_empty() {
        return Ok("Usage: /skill update <name>".to_string());
    }
    let repo_dir = resolve_repo_skills_dir();
    let user_dir = expand_tilde(&config.workspace.user_skills_dir);
    let Some(skill_path) = find_skill_file_in_dir(&user_dir, name)? else {
        if find_skill_file_in_dir(&repo_dir, name)?.is_some() {
            return Ok(format!(
                "Cannot update bundled skill `{name}` from the TUI.\n\nCreate a user override in {} or install a user skill source first.",
                user_dir.display()
            ));
        }
        return Ok(format!(
            "Skill `{name}` not found in user skills.\n\nUser skills path: {}\nRun /skills to list configured skills.",
            user_dir.display()
        ));
    };
    let marker = installed_from_marker_path(&skill_path);
    if !marker.exists() {
        return Ok(format!(
            "Skill `{name}` was not installed by /skill install.\n\nMissing marker: {}\nReinstall from a supported TOML, SKILL.md, tarball, zip, GitHub, or registry source to enable /skill update.",
            marker.display()
        ));
    }
    let marker_body = std::fs::read_to_string(&marker)?;
    let source = read_marker_value(&marker_body, "source")
        .or_else(|| read_marker_value(&marker_body, "url"))
        .unwrap_or_default();
    if source.trim().is_empty() {
        return Ok(format!(
            "Skill `{name}` install marker is missing a source.\n\nMarker: {}",
            marker.display()
        ));
    }
    let install_source = match resolve_skill_install_source(config, &source) {
        Ok(source) => source,
        Err(message) => return Ok(message),
    };
    let downloaded = match fetch_first_skill_source(&install_source.candidate_urls) {
        Ok(downloaded) => downloaded,
        Err(error) => {
            return Ok(format!(
                "Skill update failed.\n\nSkill: {name}\nSource: {source}\nError: {error}"
            ));
        }
    };
    let content = match skill_source_bytes_to_toml(&downloaded.url, &downloaded.bytes) {
        Ok(content) => content,
        Err(error) => {
            return Ok(format!(
                "Skill update failed.\n\nSkill: {name}\nSource: {source}\nURL: {}\nError: {error}",
                downloaded.url
            ));
        }
    };
    let checksum = checksum_hex(&content);
    if read_marker_value(&marker_body, "checksum").as_deref() == Some(checksum.as_str()) {
        return Ok(format!(
            "Skill `{name}` is already up to date.\n\nSource: {source}\nURL: {}",
            downloaded.url
        ));
    }
    let temp_path = temporary_skill_path(&user_dir);
    std::fs::write(&temp_path, &content)?;
    let skill = match validate_downloaded_skill_toml(&temp_path, &content) {
        Ok(skill) => skill,
        Err(error) => {
            let _ = std::fs::remove_file(&temp_path);
            return Ok(format!(
                "Skill update failed.\n\nSkill: {name}\nSource: {source}\nURL: {}\nError: {error}",
                downloaded.url
            ));
        }
    };
    if skill.name != name {
        let _ = std::fs::remove_file(&temp_path);
        return Ok(format!(
            "Skill update refused because the downloaded skill name changed.\n\nExpected: {name}\nDownloaded: {}",
            skill.name
        ));
    }
    std::fs::write(&skill_path, &content)?;
    let _ = std::fs::remove_file(&temp_path);
    write_installed_from_marker(&marker, &source, &downloaded.url, &checksum)?;
    Ok(format!(
        "Skill `{name}` updated.\n\nSource: {source}\nURL: {}\nSkill file: {}\nInstall marker: {}",
        downloaded.url,
        skill_path.display(),
        marker.display()
    ))
}

struct SkillInstallSource {
    candidate_urls: Vec<String>,
}

struct DownloadedSkillSource {
    url: String,
    bytes: Vec<u8>,
}

fn resolve_skill_install_source(
    config: &AppConfig,
    source: &str,
) -> Result<SkillInstallSource, String> {
    let source = source.trim();
    match resolve_remote_skill_entry_source(source) {
        Ok(source) => return Ok(source),
        Err(message) if source.starts_with("github:") || is_http_url(source) => {
            return Err(message);
        }
        Err(_) => {}
    }

    let registry_url = config.skills.registry_url.trim();
    let registry_url = if registry_url.is_empty() {
        crate::config::types::DEFAULT_SKILL_REGISTRY_URL
    } else {
        registry_url
    };
    let body = fetch_url_text(
        registry_url,
        REMOTE_SKILL_REGISTRY_MAX_BYTES,
        REMOTE_SKILL_REGISTRY_TIMEOUT_MS,
        false,
    )
    .map_err(|error| {
        format!(
            "Skill install failed while fetching the remote registry.\n\nRegistry: {registry_url}\nError: {error}"
        )
    })?;
    let entries = parse_remote_skill_entries(&body).map_err(|error| {
        format!(
            "Skill install failed while parsing the remote registry.\n\nRegistry: {registry_url}\nError: {error}"
        )
    })?;
    let Some(entry) = entries.iter().find(|entry| entry.name == source) else {
        let available = entries
            .iter()
            .map(|entry| entry.name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        return Err(format!(
            "Remote skill `{source}` not found in registry.\n\nRegistry: {registry_url}\nAvailable: {}",
            if available.is_empty() { "none" } else { &available }
        ));
    };
    resolve_remote_skill_entry_source(&entry.source)
}

fn is_http_url(value: &str) -> bool {
    value.starts_with("http://") || value.starts_with("https://")
}

fn resolve_remote_skill_entry_source(source: &str) -> Result<SkillInstallSource, String> {
    let source = source.trim();
    if source.is_empty() {
        return Err("source is empty".to_string());
    }
    if let Some(repo) = parse_github_source(source)? {
        return Ok(SkillInstallSource {
            candidate_urls: github_archive_candidate_urls(&repo),
        });
    }
    if is_http_url(source) {
        if let Some(repo) = parse_github_browser_url(source) {
            return Ok(SkillInstallSource {
                candidate_urls: github_archive_candidate_urls(&repo),
            });
        }
        return Ok(SkillInstallSource {
            candidate_urls: vec![source.to_string()],
        });
    }
    Err(format!(
        "source is not a supported direct skill URL or github:owner/repo spec: {source}"
    ))
}

fn parse_github_source(source: &str) -> Result<Option<String>, String> {
    let Some(rest) = source.strip_prefix("github:") else {
        return Ok(None);
    };
    let (owner, repo) = rest
        .trim()
        .split_once('/')
        .ok_or_else(|| format!("github source must be `github:owner/repo` (got {source})"))?;
    let owner = owner.trim();
    let repo = repo.trim().trim_end_matches('/').trim_end_matches(".git");
    if owner.is_empty() || repo.is_empty() || owner.contains('/') || repo.contains('/') {
        return Err(format!(
            "github source must be `github:owner/repo` (got {source})"
        ));
    }
    Ok(Some(format!("{owner}/{repo}")))
}

fn parse_github_browser_url(url: &str) -> Option<String> {
    let after_scheme = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    let (host, rest) = after_scheme.split_once('/')?;
    if !host.eq_ignore_ascii_case("github.com") && !host.eq_ignore_ascii_case("www.github.com") {
        return None;
    }
    let trimmed = rest
        .split(['?', '#'])
        .next()
        .unwrap_or(rest)
        .trim_end_matches('/');
    let mut parts = trimmed.splitn(3, '/');
    let owner = parts.next()?.trim();
    let repo = parts.next()?.trim().trim_end_matches(".git");
    if owner.is_empty() || repo.is_empty() || parts.next().is_some() {
        return None;
    }
    Some(format!("{owner}/{repo}"))
}

fn github_archive_candidate_urls(repo: &str) -> Vec<String> {
    vec![
        format!("https://github.com/{repo}/archive/refs/heads/main.tar.gz"),
        format!("https://github.com/{repo}/archive/refs/heads/master.tar.gz"),
    ]
}

fn is_zip_skill_source(source: &str) -> bool {
    let lower = source.to_ascii_lowercase();
    let lower_without_suffix = lower
        .split(|ch| ch == '?' || ch == '#')
        .next()
        .unwrap_or(lower.as_str());
    lower_without_suffix.ends_with(".zip")
}

fn fetch_first_skill_source(candidate_urls: &[String]) -> AppResult<DownloadedSkillSource> {
    let mut errors = Vec::new();
    for url in candidate_urls {
        match fetch_url_bytes(
            url,
            REMOTE_SKILL_DOWNLOAD_MAX_BYTES,
            REMOTE_SKILL_REGISTRY_TIMEOUT_MS,
            false,
        ) {
            Ok(response) if response.status == 404 => {
                errors.push(format!("{url}: HTTP 404"));
            }
            Ok(response) if !(200..=299).contains(&response.status) => {
                errors.push(format!("{url}: HTTP {}", response.status));
            }
            Ok(response)
                if response.truncated || response.total_bytes > REMOTE_SKILL_DOWNLOAD_MAX_BYTES =>
            {
                errors.push(format!(
                    "{url}: response exceeded {} bytes",
                    REMOTE_SKILL_DOWNLOAD_MAX_BYTES
                ));
            }
            Ok(response) => {
                return Ok(DownloadedSkillSource {
                    url: if response.final_url.trim().is_empty() {
                        url.clone()
                    } else {
                        response.final_url
                    },
                    bytes: response.body_bytes,
                });
            }
            Err(error) => errors.push(format!("{url}: {error}")),
        }
    }
    Err(app_error(format!(
        "all candidate skill URLs failed: {}",
        if errors.is_empty() {
            "none".to_string()
        } else {
            errors.join("; ")
        }
    )))
}

fn skill_source_bytes_to_toml(url: &str, bytes: &[u8]) -> AppResult<String> {
    if is_zip_skill_source(url) || bytes.starts_with(b"PK\x03\x04") {
        let skill_md = read_skill_md_from_zip(bytes)?;
        return skill_md_to_toml(&skill_md);
    }
    if is_tar_gz_skill_source(url, bytes) {
        let skill_md = read_skill_md_from_tar_gz(bytes)?;
        return skill_md_to_toml(&skill_md);
    }
    let content = String::from_utf8(bytes.to_vec())
        .map_err(|_| app_error("downloaded skill source is not valid UTF-8"))?;
    if is_skill_md_source(url) || content.trim_start().starts_with("---") {
        return skill_md_to_toml(&content);
    }
    Ok(content)
}

fn is_tar_gz_skill_source(url: &str, bytes: &[u8]) -> bool {
    let lower = url.to_ascii_lowercase();
    let without_suffix = lower.split(['?', '#']).next().unwrap_or(lower.as_str());
    without_suffix.ends_with(".tar.gz")
        || without_suffix.ends_with(".tgz")
        || bytes.starts_with(&[0x1f, 0x8b])
}

fn is_skill_md_source(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    let without_suffix = lower.split(['?', '#']).next().unwrap_or(lower.as_str());
    without_suffix.ends_with("/skill.md") || without_suffix.ends_with("skill.md")
}

fn read_skill_md_from_tar_gz(bytes: &[u8]) -> AppResult<String> {
    let cursor = std::io::Cursor::new(bytes);
    let decoder = GzDecoder::new(cursor);
    let mut archive = tar::Archive::new(decoder);
    let mut best = None::<(u8, String, Vec<u8>)>;
    let mut total_size = 0usize;

    let entries = archive
        .entries()
        .map_err(|error| app_error(format!("failed to read skill archive entries: {error}")))?;
    for entry in entries {
        let mut entry = entry
            .map_err(|error| app_error(format!("failed to read skill archive entry: {error}")))?;
        let header = entry.header().clone();
        let path = entry
            .path()
            .map_err(|error| app_error(format!("skill archive entry has invalid path: {error}")))?
            .to_path_buf();
        if !is_safe_archive_path(&path) {
            return Err(app_error(format!(
                "skill archive entry escapes destination: {}",
                path.display()
            )));
        }
        if let Ok(size) = header.size() {
            total_size = total_size.saturating_add(size as usize);
            if total_size > REMOTE_SKILL_DOWNLOAD_MAX_BYTES {
                return Err(app_error(format!(
                    "skill archive uncompressed size exceeds {} bytes",
                    REMOTE_SKILL_DOWNLOAD_MAX_BYTES
                )));
            }
        }
        if !header.entry_type().is_file() {
            continue;
        }
        let Some(rank) = skill_md_archive_candidate_rank(&path) else {
            continue;
        };
        if best
            .as_ref()
            .is_some_and(|(current_rank, _, _)| *current_rank <= rank)
        {
            continue;
        }
        let mut body = Vec::new();
        entry
            .read_to_end(&mut body)
            .map_err(|error| app_error(format!("failed to read SKILL.md from archive: {error}")))?;
        best = Some((rank, path.display().to_string(), body));
    }

    let Some((_, path, body)) = best else {
        return Err(app_error("missing SKILL.md in skill archive"));
    };
    String::from_utf8(body)
        .map_err(|_| app_error(format!("SKILL.md in archive is not valid UTF-8: {path}")))
}

fn read_skill_md_from_zip(bytes: &[u8]) -> AppResult<String> {
    let cursor = std::io::Cursor::new(bytes);
    let mut archive = zip::ZipArchive::new(cursor)
        .map_err(|error| app_error(format!("failed to read skill zip archive: {error}")))?;
    let mut best = None::<(u8, String, Vec<u8>)>;
    let mut total_size = 0usize;

    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).map_err(|error| {
            app_error(format!("failed to read skill zip archive entry: {error}"))
        })?;
        let path = zip_entry_path(entry.name())?;
        if entry.size() > REMOTE_SKILL_DOWNLOAD_MAX_BYTES as u64 {
            return Err(app_error(format!(
                "skill zip archive uncompressed size exceeds {} bytes",
                REMOTE_SKILL_DOWNLOAD_MAX_BYTES
            )));
        }
        total_size = total_size.saturating_add(entry.size() as usize);
        if total_size > REMOTE_SKILL_DOWNLOAD_MAX_BYTES {
            return Err(app_error(format!(
                "skill zip archive uncompressed size exceeds {} bytes",
                REMOTE_SKILL_DOWNLOAD_MAX_BYTES
            )));
        }
        if !entry.is_file() {
            continue;
        }
        let Some(rank) = skill_md_archive_candidate_rank(&path) else {
            continue;
        };
        if best
            .as_ref()
            .is_some_and(|(current_rank, _, _)| *current_rank <= rank)
        {
            continue;
        }
        let mut body = Vec::new();
        entry
            .read_to_end(&mut body)
            .map_err(|error| app_error(format!("failed to read SKILL.md from zip: {error}")))?;
        best = Some((rank, path.display().to_string(), body));
    }

    let Some((_, path, body)) = best else {
        return Err(app_error("missing SKILL.md in skill zip archive"));
    };
    String::from_utf8(body).map_err(|_| {
        app_error(format!(
            "SKILL.md in zip archive is not valid UTF-8: {path}"
        ))
    })
}

fn zip_entry_path(name: &str) -> AppResult<PathBuf> {
    if name.contains('\0') {
        return Err(app_error(
            "skill zip archive entry path contains a null byte",
        ));
    }
    let normalized = name.replace('\\', "/");
    let path = PathBuf::from(&normalized);
    if !is_safe_archive_path(&path) {
        return Err(app_error(format!(
            "skill zip archive entry escapes destination: {normalized}"
        )));
    }
    Ok(path)
}

fn skill_md_archive_candidate_rank(path: &Path) -> Option<u8> {
    let raw = path
        .components()
        .filter_map(|component| match component {
            Component::Normal(value) => value.to_str(),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/");
    let raw_rank = skill_md_candidate_rank(&raw);
    let stripped_rank = raw
        .split_once('/')
        .and_then(|(_, stripped)| skill_md_candidate_rank(stripped));
    match (raw_rank, stripped_rank) {
        (Some(left), Some(right)) => Some(left.min(right)),
        (Some(rank), None) | (None, Some(rank)) => Some(rank),
        (None, None) => None,
    }
}

fn skill_md_candidate_rank(path: &str) -> Option<u8> {
    if path.eq_ignore_ascii_case("SKILL.md") {
        return Some(0);
    }
    let parts = path.split('/').collect::<Vec<_>>();
    if parts
        .last()
        .is_none_or(|name| !name.eq_ignore_ascii_case("SKILL.md"))
    {
        return None;
    }
    if parts.len() >= 3 && parts[parts.len() - 3].eq_ignore_ascii_case("skills") {
        return Some(1);
    }
    if parts.len() == 2 {
        return Some(2);
    }
    None
}

fn is_safe_archive_path(path: &Path) -> bool {
    if path.is_absolute() {
        return false;
    }
    path.components().all(|component| {
        !matches!(
            component,
            Component::ParentDir | Component::Prefix(_) | Component::RootDir
        )
    })
}

struct ImportedSkillMd {
    name: String,
    description: String,
    body: String,
}

fn skill_md_to_toml(content: &str) -> AppResult<String> {
    let imported = parse_skill_md(content)?;
    Ok(format!(
        "name = \"{}\"\ndescription = \"{}\"\nallowed_tools = []\nsystem_append = \"\"\"\nImported from a SKILL.md bundle.\n\n{}\n\"\"\"\n\n[policy]\nrequire_write_confirmation = true\nrequire_shell_confirmation = false\nshell_allowlist = []\n",
        toml_escape(&imported.name),
        toml_escape(&imported.description),
        toml_multiline_escape(imported.body.trim())
    ))
}

fn parse_skill_md(content: &str) -> AppResult<ImportedSkillMd> {
    let trimmed = content.trim_start_matches('\u{feff}').trim_start();
    let mut lines = trimmed.lines();
    let Some(first) = lines.next() else {
        return Err(app_error("SKILL.md is empty"));
    };
    if first.trim() != "---" {
        return Err(app_error(
            "SKILL.md is missing the leading `---` frontmatter fence",
        ));
    }

    let mut frontmatter = Vec::new();
    let mut body = Vec::new();
    let mut in_frontmatter = true;
    let mut closed = false;
    for line in lines {
        if in_frontmatter && line.trim() == "---" {
            in_frontmatter = false;
            closed = true;
            continue;
        }
        if in_frontmatter {
            frontmatter.push(line);
        } else {
            body.push(line);
        }
    }
    if !closed {
        return Err(app_error(
            "SKILL.md is missing the closing `---` frontmatter fence",
        ));
    }

    let mut name = String::new();
    let mut description = String::new();
    for line in frontmatter {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim().trim_matches('"').trim_matches('\'');
        match key.trim().to_ascii_lowercase().as_str() {
            "name" => name = value.to_string(),
            "description" => description = value.to_string(),
            _ => {}
        }
    }
    if name.trim().is_empty() {
        return Err(app_error(
            "SKILL.md frontmatter missing required field: name",
        ));
    }
    validate_skill_name(&name)?;
    if description.trim().is_empty() {
        return Err(app_error(
            "SKILL.md frontmatter missing required field: description",
        ));
    }
    Ok(ImportedSkillMd {
        name,
        description,
        body: body.join("\n"),
    })
}

fn toml_multiline_escape(value: &str) -> String {
    value.replace("\"\"\"", "'''")
}

fn validate_downloaded_skill_toml(path: &Path, content: &str) -> AppResult<SkillSpec> {
    let skill = load_skill(path)?;
    if !declares_root_key(content, "name") {
        return Err(app_error("downloaded skill TOML missing required `name`"));
    }
    if skill.description.trim().is_empty() {
        return Err(app_error(
            "downloaded skill TOML missing required `description`",
        ));
    }
    validate_skill_name(&skill.name)?;
    Ok(skill)
}

fn declares_root_key(content: &str, expected: &str) -> bool {
    let mut in_root = true;
    for raw in content.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') {
            in_root = false;
            continue;
        }
        if !in_root {
            continue;
        }
        if let Some((key, _)) = line.split_once('=') {
            if key.trim() == expected {
                return true;
            }
        }
    }
    false
}

fn validate_skill_name(name: &str) -> AppResult<()> {
    if name.trim().is_empty() {
        return Err(app_error("skill name cannot be empty"));
    }
    if name.contains('/') || name.contains('\\') || name.contains("..") {
        return Err(app_error(format!("invalid skill name `{name}`")));
    }
    if !name
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_'))
    {
        return Err(app_error(format!(
            "invalid skill name `{name}`; use letters, numbers, '-' or '_'"
        )));
    }
    Ok(())
}

fn user_skill_path(user_dir: &Path, name: &str) -> AppResult<PathBuf> {
    validate_skill_name(name)?;
    Ok(user_dir.join(format!("{name}.toml")))
}

fn temporary_skill_path(user_dir: &Path) -> PathBuf {
    user_dir.join(format!(
        ".skill-install-{}-{}.toml",
        std::process::id(),
        epoch_millis_label()
    ))
}

fn installed_from_marker_path(skill_path: &Path) -> PathBuf {
    skill_path.with_extension("installed-from")
}

fn write_installed_from_marker(
    marker: &Path,
    source: &str,
    url: &str,
    checksum: &str,
) -> AppResult<()> {
    std::fs::write(
        marker,
        format!(
            "source = \"{}\"\nurl = \"{}\"\nchecksum = \"{}\"\n",
            toml_escape(source),
            toml_escape(url),
            toml_escape(checksum)
        ),
    )?;
    Ok(())
}

fn read_marker_value(content: &str, key: &str) -> Option<String> {
    content.lines().find_map(|line| {
        let line = line.trim();
        let (found, value) = line.split_once('=')?;
        (found.trim() == key).then(|| value.trim().trim_matches('"').to_string())
    })
}

fn toml_escape(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

fn checksum_hex(content: &str) -> String {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for byte in content.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

fn uninstall_user_skill(config: &AppConfig, name: &str) -> AppResult<String> {
    let repo_dir = resolve_repo_skills_dir();
    let user_dir = expand_tilde(&config.workspace.user_skills_dir);
    if let Some(path) = find_skill_file_in_dir(&user_dir, name)? {
        std::fs::remove_file(&path)?;
        let marker = trusted_skill_marker_path(&path);
        let marker_note = if marker.exists() {
            std::fs::remove_file(&marker)?;
            format!("\nRemoved trust marker: {}", marker.display())
        } else {
            String::new()
        };
        let install_marker = installed_from_marker_path(&path);
        let install_marker_note = if install_marker.exists() {
            std::fs::remove_file(&install_marker)?;
            format!("\nRemoved install marker: {}", install_marker.display())
        } else {
            String::new()
        };
        return Ok(format!(
            "Skill `{name}` uninstalled.\n\nRemoved: {}{marker_note}{install_marker_note}\n\nRun /skills to refresh the list.",
            path.display()
        ));
    }
    if find_skill_file_in_dir(&repo_dir, name)?.is_some() {
        return Ok(format!(
            "Cannot uninstall bundled skill `{name}` from the TUI.\n\nBundled skills live under {}. To override it, add a user skill with the same name in {}.",
            repo_dir.display(),
            user_dir.display()
        ));
    }
    Ok(format!(
        "Skill `{name}` not found in user skills.\n\nUser skills path: {}\nRun /skills to list configured skills.",
        user_dir.display()
    ))
}

fn trust_user_skill(config: &AppConfig, name: &str) -> AppResult<String> {
    let repo_dir = resolve_repo_skills_dir();
    let user_dir = expand_tilde(&config.workspace.user_skills_dir);
    let Some(skill_path) = find_skill_file_in_dir(&user_dir, name)? else {
        if find_skill_file_in_dir(&repo_dir, name)?.is_some() {
            return Ok(format!(
                "Cannot mark bundled skill `{name}` trusted from the TUI.\n\nCreate a user override in {} if you need a writable trust marker.",
                user_dir.display()
            ));
        }
        return Ok(format!(
            "Skill `{name}` not found in user skills.\n\nUser skills path: {}\nRun /skills to list configured skills.",
            user_dir.display()
        ));
    };
    std::fs::create_dir_all(&user_dir)?;
    let marker = trusted_skill_marker_path(&skill_path);
    std::fs::write(
        &marker,
        format!(
            "trusted = true\nskill = \"{}\"\nsource = \"{}\"\n",
            name.replace('"', "\\\""),
            skill_path.display()
        ),
    )?;
    Ok(format!(
        "Skill `{name}` trusted.\n\nSkill file: {}\nTrust marker: {}",
        skill_path.display(),
        marker.display()
    ))
}

fn find_skill_file_in_dir(dir: &Path, name: &str) -> AppResult<Option<PathBuf>> {
    if !dir.exists() {
        return Ok(None);
    }
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("toml") {
            continue;
        }
        let skill = load_skill(&path)?;
        if skill.name == name {
            return Ok(Some(path));
        }
    }
    Ok(None)
}

fn trusted_skill_marker_path(skill_path: &Path) -> PathBuf {
    skill_path.with_extension("trusted")
}

fn configured_skill_for_direct_slash(
    config: &AppConfig,
    command: &str,
    args: &[String],
) -> AppResult<Option<String>> {
    if !args.is_empty() {
        return Ok(None);
    }
    let Some(name) = command.strip_prefix('/') else {
        return Ok(None);
    };
    if name.is_empty() || name.contains('/') || name.contains(char::is_whitespace) {
        return Ok(None);
    }

    let repo_dir = resolve_repo_skills_dir();
    let user_dir = expand_tilde(&config.workspace.user_skills_dir);
    let (registry, _stats) = SkillRegistry::load_dirs(&[repo_dir.as_path(), user_dir.as_path()])?;
    Ok(registry.find(name).map(|skill| skill.name.clone()))
}

fn format_skill_detail(skill: &SkillSpec, searched: &str) -> String {
    let mut detail = format!("# Skill: {}\n\n", skill.name);
    detail.push_str("Description: ");
    detail.push_str(empty_as_placeholder(&skill.description));
    detail.push_str("\n\n");
    push_skill_list(&mut detail, "Allowed tools", &skill.allowed_tools);
    push_skill_list(&mut detail, "Triggers", &skill.triggers);
    push_skill_list(&mut detail, "Suggested steps", &skill.suggested_steps);
    push_skill_list(&mut detail, "References", &skill.references);
    detail.push_str("Policy:\n");
    detail.push_str(&format!(
        "- require_write_confirmation: {}\n",
        skill.policy.require_write_confirmation
    ));
    detail.push_str(&format!(
        "- require_shell_confirmation: {}\n",
        skill.policy.require_shell_confirmation
    ));
    push_skill_list(
        &mut detail,
        "Shell allowlist",
        &skill.policy.shell_allowlist,
    );
    if !skill.system_append.trim().is_empty() {
        detail.push_str("\nSystem append:\n");
        detail.push_str(skill.system_append.trim());
        detail.push('\n');
    }
    detail.push_str("\nSearch paths:\n");
    detail.push_str(if searched.is_empty() {
        "- none"
    } else {
        searched
    });
    detail
}

fn push_skill_list(out: &mut String, label: &str, values: &[String]) {
    out.push_str(label);
    out.push_str(":\n");
    if values.is_empty() {
        out.push_str("- none\n\n");
        return;
    }
    for value in values {
        out.push_str("- ");
        out.push_str(value);
        out.push('\n');
    }
    out.push('\n');
}

fn empty_as_placeholder(value: &str) -> &str {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        "(none)"
    } else {
        trimmed
    }
}

const TUI_SYSTEM_PROMPT_MAX_CHARS: usize = 12_000;

fn format_system_prompt_preview(preview: &SystemPromptPreview, mode: TuiMode) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "DeepSeekCode System Prompt");
    let _ = writeln!(out, "==========================");
    let _ = writeln!(out);
    let _ = writeln!(out, "Mode: {}", tui_mode_label(mode));
    let _ = writeln!(out, "Workspace: {}", preview.workspace.display());
    let _ = writeln!(out, "Profile: {}", preview.profile_name);
    let _ = writeln!(
        out,
        "Task: {}",
        preview
            .task
            .as_deref()
            .unwrap_or("(no selected user message)")
    );
    let _ = writeln!(out, "Planning mode: {}", preview.planning_mode);
    let _ = writeln!(out, "Research bootstrap: {}", preview.research_bootstrap);
    let _ = writeln!(
        out,
        "Skill: {}",
        match (
            preview.skill_name.as_deref(),
            preview.skill_resolution.as_deref(),
        ) {
            (Some(name), Some(resolution)) => format!("{name} ({resolution})"),
            (Some(name), None) => name.to_string(),
            _ => "none".to_string(),
        }
    );
    let _ = writeln!(out, "Available tools: {}", preview.available_tools.len());
    let _ = writeln!(out);
    let _ = writeln!(out, "Workspace Instructions");
    let _ = writeln!(out, "----------------------");
    if preview.workspace_instruction_paths.is_empty() {
        let _ = writeln!(out, "- none");
    } else {
        for path in &preview.workspace_instruction_paths {
            let _ = writeln!(out, "- {}", path.display());
        }
    }
    let _ = writeln!(out);
    let _ = writeln!(out, "User Memory");
    let _ = writeln!(out, "-----------");
    match &preview.user_memory_path {
        Some(path) if preview.user_memory_truncated => {
            let _ = writeln!(out, "{} (truncated)", path.display());
        }
        Some(path) => {
            let _ = writeln!(out, "{}", path.display());
        }
        None => {
            let _ = writeln!(out, "none loaded");
        }
    }
    let _ = writeln!(out);
    let _ = writeln!(out, "Prompt");
    let _ = writeln!(out, "------");
    out.push_str(&truncate_prompt_preview(
        &preview.prompt,
        TUI_SYSTEM_PROMPT_MAX_CHARS,
    ));
    out
}

fn truncate_prompt_preview(value: &str, max_chars: usize) -> String {
    let char_count = value.chars().count();
    if char_count <= max_chars {
        return value.to_string();
    }
    let truncated = value.chars().take(max_chars).collect::<String>();
    format!(
        "{truncated}\n\n[... {} characters omitted from system prompt preview]",
        char_count - max_chars
    )
}

fn tui_mode_label(mode: TuiMode) -> &'static str {
    match mode {
        TuiMode::Plan => "Plan",
        TuiMode::Agent => "Agent",
        TuiMode::Yolo => "YOLO",
    }
}

fn format_network_policy_summary(summary: &NetworkPolicySummary) -> String {
    format!(
        "Network policy ({})\n\nnetwork.default = {}\nnetwork.allow = {}\nnetwork.deny = {}\n\nUse network allow <host>, network deny <host>, network remove <host>, or network default <allow|deny|prompt>.",
        summary.path.display(),
        summary.default,
        format_string_list(&summary.allow),
        format_string_list(&summary.deny)
    )
}

fn format_lsp_summary(summary: &DiagnosticsConfigSummary) -> String {
    format!(
        "DeepSeekCode LSP Diagnostics ({})\n\ndiagnostics.post_edit = {}\n\nUse lsp on, lsp off, or lsp status. Manual diagnostics remain available with diagnostics [--changed|paths...].",
        summary.path.display(),
        summary.post_edit
    )
}

fn format_model_config_summary(summary: &ModelConfigSummary) -> String {
    format!(
        "DeepSeekCode Model Config ({})\n\nmodel.model = {}\nmodel.reasoning_effort = {}\nmodel.base_url = {}\nmodel.api_key_env = {}\n\nUse model to open the picker, model show to inspect this config, model <name> to update model.model, or models for the offline catalog.",
        summary.path.display(),
        summary.model,
        summary.reasoning_effort,
        summary.base_url,
        summary.api_key_env
    )
}

fn format_model_catalog_summary(summary: &ModelConfigSummary) -> String {
    let models = [
        "auto",
        "deepseek-v4-flash",
        "deepseek-v4-pro",
        "deepseek-chat",
        "deepseek-reasoner",
        "deepseek-coder",
    ];
    let mut out = String::new();
    out.push_str("DeepSeekCode Model Catalog\n");
    out.push_str("==========================\n\n");
    out.push_str(&format!("Current project model: {}\n", summary.model));
    out.push_str(&format!("Reasoning effort: {}\n", summary.reasoning_effort));
    out.push_str("\nKnown local model ids:\n");
    for model in models {
        if model == summary.model {
            out.push_str(&format!("- {model} (current)\n"));
        } else {
            out.push_str(&format!("- {model}\n"));
        }
    }
    out.push_str(
        "\nThis list is local and offline. DeepSeek-TUI-style online /models fetching remains a separate runtime/API-backed gap.\n",
    );
    out
}

fn format_provider_config_summary(summary: &ProviderConfigSummary) -> String {
    format!(
        "DeepSeekCode Provider Config ({})\n\nprovider = {} ({})\nmodel.base_url = {}\nmodel.api_key_env = {}\nmodel.model = {}\nmodel.reasoning_effort = {}\n\nUse provider to open the picker, provider <name> [model] to update this project config, or provider list for supported presets.",
        summary.path.display(),
        summary.provider,
        summary.label,
        summary.base_url,
        summary.api_key_env,
        summary.model,
        summary.reasoning_effort
    )
}

fn format_provider_catalog_summary(summary: &ProviderConfigSummary) -> String {
    let providers = [
        ("deepseek", "DeepSeek"),
        ("nvidia-nim", "NVIDIA NIM"),
        ("openai", "OpenAI-compatible"),
        ("atlascloud", "AtlasCloud"),
        ("openrouter", "OpenRouter"),
        ("novita", "Novita AI"),
        ("fireworks", "Fireworks AI"),
        ("sglang", "SGLang local"),
        ("vllm", "vLLM local"),
        ("ollama", "Ollama local"),
    ];
    let mut out = String::new();
    out.push_str("DeepSeekCode Provider Presets\n");
    out.push_str("=============================\n\n");
    out.push_str(&format!(
        "Current provider: {} ({})\n",
        summary.provider, summary.label
    ));
    out.push_str(&format!("Current model: {}\n\n", summary.model));
    for (name, label) in providers {
        if name == summary.provider {
            out.push_str(&format!("- {name}: {label} (current)\n"));
        } else {
            out.push_str(&format!("- {name}: {label}\n"));
        }
    }
    out.push_str(
        "\nUse /provider to open the interactive picker, or provider <name> [model] to update model.base_url/api_key_env/model.model directly.\n",
    );
    out
}

fn format_profile_config_summary(summary: &ProfileConfigSummary) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "DeepSeekCode Profiles ({})\n",
        summary.path.display()
    ));
    out.push_str("=====================\n\n");
    out.push_str(&format!(
        "Active profile: {}\n",
        summary.active_profile.as_deref().unwrap_or("(none)")
    ));
    out.push_str("\nProfiles:\n");
    if summary.profiles.is_empty() {
        out.push_str("- none configured\n");
    } else {
        for profile in &summary.profiles {
            let marker = if summary.active_profile.as_deref() == Some(profile.name.as_str()) {
                " (active)"
            } else {
                ""
            };
            out.push_str(&format!("- {}{}\n", profile.name, marker));
            for (key, value) in profile.keys.iter().take(6) {
                out.push_str(&format!("  {key} = {value}\n"));
            }
            if profile.keys.len() > 6 {
                out.push_str(&format!(
                    "  ... {} more setting(s)\n",
                    profile.keys.len() - 6
                ));
            }
        }
    }
    out.push_str(
        "\nUse profile <name> to persist workspace.active_profile. Future local TUI turns reload this profile before env overrides.\n",
    );
    out
}

fn format_workspace_trust_summary(workspace: &Path, trust: &WorkspaceTrust) -> String {
    let mut out = String::new();
    out.push_str("DeepSeekCode Workspace Trust\n");
    out.push_str("============================\n\n");
    out.push_str(&format!("Workspace: {}\n", workspace.display()));
    out.push_str(&format!("Trust file: {}\n", render_trust_file_hint()));
    out.push_str(&format!(
        "Workspace trust mode: {}\n",
        if trust.trust_mode() {
            "enabled"
        } else {
            "disabled"
        }
    ));
    out.push_str("\nTrusted external paths:\n");
    if trust.paths().is_empty() {
        out.push_str("- none\n");
    } else {
        for path in trust.paths() {
            out.push_str(&format!("- {}\n", path.display()));
        }
    }
    out.push_str(
        "\nCommands:\n- trust on/off toggles all-path trust for this workspace.\n- trust add <path> adds an existing external path.\n- trust remove <path> removes one trusted path.\n- trust list shows only this workspace's trust state.\n",
    );
    out
}

fn format_logout_summary(summary: &LogoutCredentialSummary) -> String {
    let mut out = String::new();
    out.push_str("DeepSeekCode Logout\n");
    out.push_str("===================\n\n");
    out.push_str(&format!("Workspace: {}\n", summary.workspace.display()));
    out.push_str(&format!(".env: {}\n\n", summary.dotenv_path.display()));
    out.push_str("Current process environment:\n");
    for entry in &summary.env_vars {
        out.push_str(&format!(
            "- {}: {}\n",
            entry.name,
            if entry.was_present {
                "cleared"
            } else {
                "not set"
            }
        ));
    }
    out.push_str("\n.env assignments:\n");
    if summary.dotenv_removed.is_empty() {
        out.push_str("- none removed\n");
    } else {
        for key in &summary.dotenv_removed {
            out.push_str(&format!("- {key}: removed\n"));
        }
    }
    out.push_str(
        "\nThis only affects the current TUI process and the selected workspace .env file. It cannot unset variables already exported in the parent shell.\n",
    );
    out
}

fn format_string_list(values: &[String]) -> String {
    if values.is_empty() {
        "[]".to_string()
    } else {
        format!("[{}]", values.join(", "))
    }
}

fn mcp_manager_summary(config: &AppConfig) -> AppResult<String> {
    let mut output = String::new();
    output.push_str("MCP Manager\n");
    output.push_str(&mcp_status_summary(config)?);
    output.push_str("\n\n");
    output.push_str(&list_servers_summary(config)?);
    output.push_str("\nDiscovery refresh\n");
    match validate_servers_summary(config) {
        Ok(summary) => output.push_str(&summary),
        Err(error) => output.push_str(&format!("MCP discovery refresh failed: {error}\n")),
    }
    output.push_str("\nAvailable actions:\n");
    output.push_str("- mcp init [--force]\n");
    output.push_str("- mcp add stdio <name> <command> [args...]\n");
    output.push_str("- mcp add http|sse <name> <url>\n");
    output.push_str("- mcp enable|disable|remove <name>\n");
    output.push_str("- mcp user add|enable|disable|remove ...\n");
    output.push_str("- mcp tools|prompts|resources|resource-templates [server]\n");
    output.push_str("- mcp validate\n");
    output.push_str("- mcp close\n");
    output.push_str(
        "\nManager keys: n/p select server, e enable, d disable, x remove (confirm), t tools, r reload\n",
    );
    Ok(output)
}

fn tui_mcp_config_scope(scope: TuiMcpConfigScope) -> McpConfigScope {
    match scope {
        TuiMcpConfigScope::Project => McpConfigScope::Project,
        TuiMcpConfigScope::User => McpConfigScope::User,
    }
}

fn handle_tui_action_with_live(
    store: &RuntimeStore,
    config: Option<&AppConfig>,
    app: &mut TuiApp,
    action: TuiAction,
    live_tx: Option<Sender<TuiLiveEvent>>,
) -> AppResult<()> {
    match action {
        TuiAction::SubmitUserMessage { thread_id, content } => {
            run_tui_submit_user_message(store, config, app, thread_id, content, live_tx)?;
        }
        TuiAction::UndoConversation { thread_id } => {
            match run_tui_undo_conversation(store, app, &thread_id, "Undo") {
                Ok(fork) => {
                    app.set_status(format!(
                        "undid latest exchange: active thread {}",
                        fork.thread.id
                    ));
                }
                Err(error) => {
                    app.set_status(format!("undo failed: {error}"));
                }
            }
        }
        TuiAction::RetryUserMessage { thread_id } => {
            match latest_user_turn_content(store, &thread_id).and_then(|last_user| {
                run_tui_undo_conversation(store, app, &thread_id, "Retry")
                    .map(|fork| (fork, last_user))
            }) {
                Ok((fork, last_user)) => {
                    run_tui_submit_user_message(
                        store,
                        config,
                        app,
                        fork.thread.id.clone(),
                        last_user.clone(),
                        live_tx,
                    )?;
                    app.set_status(format!(
                        "retrying on {}: {}",
                        fork.thread.id,
                        runtime_summary(&last_user)
                    ));
                }
                Err(error) => {
                    app.set_status(format!("retry failed: {error}"));
                }
            }
        }
        TuiAction::SubmitEditedUserMessage { thread_id, content } => {
            match run_tui_undo_conversation(store, app, &thread_id, "Edit") {
                Ok(fork) => {
                    run_tui_submit_user_message(
                        store,
                        config,
                        app,
                        fork.thread.id.clone(),
                        content.clone(),
                        live_tx,
                    )?;
                    app.set_status(format!(
                        "submitted edited message on {}: {}",
                        fork.thread.id,
                        runtime_summary(&content)
                    ));
                }
                Err(error) => {
                    app.set_status(format!("edit submit failed: {error}"));
                }
            }
        }
        TuiAction::RecallArchive {
            workspace,
            thread_id,
            query,
        } => {
            run_tui_recall_archive(app, config, &workspace, thread_id.as_deref(), &query);
        }
        TuiAction::ReviewTarget { workspace, target } => {
            run_tui_review_target(app, &workspace, &target);
        }
        TuiAction::RunCustomSlashCommand {
            thread_id,
            command,
            args,
        } => {
            let Some(config) = config else {
                app.set_status("custom slash commands require local config".to_string());
                return Ok(());
            };
            let arg_refs = args.iter().map(String::as_str).collect::<Vec<_>>();
            let Some(content) = load_custom_slash_command_from_config(config, &command, &arg_refs)?
            else {
                if let Some(name) = configured_skill_for_direct_slash(config, &command, &args)? {
                    let skill_command = TuiSkillsCommand::Show { name: name.clone() };
                    let detail = format_skills_summary(config, &skill_command)?;
                    app.set_mcp_detail(TuiMcpDetailKind::Skills, detail);
                    app.set_status(format!("skill shown: {name}"));
                    return Ok(());
                }
                app.set_status(format!("custom slash command not found: {command}"));
                return Ok(());
            };
            let turn = store.append_turn(&thread_id, "user".to_string(), content.clone())?;
            store.append_item(
                &thread_id,
                Some(&turn.id),
                "message".to_string(),
                Some("user".to_string()),
                content.clone(),
                "completed".to_string(),
            )?;
            let run_config = load_or_default().unwrap_or_else(|_| config.clone());
            start_tui_agent_run(
                store.clone(),
                run_config,
                thread_id.clone(),
                content,
                app.reasoning_replay_limit(),
                app.reasoning_replay_pinned_turn_ids(),
                app.translation_target_language_for_agent(),
                live_tx,
            );
            app.set_status(format!(
                "started custom slash command {command} for {thread_id}"
            ));
        }
        TuiAction::RenameSession { session_id, title } => {
            let session = store.rename_session(&session_id, title.clone())?;
            app.rename_session_title(&session_id, session.title.clone());
            app.set_status(format!("renamed session {session_id}: {}", session.title));
        }
        TuiAction::InitProjectInstructions { workspace } => {
            let path = init_project_instructions_at(Path::new(&workspace))?;
            app.set_status(format!("created project instructions: {}", path.display()));
        }
        TuiAction::Network { workspace, command } => {
            let workspace = Path::new(&workspace);
            let status = match command {
                TuiNetworkCommand::List => "network policy listed".to_string(),
                TuiNetworkCommand::Allow { host } => {
                    let result = set_network_rule_at(workspace, &host, NetworkRuleTarget::Allow)?;
                    if result.changed {
                        format!("network host allowed: {}", result.host)
                    } else {
                        format!("network host already allowed: {}", result.host)
                    }
                }
                TuiNetworkCommand::Deny { host } => {
                    let result = set_network_rule_at(workspace, &host, NetworkRuleTarget::Deny)?;
                    if result.changed {
                        format!("network host denied: {}", result.host)
                    } else {
                        format!("network host already denied: {}", result.host)
                    }
                }
                TuiNetworkCommand::Remove { host } => {
                    let result = remove_network_rule_at(workspace, &host)?;
                    if result.changed {
                        format!("network host removed: {}", result.host)
                    } else {
                        format!("network host not present: {}", result.host)
                    }
                }
                TuiNetworkCommand::Default { value } => {
                    let result = set_network_default_at(workspace, &value)?;
                    if result.changed {
                        format!("network default set: {}", result.value)
                    } else {
                        format!("network default unchanged: {}", result.value)
                    }
                }
            };
            let summary = network_policy_summary_at(workspace)?;
            app.set_mcp_detail(
                TuiMcpDetailKind::Network,
                format_network_policy_summary(&summary),
            );
            app.set_status(status);
        }
        TuiAction::Lsp { workspace, command } => {
            let workspace = Path::new(&workspace);
            let status = match command {
                TuiLspCommand::Status => "lsp diagnostics status shown".to_string(),
                TuiLspCommand::Set { enabled } => {
                    let result = set_diagnostics_post_edit_at(workspace, enabled)?;
                    if result.changed {
                        format!(
                            "lsp diagnostics {}",
                            if result.value { "enabled" } else { "disabled" }
                        )
                    } else {
                        format!(
                            "lsp diagnostics already {}",
                            if result.value { "enabled" } else { "disabled" }
                        )
                    }
                }
                TuiLspCommand::Help => "lsp help shown".to_string(),
            };
            let summary = diagnostics_config_summary_at(workspace)?;
            app.set_mcp_detail(TuiMcpDetailKind::Lsp, format_lsp_summary(&summary));
            app.set_status(status);
        }
        TuiAction::ShowSystemPrompt {
            workspace,
            mode,
            task,
        } => {
            let Some(config) = config else {
                app.set_status("system prompt preview requires local config".to_string());
                return Ok(());
            };
            let preview = preview_system_prompt_for_workspace(
                config,
                Path::new(&workspace),
                task.as_deref(),
                false,
                0,
            )?;
            app.set_mcp_detail(
                TuiMcpDetailKind::System,
                format_system_prompt_preview(&preview, mode),
            );
            app.set_status("system prompt shown".to_string());
        }
        TuiAction::Model { workspace, command } => {
            let workspace = Path::new(&workspace);
            let completes_setup_step = matches!(&command, TuiModelCommand::Set { .. });
            let status = match &command {
                TuiModelCommand::Pick => "model picker shown".to_string(),
                TuiModelCommand::Show => "model config shown".to_string(),
                TuiModelCommand::List => "model catalog shown".to_string(),
                TuiModelCommand::Set { model } => {
                    let result = set_model_at(workspace, model)?;
                    if result.changed {
                        format!("model set: {} -> {}", result.previous, result.model)
                    } else {
                        format!("model unchanged: {}", result.model)
                    }
                }
            };
            let setup_status = status.clone();
            let summary = model_config_summary_at(workspace)?;
            let detail = match command {
                TuiModelCommand::Pick | TuiModelCommand::List => {
                    format_model_catalog_summary(&summary)
                }
                TuiModelCommand::Show | TuiModelCommand::Set { .. } => {
                    format_model_config_summary(&summary)
                }
            };
            app.set_mcp_detail(TuiMcpDetailKind::Model, detail);
            app.set_status(status);
            if completes_setup_step {
                app.complete_setup_wizard_active_step("model", &["model"], &setup_status);
            }
        }
        TuiAction::Provider { workspace, command } => {
            let workspace = Path::new(&workspace);
            let completes_setup_step = matches!(&command, TuiProviderCommand::Set { .. });
            let status = match &command {
                TuiProviderCommand::Pick => "provider picker shown".to_string(),
                TuiProviderCommand::Show => "provider config shown".to_string(),
                TuiProviderCommand::List => "provider catalog shown".to_string(),
                TuiProviderCommand::Set { provider, model } => {
                    let result = set_provider_at(workspace, provider, model.as_deref())?;
                    if result.changed {
                        format!(
                            "provider set: {} -> {} ({})",
                            result.previous_provider, result.provider, result.model
                        )
                    } else {
                        format!("provider unchanged: {} ({})", result.provider, result.model)
                    }
                }
            };
            let setup_status = status.clone();
            let summary = provider_config_summary_at(workspace)?;
            let detail = match command {
                TuiProviderCommand::Pick | TuiProviderCommand::List => {
                    format_provider_catalog_summary(&summary)
                }
                TuiProviderCommand::Show | TuiProviderCommand::Set { .. } => {
                    format_provider_config_summary(&summary)
                }
            };
            app.set_mcp_detail(TuiMcpDetailKind::Provider, detail);
            app.set_status(status);
            if completes_setup_step {
                app.complete_setup_wizard_active_step(
                    "provider",
                    &["provider", "model"],
                    &setup_status,
                );
            }
        }
        TuiAction::Profile { workspace, command } => {
            let workspace = Path::new(&workspace);
            let status = match &command {
                TuiProfileCommand::Show => "profile config shown".to_string(),
                TuiProfileCommand::List => "profiles listed".to_string(),
                TuiProfileCommand::Clear => {
                    let result = switch_profile_at(workspace, None)?;
                    if result.changed {
                        match result.previous {
                            Some(previous) => format!("profile cleared: {previous}"),
                            None => "profile cleared".to_string(),
                        }
                    } else {
                        "profile already clear".to_string()
                    }
                }
                TuiProfileCommand::Switch { profile } => {
                    let result = switch_profile_at(workspace, Some(profile))?;
                    if result.changed {
                        format!("profile switched: {profile}")
                    } else {
                        format!("profile unchanged: {profile}")
                    }
                }
            };
            let summary = profile_config_summary_at(workspace)?;
            app.set_mcp_detail(
                TuiMcpDetailKind::Profile,
                format_profile_config_summary(&summary),
            );
            app.set_status(status);
        }
        TuiAction::Trust { workspace, command } => {
            let workspace = Path::new(&workspace);
            let status = match &command {
                TuiTrustCommand::Show => "workspace trust shown".to_string(),
                TuiTrustCommand::List => "trusted paths listed".to_string(),
                TuiTrustCommand::SetMode { enabled } => {
                    let changed = set_trust_mode(workspace, *enabled)?;
                    match (*enabled, changed) {
                        (true, true) => "workspace trust mode enabled".to_string(),
                        (true, false) => "workspace trust mode already enabled".to_string(),
                        (false, true) => "workspace trust mode disabled".to_string(),
                        (false, false) => "workspace trust mode already disabled".to_string(),
                    }
                }
                TuiTrustCommand::Add { path } => {
                    let target = resolve_trust_command_path(workspace, path);
                    if !target.exists() {
                        return Err(app_error(format!(
                            "trust path not found: {}",
                            target.display()
                        )));
                    }
                    let stored = add_workspace_trust_path(workspace, &target)?;
                    format!("trusted path added: {}", stored.display())
                }
                TuiTrustCommand::Remove { path } => {
                    let target = resolve_trust_command_path(workspace, path);
                    if remove_workspace_trust_path(workspace, &target)? {
                        format!("trusted path removed: {}", target.display())
                    } else {
                        format!("trusted path not present: {}", target.display())
                    }
                }
            };
            let trust = WorkspaceTrust::load_for(workspace);
            app.set_mcp_detail(
                TuiMcpDetailKind::Trust,
                format_workspace_trust_summary(workspace, &trust),
            );
            app.set_status(status);
            app.complete_setup_wizard_active_step("trust", &["trust"], "workspace trust shown");
        }
        TuiAction::Logout { workspace } => {
            let summary = logout_credentials_at(Path::new(&workspace))?;
            let cleared = summary
                .env_vars
                .iter()
                .filter(|entry| entry.was_present)
                .map(|entry| entry.name.as_str())
                .chain(summary.dotenv_removed.iter().map(String::as_str))
                .collect::<std::collections::BTreeSet<_>>();
            let status = if summary.changed() {
                format!(
                    "logged out: cleared {}",
                    cleared.into_iter().collect::<Vec<_>>().join(", ")
                )
            } else {
                "logout: no local API key state found".to_string()
            };
            app.set_mcp_detail(TuiMcpDetailKind::Logout, format_logout_summary(&summary));
            app.set_status(status);
        }
        TuiAction::AuthCredential {
            workspace,
            env_name,
            secret,
        } => {
            let result =
                persist_auth_secret_at(Path::new(&workspace), &env_name, secret.expose_secret())?;
            let mut detail = String::new();
            detail.push_str("DeepSeekCode Auth\n");
            detail.push_str("=================\n\n");
            detail.push_str(&format!("Workspace: {workspace}\n"));
            detail.push_str(&format!("Env var: {}\n", result.env_name));
            detail.push_str(&format!(".env: {}\n", result.dotenv_path.display()));
            detail.push_str("Value: present (hidden)\n");
            detail.push_str("\nNext commands:\n");
            detail.push_str("- deepseek doctor\n");
            detail.push_str("- deepseek smoke\n");
            let status = if result.changed {
                format!("auth credential stored: {}", result.env_name)
            } else {
                format!("auth credential unchanged: {}", result.env_name)
            };
            app.set_mcp_detail(TuiMcpDetailKind::Setup, detail);
            let setup_status = status.clone();
            app.set_status(status);
            app.complete_setup_wizard_active_step("auth", &["auth"], &setup_status);
        }
        TuiAction::Skills { command } => {
            let fallback_config;
            let config = match config {
                Some(config) => config,
                None => {
                    fallback_config = AppConfig::default();
                    &fallback_config
                }
            };
            let detail = format_skills_summary(config, &command)?;
            app.set_mcp_detail(TuiMcpDetailKind::Skills, detail);
            app.set_status(match command {
                TuiSkillsCommand::List {
                    prefix: Some(prefix),
                } => {
                    format!("skills listed with prefix: {prefix}")
                }
                TuiSkillsCommand::List { prefix: None } => "skills listed".to_string(),
                TuiSkillsCommand::Remote => "remote skills listed".to_string(),
                TuiSkillsCommand::Sync => "remote skills synced".to_string(),
                TuiSkillsCommand::Show { name } => format!("skill shown: {name}"),
                TuiSkillsCommand::Install { source } => {
                    format!("skill install processed: {source}")
                }
                TuiSkillsCommand::Update { name } => format!("skill update processed: {name}"),
                TuiSkillsCommand::Uninstall { name } => {
                    format!("skill uninstall processed: {name}")
                }
                TuiSkillsCommand::Trust { name } => format!("skill trust processed: {name}"),
            });
        }
        TuiAction::RespondApproval {
            thread_id,
            turn_id,
            request_id,
            decision,
            scope,
        } => {
            store.append_permission_response_with_scope(
                &thread_id,
                turn_id.as_deref(),
                request_id.clone(),
                decision.clone(),
                scope,
            )?;
            app.set_status(format!(
                "recorded approval response: {request_id} {decision}"
            ));
        }
        TuiAction::RespondUserInput {
            thread_id,
            turn_id,
            request_id,
            answers,
        } => {
            store.append_user_input_response(
                &thread_id,
                turn_id.as_deref(),
                request_id.clone(),
                answers,
            )?;
            app.set_status(format!("recorded user input response: {request_id}"));
        }
        TuiAction::CancelRun { thread_id, turn_id } => {
            store.append_cancel_request(
                &thread_id,
                turn_id.as_deref(),
                None,
                "user requested cancellation from TUI".to_string(),
            )?;
            app.set_status(format!("cancel event recorded for {thread_id}"));
        }
        TuiAction::CreateTask { thread_id, summary } => {
            let thread = store.load_thread(&thread_id)?;
            let task = store.create_task(
                thread.session_id.as_deref(),
                Some(&thread_id),
                None,
                "agent".to_string(),
                "pending".to_string(),
                summary,
            )?;
            app.set_status(format!("created pending task {}", task.id));
        }
        TuiAction::CreateSubagentTask {
            thread_id,
            task,
            max_depth,
        } => {
            let task = run_tui_create_subagent_task(store, &thread_id, max_depth, &task)?;
            app.set_status(format!(
                "created pending subagent task {} (depth={max_depth})",
                task.id
            ));
        }
        TuiAction::PauseTask { task_id } => match store.pause_task(&task_id, None) {
            Ok(task) => {
                app.set_status(format!("paused task {}", task.id));
            }
            Err(error) => {
                app.set_status(format!("task pause failed for {task_id}: {error}"));
            }
        },
        TuiAction::ResumeTask { task_id } => match store.resume_task(&task_id, None) {
            Ok(task) => {
                app.set_status(format!("resumed task {}", task.id));
            }
            Err(error) => {
                app.set_status(format!("task resume failed for {task_id}: {error}"));
            }
        },
        TuiAction::CancelTask { task_id } => {
            match store.cancel_task(&task_id, "cancelled from TUI task panel".to_string()) {
                Ok((task, _)) => {
                    app.set_status(format!("cancelled task {}", task.id));
                }
                Err(error) => {
                    app.set_status(format!("task cancel failed for {task_id}: {error}"));
                }
            }
        }
        TuiAction::RunDiagnostics { changed, paths } => {
            run_tui_diagnostics_from_current_dir(app, changed, paths);
        }
        TuiAction::RunShell { command } => {
            run_tui_shell_command(app, &command);
        }
        TuiAction::RunApprovedShell { command } => {
            run_tui_approved_shell_command(app, &command);
        }
        TuiAction::ListShell => {
            run_tui_shell_list(app);
        }
        TuiAction::ShowShell { task_id } => {
            run_tui_shell_show(app, &task_id);
        }
        TuiAction::AttachShell {
            task_id,
            cursor,
            tail,
        } => {
            run_tui_shell_attach(app, &task_id, cursor, tail);
        }
        TuiAction::ShellSupervisorStatus => {
            run_tui_shell_supervisor_status(app);
        }
        TuiAction::SendShellStdin {
            task_id,
            input,
            close,
        } => {
            run_tui_shell_stdin(app, &task_id, &input, close);
        }
        TuiAction::WaitShell {
            task_id,
            wait,
            timeout_ms,
        } => {
            run_tui_shell_wait(app, &task_id, wait, timeout_ms);
        }
        TuiAction::ResizeShell {
            task_id,
            rows,
            cols,
        } => {
            run_tui_shell_resize(app, &task_id, rows, cols);
        }
        TuiAction::CancelShell { task_id, all } => {
            run_tui_shell_cancel(app, task_id.as_deref(), all);
        }
        TuiAction::AppendMemory { note } => {
            run_tui_memory_append(app, config, &note);
        }
        TuiAction::Memory { command } => {
            run_tui_memory_command(app, config, command);
        }
        TuiAction::Note { command } => {
            run_tui_note_command(app, config, command);
        }
        TuiAction::Anchor { workspace, command } => {
            run_tui_anchor_command(app, Path::new(&workspace), command);
        }
        TuiAction::ShareSession { thread_id } => {
            run_tui_share_session(store, app, &thread_id)?;
        }
        TuiAction::ExportThread { thread_id, path } => {
            run_tui_export_thread(store, app, &thread_id, path.as_deref())?;
        }
        TuiAction::SaveSession {
            session_id,
            thread_id,
            path,
        } => {
            run_tui_save_session(store, app, &session_id, &thread_id, path.as_deref())?;
        }
        TuiAction::PruneSessions { days } => {
            run_tui_prune_sessions(store, app, days)?;
        }
        TuiAction::LoadSession { workspace, path } => {
            run_tui_load_session(store, app, &workspace, &path)?;
        }
        TuiAction::ClearConversation {
            session_id,
            previous_thread_id,
        } => {
            run_tui_clear_conversation(
                store,
                config,
                app,
                &session_id,
                previous_thread_id.as_deref(),
            )?;
        }
        TuiAction::ShowDiff { workspace } => {
            run_tui_diff_command(app, Path::new(&workspace));
        }
        TuiAction::Hooks { command } => {
            run_tui_hooks_command(app, config, command);
        }
        TuiAction::McpManager => match config {
            Some(config) => match mcp_manager_summary(config) {
                Ok(summary) => {
                    app.set_status(mcp_status_summary(config)?);
                    app.set_mcp_manager(summary);
                }
                Err(error) => app.set_status(format!("mcp manager failed: {error}")),
            },
            None => app.set_status("mcp commands require local config".to_string()),
        },
        TuiAction::McpList => match config {
            Some(config) => match mcp_status_summary(config) {
                Ok(summary) => app.set_status(summary),
                Err(error) => app.set_status(format!("mcp list failed: {error}")),
            },
            None => app.set_status("mcp commands require local config".to_string()),
        },
        TuiAction::McpDetails { kind, server } => match config {
            Some(config) => match mcp_detail_summary(config, &kind, server.as_deref()) {
                Ok(summary) => {
                    app.set_status(last_nonempty_line(&summary, "mcp detail: ok"));
                    app.set_mcp_detail(kind, summary);
                }
                Err(error) => {
                    app.set_status(format!("mcp {} failed: {error}", kind.command_name()))
                }
            },
            None => app.set_status("mcp commands require local config".to_string()),
        },
        TuiAction::McpManagerDetails { kind, server } => match config {
            Some(config) => match mcp_detail_summary(config, &kind, server.as_deref()) {
                Ok(summary) => {
                    app.set_status(last_nonempty_line(&summary, "mcp detail: ok"));
                    app.set_mcp_manager_detail(kind, summary);
                }
                Err(error) => {
                    app.set_status(format!("mcp {} failed: {error}", kind.command_name()))
                }
            },
            None => app.set_status("mcp commands require local config".to_string()),
        },
        TuiAction::McpInit { force } => {
            let Some(config) = config else {
                app.set_status("mcp commands require local config".to_string());
                return Ok(());
            };
            match std::env::current_dir()
                .map_err(|error| app_error(format!("failed to read current directory: {error}")))
                .and_then(|root| init_mcp_config_at(&root, config, force))
            {
                Ok(path) => app.set_status(format!(
                    "mcp project config initialized: {}",
                    path.display()
                )),
                Err(error) => app.set_status(format!("mcp init failed: {error}")),
            }
        }
        TuiAction::McpAddStdio {
            scope,
            name,
            command,
            args,
        } => {
            let Some(config) = config else {
                app.set_status("mcp commands require local config".to_string());
                return Ok(());
            };
            let config_scope = tui_mcp_config_scope(scope);
            let spec = McpServerConfigSpec::stdio(name.clone(), command, args, false);
            match std::env::current_dir()
                .map_err(|error| app_error(format!("failed to read current directory: {error}")))
                .and_then(|root| add_mcp_server_at(&root, config, config_scope, spec))
            {
                Ok(path) => app.set_status(format!(
                    "mcp {} stdio server added: {name} ({})",
                    scope.label(),
                    path.display()
                )),
                Err(error) => app.set_status(format!("mcp add failed for {name}: {error}")),
            }
        }
        TuiAction::McpAddRemote {
            scope,
            name,
            transport,
            url,
        } => {
            let Some(config) = config else {
                app.set_status("mcp commands require local config".to_string());
                return Ok(());
            };
            let config_scope = tui_mcp_config_scope(scope);
            let spec = McpServerConfigSpec::remote(name.clone(), transport.clone(), url, false);
            match std::env::current_dir()
                .map_err(|error| app_error(format!("failed to read current directory: {error}")))
                .and_then(|root| add_mcp_server_at(&root, config, config_scope, spec))
            {
                Ok(path) => app.set_status(format!(
                    "mcp {} {transport} server added: {name} ({})",
                    scope.label(),
                    path.display()
                )),
                Err(error) => app.set_status(format!("mcp add failed for {name}: {error}")),
            }
        }
        TuiAction::McpRemove { scope, name } => {
            let Some(config) = config else {
                app.set_status("mcp commands require local config".to_string());
                return Ok(());
            };
            let config_scope = tui_mcp_config_scope(scope);
            match std::env::current_dir()
                .map_err(|error| app_error(format!("failed to read current directory: {error}")))
                .and_then(|root| remove_mcp_server_at(&root, config, config_scope, &name))
            {
                Ok(path) => app.set_status(format!(
                    "mcp {} server removed: {name} ({})",
                    scope.label(),
                    path.display()
                )),
                Err(error) => app.set_status(format!("mcp remove failed for {name}: {error}")),
            }
        }
        TuiAction::McpSetEnabled {
            scope,
            name,
            enabled,
        } => {
            let Some(config) = config else {
                app.set_status("mcp commands require local config".to_string());
                return Ok(());
            };
            let config_scope = tui_mcp_config_scope(scope);
            match std::env::current_dir()
                .map_err(|error| app_error(format!("failed to read current directory: {error}")))
                .and_then(|root| {
                    set_mcp_server_enabled_at(&root, config, config_scope, &name, enabled)
                }) {
                Ok(path) => {
                    let action = if enabled { "enabled" } else { "disabled" };
                    app.set_status(format!(
                        "mcp {} server {action}: {name} ({})",
                        scope.label(),
                        path.display()
                    ))
                }
                Err(error) => app.set_status(format!("mcp update failed for {name}: {error}")),
            }
        }
        TuiAction::McpValidate => match config {
            Some(config) => match validate_servers_summary(config) {
                Ok(summary) => {
                    app.set_status(last_nonempty_line(&summary, "mcp validate: ok"));
                    app.set_mcp_detail(TuiMcpDetailKind::Health, summary);
                }
                Err(error) => app.set_status(format!("mcp validate failed: {error}")),
            },
            None => app.set_status("mcp commands require local config".to_string()),
        },
        TuiAction::CreateRollbackSnapshot { label } => {
            let Some(rollback_store) = rollback_store_for_config(config, app) else {
                return Ok(());
            };
            match std::env::current_dir()
                .map_err(|error| app_error(format!("failed to read current directory: {error}")))
                .and_then(|workspace| {
                    rollback_store.create_snapshot(
                        &workspace,
                        label.unwrap_or_else(|| "manual TUI snapshot".to_string()),
                    )
                }) {
                Ok(snapshot) => {
                    app.set_status(format!(
                        "created rollback snapshot {} (patch_bytes={}, untracked_entries={})",
                        snapshot.id,
                        snapshot.patch_bytes,
                        snapshot.untracked_entry_count()
                    ));
                    app.set_mcp_detail(
                        TuiMcpDetailKind::Rollback,
                        render_rollback_snapshot_detail(&snapshot, None),
                    );
                }
                Err(error) => {
                    app.set_status(format!("rollback snapshot failed: {error}"));
                }
            }
        }
        TuiAction::ListRollbackSnapshots { limit } => {
            let Some(rollback_store) = rollback_store_for_config(config, app) else {
                return Ok(());
            };
            match rollback_store.list_snapshots(limit) {
                Ok(snapshots) if snapshots.is_empty() => {
                    app.set_status("no rollback snapshots".to_string());
                    app.set_mcp_detail(
                        TuiMcpDetailKind::Rollback,
                        "Rollback snapshots\n\nNo local rollback snapshots found.",
                    );
                }
                Ok(snapshots) => {
                    let latest = &snapshots[0];
                    app.set_status(format!(
                        "rollback snapshots={} latest={} turn={} {}",
                        snapshots.len(),
                        latest.id,
                        latest.runtime_turn_id.as_deref().unwrap_or("-"),
                        runtime_summary(&latest.label)
                    ));
                    app.set_mcp_detail(
                        TuiMcpDetailKind::Rollback,
                        render_rollback_snapshot_list(&snapshots),
                    );
                }
                Err(error) => {
                    app.set_status(format!("rollback list failed: {error}"));
                }
            }
        }
        TuiAction::ShowRollbackSnapshot { id } => {
            let Some(rollback_store) = rollback_store_for_config(config, app) else {
                return Ok(());
            };
            match rollback_store.load_snapshot_or_turn(&id) {
                Ok(snapshot) => {
                    let patch = rollback_store.snapshot_patch(&snapshot.id).ok();
                    app.set_status(format!(
                        "rollback snapshot {} patch={} staged={} unstaged={} untracked_entries={}",
                        snapshot.id,
                        snapshot.patch_bytes,
                        snapshot.staged_patch_bytes,
                        snapshot.unstaged_patch_bytes,
                        snapshot.untracked_entry_count()
                    ));
                    app.set_mcp_detail(
                        TuiMcpDetailKind::Rollback,
                        render_rollback_snapshot_detail(&snapshot, patch.as_deref()),
                    );
                }
                Err(error) => {
                    app.set_status(format!("rollback show failed for {id}: {error}"));
                }
            }
        }
        TuiAction::ShowRollbackHunk { id, hunk } => {
            let Some(rollback_store) = rollback_store_for_config(config, app) else {
                return Ok(());
            };
            match rollback_store.load_snapshot_or_turn(&id) {
                Ok(snapshot) => match rollback_store.snapshot_patch(&snapshot.id) {
                    Ok(patch) => {
                        let hunks = parse_rollback_patch_hunks(&patch);
                        let detail = render_rollback_hunk_detail(&snapshot, &patch, hunk);
                        app.set_mcp_detail(TuiMcpDetailKind::Rollback, detail);
                        app.set_status(match hunk {
                            Some(index) => format!(
                                "rollback hunk {} for {} (hunks={})",
                                index,
                                snapshot.id,
                                hunks.len()
                            ),
                            None => format!(
                                "rollback hunks for {} (hunks={})",
                                snapshot.id,
                                hunks.len()
                            ),
                        });
                    }
                    Err(error) => {
                        app.set_status(format!("rollback hunk failed for {id}: {error}"));
                    }
                },
                Err(error) => {
                    app.set_status(format!("rollback hunk failed for {id}: {error}"));
                }
            }
        }
        TuiAction::RestoreRollbackHunk { id, hunk, apply } => {
            let Some(rollback_store) = rollback_store_for_config(config, app) else {
                return Ok(());
            };
            match restore_rollback_hunk(&rollback_store, &id, hunk, apply) {
                Ok(plan) if plan.applied => {
                    app.set_status(format!(
                        "restored rollback hunk {} #{} changed_files={}",
                        plan.snapshot_id,
                        plan.hunk,
                        plan.changed_files.len()
                    ));
                    app.set_mcp_detail(
                        TuiMcpDetailKind::Rollback,
                        render_rollback_hunk_restore_plan(&plan),
                    );
                }
                Ok(plan) => {
                    app.set_status(format!(
                        "dry-run rollback hunk {} #{} file={}",
                        plan.snapshot_id, plan.hunk, plan.file
                    ));
                    app.set_mcp_detail(
                        TuiMcpDetailKind::Rollback,
                        render_rollback_hunk_restore_plan(&plan),
                    );
                }
                Err(error) => {
                    app.set_status(format!(
                        "rollback hunk restore failed for {id} #{hunk}: {error}"
                    ));
                }
            }
        }
        TuiAction::RevertTurn { id, apply } => {
            let Some(rollback_store) = rollback_store_for_config(config, app) else {
                return Ok(());
            };
            match rollback_store.restore_snapshot(&id, apply) {
                Ok(plan) if plan.applied => {
                    app.set_status(format!(
                        "restored rollback {} changed_files={}",
                        plan.snapshot_id,
                        plan.changed_files.len()
                    ));
                    app.set_mcp_detail(
                        TuiMcpDetailKind::Rollback,
                        render_rollback_restore_plan(&plan),
                    );
                }
                Ok(plan) => {
                    app.set_status(format!(
                        "dry-run rollback {} current_patch={} snapshot_patch={}; add --apply to restore",
                        plan.snapshot_id, plan.current_patch_bytes, plan.patch_bytes
                    ));
                    app.set_mcp_detail(
                        TuiMcpDetailKind::Rollback,
                        render_rollback_restore_plan(&plan),
                    );
                }
                Err(error) => {
                    app.set_status(format!("rollback failed for {id}: {error}"));
                }
            }
        }
        TuiAction::TriggerAutomation {
            automation_id,
            prompt_override,
        } => match store.trigger_automation(&automation_id, prompt_override) {
            Ok((automation, task)) => {
                app.set_status(format!(
                    "triggered automation {} -> task {}",
                    automation.id, task.id
                ));
            }
            Err(error) => {
                app.set_status(format!(
                    "automation trigger failed for {automation_id}: {error}"
                ));
            }
        },
        TuiAction::CompactThread {
            thread_id,
            keep_tail_turns,
        } => match store.compact_thread(&thread_id, keep_tail_turns, None) {
            Ok(compaction) => {
                app.set_status(format!(
                    "compacted {thread_id}: summarized={} kept={}",
                    compaction.summarized_turn_count, compaction.kept_turn_count
                ));
            }
            Err(error) => {
                app.set_status(format!("compaction failed for {thread_id}: {error}"));
            }
        },
    }
    Ok(())
}

fn run_tui_submit_user_message(
    store: &RuntimeStore,
    config: Option<&AppConfig>,
    app: &mut TuiApp,
    thread_id: String,
    content: String,
    live_tx: Option<Sender<TuiLiveEvent>>,
) -> AppResult<()> {
    let turn = store.append_turn(&thread_id, "user".to_string(), content.clone())?;
    store.append_item(
        &thread_id,
        Some(&turn.id),
        "message".to_string(),
        Some("user".to_string()),
        content.clone(),
        "completed".to_string(),
    )?;
    if let Some(config) = config {
        let run_config = load_or_default().unwrap_or_else(|_| config.clone());
        start_tui_agent_run(
            store.clone(),
            run_config,
            thread_id.clone(),
            content,
            app.reasoning_replay_limit(),
            app.reasoning_replay_pinned_turn_ids(),
            app.translation_target_language_for_agent(),
            live_tx,
        );
        app.set_status(format!("started agent run for {thread_id}"));
    } else {
        app.set_status(format!("submitted user message to {thread_id}"));
    }
    Ok(())
}

fn latest_user_turn_index_and_content(
    store: &RuntimeStore,
    thread_id: &str,
) -> AppResult<(usize, String)> {
    let turns = store.list_turns(thread_id)?;
    turns
        .iter()
        .enumerate()
        .rev()
        .find(|(_, turn)| turn.role == "user")
        .map(|(index, turn)| (index, turn.content.clone()))
        .ok_or_else(|| app_error("no previous user message to undo"))
}

fn latest_user_turn_content(store: &RuntimeStore, thread_id: &str) -> AppResult<String> {
    latest_user_turn_index_and_content(store, thread_id).map(|(_, content)| content)
}

fn run_tui_undo_conversation(
    store: &RuntimeStore,
    app: &mut TuiApp,
    thread_id: &str,
    title_prefix: &str,
) -> AppResult<ThreadForkRecord> {
    let (keep_turn_count, content) = latest_user_turn_index_and_content(store, thread_id)?;
    let source = store.load_thread(thread_id)?;
    let fork = store.fork_thread_at_turn_count(
        thread_id,
        keep_turn_count,
        Some(format!(
            "{}: {}",
            title_prefix,
            runtime_summary(if content.trim().is_empty() {
                &source.title
            } else {
                &content
            })
        )),
    )?;
    refresh_app_from_store(store, app)?;
    app.clear_transient_conversation_state();
    app.select_thread_by_id(&fork.thread.id);
    Ok(fork)
}

fn run_tui_recall_archive(
    app: &mut TuiApp,
    config: Option<&AppConfig>,
    workspace: &str,
    thread_id: Option<&str>,
    query: &str,
) {
    let mut recall_config = config.cloned().unwrap_or_else(AppConfig::default);
    if config.is_none() {
        recall_config.workspace.config_dir =
            Path::new(workspace).join(".dscode").display().to_string();
    }
    let mut input = ToolInput::new()
        .with_arg("query", query.to_string())
        .with_arg("max_results", "5".to_string());
    if let Some(thread_id) = thread_id {
        input = input.with_arg("thread_id", thread_id.to_string());
    }
    match RecallArchiveTool::new(&recall_config).execute(input) {
        Ok(output) => {
            app.set_mcp_detail(
                TuiMcpDetailKind::Recall,
                format!(
                    "Recall Archive\n==============\n\nQuery: {}\nThread: {}\n\n{}",
                    query,
                    thread_id.unwrap_or("all recent threads"),
                    output.summary
                ),
            );
            app.set_status(format!(
                "recall complete: {}",
                last_nonempty_line(&output.summary, "ok")
            ));
        }
        Err(error) => {
            app.set_mcp_detail(
                TuiMcpDetailKind::Recall,
                format!("Recall Archive\n==============\n\nQuery: {query}\n\n{error}"),
            );
            app.set_status(format!("recall failed: {error}"));
        }
    }
}

fn run_tui_review_target(app: &mut TuiApp, workspace: &str, target: &str) {
    let input = ToolInput::new()
        .with_arg("target", target.to_string())
        .with_arg("cwd", workspace.to_string())
        .with_arg("max_chars", "20000".to_string());
    match ReviewTool::default().execute(input) {
        Ok(output) => {
            app.set_mcp_detail(
                TuiMcpDetailKind::Review,
                format!(
                    "Review\n======\n\nWorkspace: {}\nTarget: {}\n\n{}",
                    workspace, target, output.summary
                ),
            );
            app.set_status(format!(
                "review complete: {}",
                last_nonempty_line(&output.summary, "ok")
            ));
        }
        Err(error) => {
            app.set_mcp_detail(
                TuiMcpDetailKind::Review,
                format!("Review\n======\n\nWorkspace: {workspace}\nTarget: {target}\n\n{error}"),
            );
            app.set_status(format!("review failed: {error}"));
        }
    }
}

fn run_tui_memory_append(app: &mut TuiApp, config: Option<&AppConfig>, note: &str) {
    let Some(config) = config else {
        app.set_status("memory commands require local config".to_string());
        return;
    };
    if !config.memory.enabled {
        app.set_status("memory disabled; set memory.enabled=true or DSCODE_MEMORY=on".to_string());
        return;
    }
    let path = config.memory.memory_path();
    match crate::core::memory::append_user_memory(&path, note) {
        Ok(remembered) => {
            app.set_status(format!("remembered: {remembered} ({})", path.display()));
        }
        Err(error) => {
            app.set_status(format!("memory append failed: {error}"));
        }
    }
}

fn run_tui_memory_command(app: &mut TuiApp, config: Option<&AppConfig>, command: TuiMemoryCommand) {
    let Some(config) = config else {
        app.set_status("memory commands require local config".to_string());
        return;
    };
    let path = config.memory.memory_path();
    match command {
        TuiMemoryCommand::Show => {
            let body = match std::fs::read_to_string(&path) {
                Ok(content) if content.trim().is_empty() => "(empty)\n".to_string(),
                Ok(content) => content,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    "(empty)\n".to_string()
                }
                Err(error) => {
                    app.set_status(format!("memory show failed: {error}"));
                    return;
                }
            };
            let enabled = if config.memory.enabled {
                "enabled"
            } else {
                "disabled"
            };
            app.set_status(format!("memory {enabled}: {}", path.display()));
            app.set_mcp_detail(
                TuiMcpDetailKind::Memory,
                format!(
                    "User memory: {enabled}\nPath: {}\n\n{}",
                    path.display(),
                    body
                ),
            );
        }
        TuiMemoryCommand::Path => {
            let enabled = if config.memory.enabled {
                "enabled"
            } else {
                "disabled"
            };
            app.set_status(format!("memory path ({enabled}): {}", path.display()));
        }
        TuiMemoryCommand::Clear => {
            if !config.memory.enabled {
                app.set_status(
                    "memory disabled; enable it before clearing the memory file".to_string(),
                );
                return;
            }
            if let Some(parent) = path.parent() {
                if !parent.as_os_str().is_empty() {
                    if let Err(error) = std::fs::create_dir_all(parent) {
                        app.set_status(format!("memory clear failed: {error}"));
                        return;
                    }
                }
            }
            match std::fs::write(&path, "") {
                Ok(_) => app.set_status(format!("memory cleared: {}", path.display())),
                Err(error) => app.set_status(format!("memory clear failed: {error}")),
            }
        }
        TuiMemoryCommand::Edit => {
            let editor = std::env::var("VISUAL")
                .or_else(|_| std::env::var("EDITOR"))
                .unwrap_or_else(|_| "vi".to_string());
            app.set_status(format!("memory edit command: {editor} {}", path.display()));
            app.set_mcp_detail(
                TuiMcpDetailKind::Memory,
                format!(
                    "Memory edit command:\n{} {}\n\nDeepSeekCode prints the editor command instead of spawning it inside the TUI.",
                    editor,
                    path.display()
                ),
            );
        }
        TuiMemoryCommand::Help => {
            app.set_status("memory commands: show|path|clear|edit|help".to_string());
            app.set_mcp_detail(
                TuiMcpDetailKind::Memory,
                format!(
                    "Memory commands:\n- # <note> in the composer appends a durable memory note without starting a model turn.\n- /memory shows the memory file.\n- /memory path prints the configured path.\n- /memory clear empties the file when memory is enabled.\n- /memory edit prints the editor command.\n\nPath: {}\nEnabled: {}",
                    path.display(),
                    config.memory.enabled
                ),
            );
        }
    }
}

fn run_tui_note_command(app: &mut TuiApp, config: Option<&AppConfig>, command: TuiNoteCommand) {
    let Some(config) = config else {
        app.set_status("note commands require local config".to_string());
        return;
    };
    let path = config.memory.notes_path();
    match command {
        TuiNoteCommand::Add { content } => match append_tui_note(&path, &content) {
            Ok(()) => {
                app.set_status(format!("note appended: {}", path.display()));
                app.set_mcp_detail(
                    TuiMcpDetailKind::Note,
                    format!(
                        "Note appended\nPath: {}\n\n{}",
                        path.display(),
                        content.trim()
                    ),
                );
            }
            Err(error) => app.set_status(format!("note append failed: {error}")),
        },
        TuiNoteCommand::List => match read_tui_notes(&path) {
            Ok(notes) => {
                app.set_status(format!("notes listed: {} note(s)", notes.len()));
                app.set_mcp_detail(TuiMcpDetailKind::Note, render_tui_notes_list(&path, &notes));
            }
            Err(error) => app.set_status(format!("note list failed: {error}")),
        },
        TuiNoteCommand::Show { index } => match read_tui_notes(&path) {
            Ok(notes) => match notes.get(index - 1) {
                Some(note) => {
                    app.set_status(format!("showing note {index}"));
                    app.set_mcp_detail(
                        TuiMcpDetailKind::Note,
                        format!("Note {index}\nPath: {}\n\n{}", path.display(), note),
                    );
                }
                None => app.set_status(format!("note {index} not found")),
            },
            Err(error) => app.set_status(format!("note show failed: {error}")),
        },
        TuiNoteCommand::Edit { index, content } => match read_tui_notes(&path) {
            Ok(mut notes) => {
                if index > notes.len() {
                    app.set_status(format!("note {index} not found"));
                    return;
                }
                notes[index - 1] = content.trim().to_string();
                match write_tui_notes(&path, &notes) {
                    Ok(()) => {
                        app.set_status(format!("note {index} updated"));
                        app.set_mcp_detail(
                            TuiMcpDetailKind::Note,
                            format!(
                                "Note {index} updated\nPath: {}\n\n{}",
                                path.display(),
                                content.trim()
                            ),
                        );
                    }
                    Err(error) => app.set_status(format!("note edit failed: {error}")),
                }
            }
            Err(error) => app.set_status(format!("note edit failed: {error}")),
        },
        TuiNoteCommand::Remove { index } => match read_tui_notes(&path) {
            Ok(mut notes) => {
                if index > notes.len() {
                    app.set_status(format!("note {index} not found"));
                    return;
                }
                let removed = notes.remove(index - 1);
                match write_tui_notes(&path, &notes) {
                    Ok(()) => {
                        app.set_status(format!("note {index} removed"));
                        app.set_mcp_detail(
                            TuiMcpDetailKind::Note,
                            format!(
                                "Note {index} removed\nPath: {}\n\nRemoved:\n{}",
                                path.display(),
                                removed
                            ),
                        );
                    }
                    Err(error) => app.set_status(format!("note remove failed: {error}")),
                }
            }
            Err(error) => app.set_status(format!("note remove failed: {error}")),
        },
        TuiNoteCommand::Clear => match write_tui_notes(&path, &[]) {
            Ok(()) => {
                app.set_status(format!("notes cleared: {}", path.display()));
                app.set_mcp_detail(
                    TuiMcpDetailKind::Note,
                    format!("Notes cleared\nPath: {}", path.display()),
                );
            }
            Err(error) => app.set_status(format!("note clear failed: {error}")),
        },
        TuiNoteCommand::Path => {
            app.set_status(format!("notes path: {}", path.display()));
            app.set_mcp_detail(
                TuiMcpDetailKind::Note,
                format!("Notes path\n\n{}", path.display()),
            );
        }
        TuiNoteCommand::Help => {
            app.set_status("note commands: add|list|show|edit|remove|clear|path".to_string());
            app.set_mcp_detail(
                TuiMcpDetailKind::Note,
                format!(
                    "Note commands:\n- /note <text> appends a persistent workspace note.\n- /note add <text> appends a note.\n- /note list lists notes.\n- /note show <n> shows one note.\n- /note edit <n> <text> replaces one note.\n- /note remove <n> removes one note.\n- /note clear clears the notes file.\n- /note path prints the configured notes path.\n\nPath: {}",
                    path.display()
                ),
            );
        }
    }
}

fn append_tui_note(path: &Path, content: &str) -> AppResult<()> {
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return Err(app_error("note content must not be empty"));
    }
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    writeln!(file, "\n---\n{trimmed}")?;
    Ok(())
}

fn read_tui_notes(path: &Path) -> AppResult<Vec<String>> {
    match std::fs::read_to_string(path) {
        Ok(content) => Ok(parse_tui_notes(&content)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(error.into()),
    }
}

fn write_tui_notes(path: &Path, notes: &[String]) -> AppResult<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let body = notes
        .iter()
        .map(|note| note.trim())
        .filter(|note| !note.is_empty())
        .collect::<Vec<_>>()
        .join("\n---\n");
    std::fs::write(path, body)?;
    Ok(())
}

fn parse_tui_notes(content: &str) -> Vec<String> {
    content
        .split("\n---\n")
        .map(str::trim)
        .filter(|note| !note.is_empty())
        .map(str::to_string)
        .collect()
}

fn render_tui_notes_list(path: &Path, notes: &[String]) -> String {
    let mut detail = String::new();
    detail.push_str("Notes\n");
    detail.push_str(&format!("Path: {}\n", path.display()));
    detail.push_str(&format!("Count: {}\n", notes.len()));
    detail.push('\n');
    if notes.is_empty() {
        detail.push_str("No notes recorded. Use /note <text> or /note add <text>.\n");
    } else {
        for (index, note) in notes.iter().enumerate() {
            detail.push_str(&format!("{}. {}\n", index + 1, runtime_summary(note)));
        }
        detail.push_str("\nUse /note show <n>, /note edit <n> <text>, or /note remove <n>.\n");
    }
    detail
}

fn run_tui_anchor_command(app: &mut TuiApp, workspace: &Path, command: TuiAnchorCommand) {
    let path = tui_anchors_path(workspace);
    match command {
        TuiAnchorCommand::Add { content } => match append_tui_anchor(&path, &content) {
            Ok(()) => {
                app.set_status(format!("anchor pinned: {}", path.display()));
                app.set_mcp_detail(
                    TuiMcpDetailKind::Anchor,
                    format!(
                        "Anchor pinned\nPath: {}\n\n{}",
                        path.display(),
                        content.trim()
                    ),
                );
            }
            Err(error) => app.set_status(format!("anchor pin failed: {error}")),
        },
        TuiAnchorCommand::List => match read_tui_anchors(&path) {
            Ok(anchors) => {
                app.set_status(format!("anchors listed: {} anchor(s)", anchors.len()));
                app.set_mcp_detail(
                    TuiMcpDetailKind::Anchor,
                    render_tui_anchors_list(&path, &anchors),
                );
            }
            Err(error) => app.set_status(format!("anchor list failed: {error}")),
        },
        TuiAnchorCommand::Remove { index } => match read_tui_anchors(&path) {
            Ok(mut anchors) => {
                if index > anchors.len() {
                    app.set_status(format!("anchor {index} not found"));
                    return;
                }
                let removed = anchors.remove(index - 1);
                match write_tui_anchors(&path, &anchors) {
                    Ok(()) => {
                        app.set_status(format!("anchor {index} removed"));
                        app.set_mcp_detail(
                            TuiMcpDetailKind::Anchor,
                            format!(
                                "Anchor {index} removed\nPath: {}\n\nRemoved:\n{}",
                                path.display(),
                                removed
                            ),
                        );
                    }
                    Err(error) => app.set_status(format!("anchor remove failed: {error}")),
                }
            }
            Err(error) => app.set_status(format!("anchor remove failed: {error}")),
        },
        TuiAnchorCommand::Path => {
            app.set_status(format!("anchors path: {}", path.display()));
            app.set_mcp_detail(
                TuiMcpDetailKind::Anchor,
                format!("Anchors path\n\n{}", path.display()),
            );
        }
        TuiAnchorCommand::Help => {
            app.set_status("anchor commands: add|list|remove|path".to_string());
            app.set_mcp_detail(
                TuiMcpDetailKind::Anchor,
                format!(
                    "Anchor commands:\n- /anchor <text> pins a workspace fact.\n- /anchor add <text> pins a workspace fact.\n- /anchor list lists pinned anchors.\n- /anchor remove <n> removes one anchor.\n- /anchor path prints the workspace anchor path.\n\nPath: {}\n\nAnchors are stored as durable workspace context for compaction-aware workflows.",
                    path.display()
                ),
            );
        }
    }
}

fn tui_anchors_path(workspace: &Path) -> PathBuf {
    workspace.join(".dscode").join("anchors.md")
}

fn append_tui_anchor(path: &Path, content: &str) -> AppResult<()> {
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return Err(app_error("anchor content must not be empty"));
    }
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    writeln!(file, "\n---\n{trimmed}")?;
    Ok(())
}

fn read_tui_anchors(path: &Path) -> AppResult<Vec<String>> {
    match std::fs::read_to_string(path) {
        Ok(content) => Ok(parse_tui_anchors(&content)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(error.into()),
    }
}

fn write_tui_anchors(path: &Path, anchors: &[String]) -> AppResult<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let body = anchors
        .iter()
        .map(|anchor| anchor.trim())
        .filter(|anchor| !anchor.is_empty())
        .collect::<Vec<_>>()
        .join("\n---\n");
    std::fs::write(path, body)?;
    Ok(())
}

fn parse_tui_anchors(content: &str) -> Vec<String> {
    content
        .split("\n---\n")
        .map(str::trim)
        .filter(|anchor| !anchor.is_empty())
        .map(str::to_string)
        .collect()
}

fn render_tui_anchors_list(path: &Path, anchors: &[String]) -> String {
    let mut detail = String::new();
    detail.push_str("Anchors\n");
    detail.push_str(&format!("Path: {}\n", path.display()));
    detail.push_str(&format!("Count: {}\n", anchors.len()));
    detail.push('\n');
    if anchors.is_empty() {
        detail.push_str("No anchors pinned. Use /anchor <text> to pin a fact.\n");
    } else {
        for (index, anchor) in anchors.iter().enumerate() {
            detail.push_str(&format!("{}. {}\n", index + 1, runtime_summary(anchor)));
        }
        detail.push_str("\nUse /anchor remove <n> to remove an anchor.\n");
    }
    detail
}

fn run_tui_share_session(store: &RuntimeStore, app: &mut TuiApp, thread_id: &str) -> AppResult<()> {
    let thread = store.load_thread(thread_id)?;
    let session = thread
        .session_id
        .as_deref()
        .and_then(|session_id| store.load_session(session_id).ok());
    let items = store.list_items(thread_id, None)?;
    if items.is_empty() {
        app.set_status("nothing to share: active thread has no transcript items".to_string());
        app.set_mcp_detail(
            TuiMcpDetailKind::Share,
            format!(
                "Share export skipped\n\nThread: {}\nNo transcript items.",
                thread.title
            ),
        );
        return Ok(());
    }

    let html = render_tui_share_html(session.as_ref(), &thread, &items);
    let path = write_tui_share_html(&html)?;
    match upload_tui_share_gist(&path) {
        Ok(url) => {
            app.set_status(format!("share gist created: {url}"));
            app.set_mcp_detail(
                TuiMcpDetailKind::Share,
                format!(
                    "Share export complete\n\nURL: {url}\nLocal HTML: {}\nItems: {}",
                    path.display(),
                    items.len()
                ),
            );
        }
        Err(error) => {
            app.set_status(format!(
                "share gist upload failed; local export kept: {error}"
            ));
            app.set_mcp_detail(
                TuiMcpDetailKind::Share,
                format!(
                    "Share export written\n\nLocal HTML: {}\nItems: {}\n\nGist upload failed:\n{}",
                    path.display(),
                    items.len(),
                    error
                ),
            );
        }
    }
    Ok(())
}

fn render_tui_share_html(
    session: Option<&SessionRecord>,
    thread: &ThreadRecord,
    items: &[ItemRecord],
) -> String {
    let session_title = session
        .map(|session| session.title.as_str())
        .unwrap_or("No session");
    let workspace = session
        .map(|session| session.workspace.as_str())
        .unwrap_or(thread.workspace.as_str());
    let mut body = String::new();
    for item in items {
        let role = item.role.as_deref().unwrap_or(item.item_type.as_str());
        let class = match role {
            "user" => "user",
            "assistant" => "assistant",
            "tool" => "tool",
            _ => "item",
        };
        let _ = writeln!(
            body,
            "<section class=\"message {class}\"><div class=\"meta\">#{index} {role} · {kind} · {status}</div><pre>{content}</pre></section>",
            index = item.index,
            kind = html_escape(&item.item_type),
            status = html_escape(&item.status),
            content = html_escape(&item.content)
        );
    }
    format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<meta name="viewport" content="width=device-width, initial-scale=1.0">
<title>DeepSeekCode TUI Session Export</title>
<style>
  body {{ font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif; max-width: 900px; margin: 2rem auto; padding: 0 1rem; background: #0d1117; color: #c9d1d9; }}
  h1 {{ color: #58a6ff; border-bottom: 1px solid #30363d; padding-bottom: 0.5rem; }}
  .meta {{ color: #8b949e; font-size: 0.9rem; margin-bottom: 0.5rem; }}
  .message {{ margin: 1rem 0; padding: 0.85rem; border-radius: 6px; border: 1px solid #30363d; }}
  .user {{ background: #1f2937; border-left: 3px solid #58a6ff; }}
  .assistant {{ background: #161b22; border-left: 3px solid #3fb950; }}
  .tool {{ background: #0d1117; border-left: 3px solid #d29922; }}
  pre {{ white-space: pre-wrap; overflow-wrap: anywhere; margin: 0; }}
  footer {{ margin-top: 2rem; padding-top: 1rem; border-top: 1px solid #30363d; color: #8b949e; font-size: 0.85rem; }}
</style>
</head>
<body>
<h1>DeepSeekCode TUI Session</h1>
<div class="meta"><strong>Session:</strong> {session}</div>
<div class="meta"><strong>Thread:</strong> {thread_title} · <strong>Mode:</strong> {mode} · <strong>Status:</strong> {status}</div>
<div class="meta"><strong>Workspace:</strong> {workspace}</div>
<main>
{body}
</main>
<footer>Generated by DeepSeekCode · https://github.com/willamhou/DeepSeekCode</footer>
</body>
</html>"#,
        session = html_escape(session_title),
        thread_title = html_escape(&thread.title),
        mode = html_escape(&thread.mode),
        status = html_escape(&thread.status),
        workspace = html_escape(workspace),
    )
}

fn write_tui_share_html(html: &str) -> AppResult<PathBuf> {
    let mut path = std::env::temp_dir();
    path.push(format!(
        "deepseekcode-share-{}-{}.html",
        std::process::id(),
        epoch_millis_label()
    ));
    std::fs::write(&path, html)?;
    Ok(path)
}

fn upload_tui_share_gist(path: &Path) -> Result<String, String> {
    let path_string = path.to_string_lossy().to_string();
    let output = Command::new("gh")
        .args([
            "gist",
            "create",
            "--public",
            path_string.as_str(),
            "--filename",
            "session-export.html",
            "--desc",
            "DeepSeekCode TUI Session Export",
        ])
        .output()
        .map_err(|error| format!("failed to run `gh gist create`: {error}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(if stderr.is_empty() {
            "`gh gist create` failed without stderr".to_string()
        } else {
            format!("`gh gist create` failed: {stderr}")
        });
    }
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if stdout.is_empty() {
        return Err("`gh gist create` returned no URL".to_string());
    }
    Ok(stdout)
}

fn run_tui_export_thread(
    store: &RuntimeStore,
    app: &mut TuiApp,
    thread_id: &str,
    requested_path: Option<&str>,
) -> AppResult<()> {
    let thread = store.load_thread(thread_id)?;
    let session = thread
        .session_id
        .as_deref()
        .and_then(|session_id| store.load_session(session_id).ok());
    let items = store.list_items(thread_id, None)?;
    let path = resolve_tui_export_path(session.as_ref(), &thread, requested_path);
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)?;
    }
    let markdown = render_tui_export_markdown(session.as_ref(), &thread, &items);
    std::fs::write(&path, markdown)?;
    app.set_status(format!("exported thread markdown: {}", path.display()));
    app.set_mcp_detail(
        TuiMcpDetailKind::Export,
        format!(
            "Export complete\n\nPath: {}\nItems: {}\nThread: {}\nWorkspace: {}",
            path.display(),
            items.len(),
            thread.title,
            tui_export_workspace(session.as_ref(), &thread).display()
        ),
    );
    Ok(())
}

const TUI_SESSION_SNAPSHOT_KIND: &str = "deepseek.tui.session_snapshot.v1";

struct TuiSessionSnapshotImport {
    session: SessionRecord,
    thread: ThreadRecord,
    turn_count: usize,
    item_count: usize,
}

fn run_tui_save_session(
    store: &RuntimeStore,
    app: &mut TuiApp,
    session_id: &str,
    thread_id: &str,
    requested_path: Option<&str>,
) -> AppResult<()> {
    let session = store.load_session(session_id)?;
    let thread = store.load_thread(thread_id)?;
    if thread.session_id.as_deref() != Some(session.id.as_str()) {
        return Err(app_error(format!(
            "thread `{thread_id}` does not belong to session `{session_id}`"
        )));
    }
    let turns = store.list_turns(thread_id)?;
    let items = store.list_items(thread_id, None)?;
    let path = resolve_tui_save_path(&session, &thread, requested_path);
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)?;
    }
    let snapshot = tui_session_snapshot_json(&session, &thread, &turns, &items);
    let mut content = json_value_to_string(&snapshot);
    content.push('\n');
    std::fs::write(&path, content)?;
    app.set_status(format!("saved session snapshot: {}", path.display()));
    app.set_mcp_detail(
        TuiMcpDetailKind::Save,
        format!(
            "Save complete\n\nPath: {}\nSession: {}\nThread: {}\nTurns: {}\nItems: {}",
            path.display(),
            session.title,
            thread.title,
            turns.len(),
            items.len()
        ),
    );
    Ok(())
}

fn run_tui_prune_sessions(store: &RuntimeStore, app: &mut TuiApp, days: u64) -> AppResult<()> {
    let max_age = Duration::from_secs(days.saturating_mul(24 * 60 * 60));
    let summary = store.prune_sessions_older_than(max_age)?;
    refresh_app_from_store(store, app)?;
    if summary.sessions == 0 {
        app.set_status(format!("no sessions older than {days}d to prune"));
    } else {
        app.set_status(format!(
            "pruned {} session{} older than {days}d",
            summary.sessions,
            if summary.sessions == 1 { "" } else { "s" }
        ));
    }
    Ok(())
}

fn run_tui_load_session(
    store: &RuntimeStore,
    app: &mut TuiApp,
    workspace: &str,
    requested_path: &str,
) -> AppResult<()> {
    let path = resolve_tui_load_path(workspace, requested_path);
    let content = std::fs::read_to_string(&path)?;
    let imported = import_tui_session_snapshot(store, workspace, &content)?;
    refresh_app_from_store(store, app)?;
    app.select_thread_by_id(&imported.thread.id);
    app.set_status(format!("loaded session snapshot: {}", path.display()));
    app.set_mcp_detail(
        TuiMcpDetailKind::Load,
        format!(
            "Load complete\n\nPath: {}\nNew session: {} [{}]\nNew thread: {} [{}]\nTurns: {}\nItems: {}",
            path.display(),
            imported.session.title,
            imported.session.id,
            imported.thread.title,
            imported.thread.id,
            imported.turn_count,
            imported.item_count
        ),
    );
    Ok(())
}

fn tui_session_snapshot_json(
    session: &SessionRecord,
    thread: &ThreadRecord,
    turns: &[TurnRecord],
    items: &[ItemRecord],
) -> JsonValue {
    json_object([
        (
            "kind",
            JsonValue::String(TUI_SESSION_SNAPSHOT_KIND.to_string()),
        ),
        ("session", session_to_json(session)),
        ("thread", thread_to_json(thread)),
        (
            "turns",
            JsonValue::Array(turns.iter().map(turn_to_json).collect()),
        ),
        (
            "items",
            JsonValue::Array(items.iter().map(item_to_json).collect()),
        ),
    ])
}

fn import_tui_session_snapshot(
    store: &RuntimeStore,
    fallback_workspace: &str,
    content: &str,
) -> AppResult<TuiSessionSnapshotImport> {
    let root = parse_root_object(content)?;
    let kind = root
        .get("kind")
        .and_then(json_as_string)
        .ok_or_else(|| app_error("session snapshot missing kind"))?;
    if kind != TUI_SESSION_SNAPSHOT_KIND {
        return Err(app_error(format!(
            "unsupported session snapshot kind `{kind}`"
        )));
    }

    let saved_session = parse_session_record(snapshot_object(&root, "session")?)?;
    let saved_thread = parse_thread_record(snapshot_object(&root, "thread")?)?;
    let mut saved_turns = snapshot_array(&root, "turns")?
        .iter()
        .map(|value| {
            let object = json_as_object(value)
                .ok_or_else(|| app_error("session snapshot turns entry must be an object"))?;
            parse_turn_record(object)
        })
        .collect::<AppResult<Vec<_>>>()?;
    let mut saved_items = snapshot_array(&root, "items")?
        .iter()
        .map(|value| {
            let object = json_as_object(value)
                .ok_or_else(|| app_error("session snapshot items entry must be an object"))?;
            parse_item_record(object)
        })
        .collect::<AppResult<Vec<_>>>()?;

    saved_turns.sort_by_key(|turn| turn.index);
    saved_items.sort_by_key(|item| item.index);

    let session_workspace = non_empty_or(&saved_session.workspace, fallback_workspace);
    let imported_session = store.create_session(
        format_imported_title(&saved_session.title),
        session_workspace.to_string(),
    )?;
    let thread_workspace = non_empty_or(&saved_thread.workspace, session_workspace);
    let imported_thread = store.create_thread_for_session(
        &imported_session.id,
        format_imported_title(&saved_thread.title),
        thread_workspace.to_string(),
        saved_thread.model.clone(),
        saved_thread.mode.clone(),
    )?;

    let mut turn_id_map = BTreeMap::<String, String>::new();
    for saved_turn in &saved_turns {
        let imported_turn = store.append_turn(
            &imported_thread.id,
            saved_turn.role.clone(),
            saved_turn.content.clone(),
        )?;
        if saved_turn.status != "completed" {
            store.update_turn(
                &imported_thread.id,
                &imported_turn.id,
                saved_turn.content.clone(),
                saved_turn.status.clone(),
            )?;
        }
        turn_id_map.insert(saved_turn.id.clone(), imported_turn.id);
    }

    for saved_item in &saved_items {
        let mapped_turn_id = saved_item
            .turn_id
            .as_ref()
            .and_then(|turn_id| turn_id_map.get(turn_id));
        store.append_item(
            &imported_thread.id,
            mapped_turn_id.map(String::as_str),
            saved_item.item_type.clone(),
            saved_item.role.clone(),
            saved_item.content.clone(),
            saved_item.status.clone(),
        )?;
    }
    store.append_thread_event(
        &imported_thread.id,
        "session_snapshot_loaded",
        json_object([
            ("source_session_id", JsonValue::String(saved_session.id)),
            ("source_thread_id", JsonValue::String(saved_thread.id)),
            ("turns", JsonValue::Number(saved_turns.len().to_string())),
            ("items", JsonValue::Number(saved_items.len().to_string())),
        ]),
    )?;

    let session = store.load_session(&imported_session.id)?;
    let thread = store.load_thread(&imported_thread.id)?;
    Ok(TuiSessionSnapshotImport {
        session,
        thread,
        turn_count: saved_turns.len(),
        item_count: saved_items.len(),
    })
}

fn snapshot_object<'a>(
    root: &'a BTreeMap<String, JsonValue>,
    key: &str,
) -> AppResult<&'a BTreeMap<String, JsonValue>> {
    root.get(key)
        .and_then(json_as_object)
        .ok_or_else(|| app_error(format!("session snapshot missing object `{key}`")))
}

fn snapshot_array<'a>(
    root: &'a BTreeMap<String, JsonValue>,
    key: &str,
) -> AppResult<&'a Vec<JsonValue>> {
    root.get(key)
        .and_then(json_as_array)
        .ok_or_else(|| app_error(format!("session snapshot missing array `{key}`")))
}

fn non_empty_or<'a>(value: &'a str, fallback: &'a str) -> &'a str {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        fallback
    } else {
        trimmed
    }
}

fn format_imported_title(title: &str) -> String {
    let title = title.trim();
    if title.is_empty() {
        "Imported session".to_string()
    } else {
        format!("Imported: {title}")
    }
}

fn resolve_tui_export_path(
    session: Option<&SessionRecord>,
    thread: &ThreadRecord,
    requested_path: Option<&str>,
) -> PathBuf {
    let workspace = tui_export_workspace(session, thread);
    let Some(requested_path) = requested_path
        .map(str::trim)
        .filter(|path| !path.is_empty())
    else {
        return workspace.join(format!("chat_export_{}.md", epoch_millis_label()));
    };
    let expanded = expand_tilde(requested_path);
    if expanded.is_absolute() {
        expanded
    } else {
        workspace.join(expanded)
    }
}

fn resolve_tui_save_path(
    session: &SessionRecord,
    thread: &ThreadRecord,
    requested_path: Option<&str>,
) -> PathBuf {
    let workspace = tui_export_workspace(Some(session), thread);
    let Some(requested_path) = requested_path
        .map(str::trim)
        .filter(|path| !path.is_empty())
    else {
        return workspace.join(format!("session_{}.json", epoch_millis_label()));
    };
    let expanded = expand_tilde(requested_path);
    if expanded.is_absolute() {
        expanded
    } else {
        workspace.join(expanded)
    }
}

fn resolve_tui_load_path(workspace: &str, requested_path: &str) -> PathBuf {
    let expanded = expand_tilde(requested_path.trim());
    if expanded.is_absolute() {
        expanded
    } else {
        PathBuf::from(workspace).join(expanded)
    }
}

fn tui_export_workspace(session: Option<&SessionRecord>, thread: &ThreadRecord) -> PathBuf {
    session
        .map(|session| PathBuf::from(&session.workspace))
        .unwrap_or_else(|| PathBuf::from(&thread.workspace))
}

fn render_tui_export_markdown(
    session: Option<&SessionRecord>,
    thread: &ThreadRecord,
    items: &[ItemRecord],
) -> String {
    let session_title = session
        .map(|session| session.title.as_str())
        .unwrap_or("No session");
    let workspace = session
        .map(|session| session.workspace.as_str())
        .unwrap_or(thread.workspace.as_str());
    let mut content = String::new();
    let _ = writeln!(content, "# Chat Export");
    let _ = writeln!(content);
    let _ = writeln!(content, "**Session:** {session_title}");
    let _ = writeln!(content, "**Thread:** {}", thread.title);
    let _ = writeln!(content, "**Model:** {}", thread.model);
    let _ = writeln!(content, "**Mode:** {}", thread.mode);
    let _ = writeln!(content, "**Workspace:** {workspace}");
    let _ = writeln!(content, "**Date:** {}", epoch_millis_label());
    let _ = writeln!(content);
    let _ = writeln!(content, "---");
    let _ = writeln!(content);

    if items.is_empty() {
        let _ = writeln!(content, "No transcript items.");
        return content;
    }

    for item in items {
        let _ = writeln!(content, "{}", tui_export_item_label(item));
        let _ = writeln!(content);
        let body = item.content.trim();
        if body.is_empty() {
            let _ = writeln!(content, "_No content._");
        } else {
            let _ = writeln!(content, "{body}");
        }
        let _ = writeln!(content);
        let _ = writeln!(
            content,
            "_Item #{}, type: {}, status: {}_",
            item.index, item.item_type, item.status
        );
        let _ = writeln!(content);
        let _ = writeln!(content, "---");
        let _ = writeln!(content);
    }
    content
}

fn tui_export_item_label(item: &ItemRecord) -> String {
    let key = item.role.as_deref().unwrap_or(item.item_type.as_str());
    match key {
        "user" => "**You:**".to_string(),
        "assistant" => "**Assistant:**".to_string(),
        "system" => "*System:*".to_string(),
        "tool" | "tool_result" | "function_call" | "function_result" => "**Tool:**".to_string(),
        "reasoning" | "thinking" => "*Thinking:*".to_string(),
        value => format!("**{}:**", tui_export_title(value)),
    }
}

fn tui_export_title(value: &str) -> String {
    let words = value
        .split(['_', '-', ' '])
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>();
    if words.is_empty() {
        return "Item".to_string();
    }
    words
        .into_iter()
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                Some(first) => format!("{}{}", first.to_uppercase(), chars.as_str()),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn run_tui_clear_conversation(
    store: &RuntimeStore,
    config: Option<&AppConfig>,
    app: &mut TuiApp,
    session_id: &str,
    previous_thread_id: Option<&str>,
) -> AppResult<()> {
    let session = store.load_session(session_id)?;
    let previous_thread =
        previous_thread_id.and_then(|thread_id| store.load_thread(thread_id).ok());
    let workspace = previous_thread
        .as_ref()
        .map(|thread| thread.workspace.clone())
        .unwrap_or_else(|| session.workspace.clone());
    let model = previous_thread
        .as_ref()
        .map(|thread| thread.model.clone())
        .or_else(|| config.map(|config| config.model.model.clone()))
        .unwrap_or_else(|| AppConfig::default().model.model);
    let mode = previous_thread
        .as_ref()
        .map(|thread| thread.mode.clone())
        .unwrap_or_else(|| "agent".to_string());
    let thread = store.create_thread_for_session(
        session_id,
        "New conversation".to_string(),
        workspace,
        model,
        mode,
    )?;
    refresh_app_from_store(store, app)?;
    app.clear_transient_conversation_state();
    app.select_thread_by_id(&thread.id);
    app.set_status(format!(
        "cleared conversation; new active thread {}",
        thread.id
    ));
    Ok(())
}

fn run_tui_create_subagent_task(
    store: &RuntimeStore,
    thread_id: &str,
    max_depth: usize,
    task: &str,
) -> AppResult<TaskRecord> {
    let thread = store.load_thread(thread_id)?;
    store.create_task(
        thread.session_id.as_deref(),
        Some(thread_id),
        None,
        "subagent".to_string(),
        "pending".to_string(),
        tui_subagent_task_summary(max_depth, task),
    )
}

fn tui_subagent_task_summary(max_depth: usize, task: &str) -> String {
    format!("max_depth={max_depth}: {}", task.trim())
}

fn run_tui_diff_command(app: &mut TuiApp, workspace: &Path) {
    match render_tui_diff_detail(workspace) {
        Ok(detail) => {
            let no_changes = detail.contains("No changes since session start");
            app.set_mcp_detail(TuiMcpDetailKind::Diff, detail);
            app.set_status(if no_changes {
                "no diff changes".to_string()
            } else {
                "diff shown".to_string()
            });
        }
        Err(error) => {
            app.set_status(format!("diff failed: {error}"));
            app.set_mcp_detail(
                TuiMcpDetailKind::Diff,
                format!(
                    "Diff failed\n\nWorkspace: {}\nError: {error}",
                    workspace.display()
                ),
            );
        }
    }
}

fn render_tui_diff_detail(workspace: &Path) -> AppResult<String> {
    let names = git_diff_output(workspace, &["diff", "--name-only"])?;
    let stat = git_diff_output(workspace, &["diff", "--stat"])?;
    let files = names
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>();

    let mut detail = String::new();
    let _ = writeln!(detail, "DeepSeekCode Diff");
    let _ = writeln!(detail, "=================");
    let _ = writeln!(detail);
    let _ = writeln!(detail, "Workspace: {}", workspace.display());
    let _ = writeln!(detail);

    if files.is_empty() {
        let _ = writeln!(detail, "No changes since session start");
        return Ok(detail);
    }

    let renamed_count = files.iter().filter(|file| file.contains(" -> ")).count();
    if renamed_count > 0 {
        let _ = writeln!(
            detail,
            "Changed files ({}, {} renamed):",
            files.len(),
            renamed_count
        );
    } else {
        let _ = writeln!(detail, "Changed files ({}):", files.len());
    }
    for file in &files {
        let _ = writeln!(detail, "{file}");
    }

    let stat = stat.trim();
    if !stat.is_empty() {
        let _ = writeln!(detail);
        let _ = writeln!(detail, "Stat");
        let _ = writeln!(detail, "----");
        let _ = writeln!(detail, "{stat}");
    }
    Ok(detail)
}

fn git_diff_output(workspace: &Path, args: &[&str]) -> AppResult<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(workspace)
        .output()
        .map_err(|error| app_error(format!("could not invoke git: {error}")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let command = args.join(" ");
        return Err(app_error(if stderr.is_empty() {
            format!("git {command} failed")
        } else {
            format!("git {command} failed: {stderr}")
        }));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn html_escape(value: &str) -> String {
    let mut escaped = String::new();
    for ch in value.chars() {
        match ch {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#39;"),
            _ => escaped.push(ch),
        }
    }
    escaped
}

fn epoch_millis_label() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0)
}

fn run_tui_hooks_command(app: &mut TuiApp, config: Option<&AppConfig>, command: TuiHooksCommand) {
    let Some(config) = config else {
        app.set_status("hooks commands require local config".to_string());
        return;
    };
    match command {
        TuiHooksCommand::List => match render_tui_hooks_list(config) {
            Ok(detail) => {
                app.set_status("hooks listed".to_string());
                app.set_mcp_detail(TuiMcpDetailKind::Hooks, detail);
            }
            Err(error) => app.set_status(format!("hooks list failed: {error}")),
        },
        TuiHooksCommand::Events => {
            app.set_status("hook events listed".to_string());
            app.set_mcp_detail(TuiMcpDetailKind::Hooks, render_tui_hooks_events());
        }
    }
}

#[derive(Clone, Copy)]
struct TuiHookEventSpec {
    event: HookEvent,
    description: &'static str,
}

fn tui_hook_event_specs() -> [TuiHookEventSpec; 10] {
    [
        TuiHookEventSpec {
            event: HookEvent::SessionStart,
            description: "fires when a local agent session starts",
        },
        TuiHookEventSpec {
            event: HookEvent::SessionStop,
            description: "fires when a local agent session stops",
        },
        TuiHookEventSpec {
            event: HookEvent::UserPromptSubmit,
            description: "fires before a submitted user prompt is dispatched",
        },
        TuiHookEventSpec {
            event: HookEvent::PreToolUse,
            description: "fires before a tool call is executed",
        },
        TuiHookEventSpec {
            event: HookEvent::PermissionRequest,
            description: "fires when a permission gate asks for approval",
        },
        TuiHookEventSpec {
            event: HookEvent::PostToolUse,
            description: "fires after a tool call completes",
        },
        TuiHookEventSpec {
            event: HookEvent::SubagentStart,
            description: "fires when a subagent task is queued or started",
        },
        TuiHookEventSpec {
            event: HookEvent::SubagentStop,
            description: "fires when a subagent task finishes",
        },
        TuiHookEventSpec {
            event: HookEvent::PreCompact,
            description: "fires before active context compaction",
        },
        TuiHookEventSpec {
            event: HookEvent::ShellEnv,
            description: "fires before shell tools and may emit KEY=VALUE environment",
        },
    ]
}

fn render_tui_hooks_events() -> String {
    let mut detail = String::new();
    detail.push_str("Hook Events\n\n");
    detail.push_str("Create executable scripts in one of these event directories under the project or user hook root.\n\n");
    for spec in tui_hook_event_specs() {
        detail.push_str(&format!(
            "- {}: {}\n",
            spec.event.dir_name(),
            spec.description
        ));
    }
    detail
}

fn render_tui_hooks_list(config: &AppConfig) -> AppResult<String> {
    let project_dir = PathBuf::from(&config.hooks.project_dir);
    let user_dir = expand_tilde(&config.hooks.user_dir);
    let mut detail = String::new();
    detail.push_str("Hooks\n");
    detail.push_str(&format!("Enabled: {}\n", config.hooks.enabled));
    detail.push_str(&format!("Timeout: {} ms\n", config.hooks.timeout_ms.max(1)));
    detail.push_str(&format!("Project root: {}\n", project_dir.display()));
    detail.push_str(&format!("User root: {}\n", user_dir.display()));
    detail.push('\n');
    if !config.hooks.enabled {
        detail.push_str(
            "Hooks are globally disabled; executable scripts are shown but will not fire.\n\n",
        );
    }

    let mut executable_total = 0usize;
    executable_total += render_tui_hook_root(&mut detail, "User", &user_dir)?;
    detail.push('\n');
    executable_total += render_tui_hook_root(&mut detail, "Project", &project_dir)?;

    if executable_total == 0 {
        detail.push_str("\nNo executable hook scripts found. Use /hooks events to see supported event directories.\n");
    }
    Ok(detail)
}

fn render_tui_hook_root(detail: &mut String, label: &str, root: &Path) -> AppResult<usize> {
    detail.push_str(&format!("{label} Hooks ({})\n", root.display()));
    let mut executable_total = 0usize;
    for spec in tui_hook_event_specs() {
        let dir = root.join(spec.event.dir_name());
        let exists = dir.exists();
        let entries = tui_hook_dir_entries(&dir)?;
        let executable_count = entries.iter().filter(|entry| entry.executable).count();
        executable_total += executable_count;
        if !exists {
            detail.push_str(&format!("- {}: missing\n", spec.event.dir_name()));
        } else if entries.is_empty() {
            detail.push_str(&format!("- {}: empty\n", spec.event.dir_name()));
        } else {
            let ignored_count = entries.len().saturating_sub(executable_count);
            if ignored_count == 0 {
                detail.push_str(&format!(
                    "- {}: {} executable script(s)\n",
                    spec.event.dir_name(),
                    executable_count
                ));
            } else {
                detail.push_str(&format!(
                    "- {}: {} executable script(s), {} ignored\n",
                    spec.event.dir_name(),
                    executable_count,
                    ignored_count
                ));
            }
            for entry in entries {
                let suffix = if entry.executable {
                    ""
                } else if entry.is_file {
                    " (ignored: not executable)"
                } else {
                    " (ignored: not a file)"
                };
                detail.push_str(&format!("  - {}{suffix}\n", entry.name));
            }
        }
    }
    Ok(executable_total)
}

#[derive(Debug)]
struct TuiHookDirEntry {
    name: String,
    executable: bool,
    is_file: bool,
}

fn tui_hook_dir_entries(dir: &Path) -> AppResult<Vec<TuiHookDirEntry>> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let mut out = Vec::new();
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        let name = path
            .file_name()
            .and_then(|value| value.to_str())
            .map(str::to_string)
            .unwrap_or_else(|| path.display().to_string());
        let is_file = path.is_file();
        out.push(TuiHookDirEntry {
            name,
            executable: is_tui_hook_executable_file(&path),
            is_file,
        });
    }
    out.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(out)
}

#[cfg(unix)]
fn is_tui_hook_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.is_file()
        && path
            .metadata()
            .map(|metadata| metadata.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_tui_hook_executable_file(path: &Path) -> bool {
    path.is_file()
}

fn run_tui_diagnostics_from_current_dir(app: &mut TuiApp, changed: bool, paths: Vec<String>) {
    match std::env::current_dir() {
        Ok(cwd) => run_tui_diagnostics_in(app, &cwd, changed, paths),
        Err(error) => app.set_status(format!("diagnostics failed: {error}")),
    }
}

fn run_tui_diagnostics_in(app: &mut TuiApp, cwd: &Path, changed: bool, paths: Vec<String>) {
    let files = if changed {
        match crate::cli::commands::diagnostics::changed_files(cwd) {
            Ok(files) => files,
            Err(error) => {
                app.set_status(format!("diagnostics changed files failed: {error}"));
                return;
            }
        }
    } else {
        paths
    };
    if changed && files.is_empty() {
        app.set_status("diagnostics: no changed files".to_string());
        return;
    }

    let report = crate::language::diagnostics::run_diagnostics(cwd, &files);
    let engine = if report.engine.is_empty() {
        "none"
    } else {
        report.engine.as_str()
    };
    let target = if report.checked_files.is_empty() {
        if files.is_empty() {
            "workspace".to_string()
        } else {
            format!("{} requested paths", files.len())
        }
    } else {
        format!("{} checked files", report.checked_files.len())
    };
    let mut status = format!(
        "diagnostics {} via {} ({target})",
        report.status.as_str(),
        engine
    );
    if let Some(note) = report.note.as_deref() {
        status.push_str(": ");
        status.push_str(&runtime_summary(note));
    }
    app.set_status(status);
}

fn run_tui_shell_command(app: &mut TuiApp, command: &str) {
    let input = ToolInput::new()
        .with_arg("command", command.to_string())
        .with_arg("background", "true");
    match ExecShellTool.execute(input) {
        Ok(output) => {
            let task_id = shell_task_id_from_summary(&output.summary);
            app.set_status(match task_id {
                Some(task_id) => format!("shell job started: {task_id}"),
                None => "shell job started".to_string(),
            });
            app.set_mcp_detail(
                TuiMcpDetailKind::Shell,
                format_shell_detail("Shell job started", &output.summary),
            );
        }
        Err(error) => {
            app.set_status(format!("shell job failed to start: {error}"));
        }
    }
}

fn run_tui_approved_shell_command(app: &mut TuiApp, command: &str) {
    match run_trusted_background_shell(command, ".") {
        Ok(output) => {
            let task_id = shell_task_id_from_summary(&output.summary);
            app.set_status(match task_id {
                Some(task_id) => format!("approved shell job started: {task_id}"),
                None => "approved shell job started".to_string(),
            });
            app.set_mcp_detail(
                TuiMcpDetailKind::Shell,
                format_shell_detail("Approved shell job started", &output.summary),
            );
        }
        Err(error) => {
            app.set_status(format!("approved shell job failed to start: {error}"));
        }
    }
}

fn run_tui_shell_list(app: &mut TuiApp) {
    match ExecShellListTool.execute(ToolInput::new()) {
        Ok(output) => {
            app.set_status(last_nonempty_line(&output.summary, "shell jobs listed"));
            app.set_mcp_detail(
                TuiMcpDetailKind::Shell,
                format_shell_detail("Shell jobs", &output.summary),
            );
        }
        Err(error) => {
            app.set_status(format!("shell list failed: {error}"));
        }
    }
}

fn run_tui_shell_show(app: &mut TuiApp, task_id: &str) {
    let input = ToolInput::new().with_arg("task_id", task_id.to_string());
    match ExecShellShowTool.execute(input) {
        Ok(output) => {
            app.set_status(last_nonempty_line(&output.summary, "shell job shown"));
            app.set_mcp_detail(
                TuiMcpDetailKind::Shell,
                format_shell_detail(&format!("Shell job {task_id}"), &output.summary),
            );
        }
        Err(error) => {
            app.set_status(format!("shell show failed for {task_id}: {error}"));
        }
    }
}

fn run_tui_shell_attach(app: &mut TuiApp, task_id: &str, cursor: Option<usize>, tail: bool) {
    let mut input = ToolInput::new().with_arg("task_id", task_id.to_string());
    if let Some(cursor) = cursor {
        input = input.with_arg("cursor", cursor.to_string());
    }
    if tail {
        input = input.with_arg("tail", "true");
    }
    match ExecShellAttachTool.execute(input) {
        Ok(output) => {
            app.set_status(last_nonempty_line(&output.summary, "shell attach replayed"));
            app.set_mcp_detail(
                TuiMcpDetailKind::Shell,
                format_shell_detail(&format!("Shell job {task_id} attach"), &output.summary),
            );
        }
        Err(error) => {
            app.set_status(format!("shell attach failed for {task_id}: {error}"));
        }
    }
}

fn run_tui_shell_supervisor_status(app: &mut TuiApp) {
    match ExecShellSupervisorStatusTool.execute(ToolInput::new()) {
        Ok(output) => {
            app.set_status(last_nonempty_line(
                &output.summary,
                "shell supervisor status shown",
            ));
            let inventory = match ExecShellListTool.execute(ToolInput::new()) {
                Ok(inventory) => inventory.summary,
                Err(error) => format!("job_inventory_error: {error}"),
            };
            let detail = format!("{}\n\nShell job inventory\n\n{}", output.summary, inventory);
            app.set_mcp_detail(
                TuiMcpDetailKind::Shell,
                format_shell_detail("Shell supervisor status", &detail),
            );
        }
        Err(error) => {
            app.set_status(format!("shell supervisor status failed: {error}"));
        }
    }
}

fn run_tui_shell_stdin(app: &mut TuiApp, task_id: &str, input: &str, close: bool) {
    let tool_input = ToolInput::new()
        .with_arg("task_id", task_id.to_string())
        .with_arg("input", input.to_string())
        .with_arg("close_stdin", if close { "true" } else { "false" })
        .with_arg("timeout_ms", "1000");
    match (ExecShellInteractTool {
        tool_name: "exec_shell_interact",
    })
    .execute(tool_input)
    {
        Ok(output) => {
            app.set_status(if close {
                format!("shell stdin closed: {task_id}")
            } else {
                format!("shell stdin sent: {task_id}")
            });
            app.set_mcp_detail(
                TuiMcpDetailKind::Shell,
                format_shell_detail(&format!("Shell job {task_id}"), &output.summary),
            );
        }
        Err(error) => {
            app.set_status(format!("shell stdin failed for {task_id}: {error}"));
        }
    }
}

fn run_tui_shell_wait(app: &mut TuiApp, task_id: &str, wait: bool, timeout_ms: u64) {
    let input = ToolInput::new()
        .with_arg("task_id", task_id.to_string())
        .with_arg("wait", if wait { "true" } else { "false" })
        .with_arg("timeout_ms", timeout_ms.to_string());
    match (ExecShellWaitTool {
        tool_name: "exec_shell_wait",
    })
    .execute(input)
    {
        Ok(output) => {
            app.set_status(last_nonempty_line(&output.summary, "shell wait completed"));
            app.set_mcp_detail(
                TuiMcpDetailKind::Shell,
                format_shell_detail(&format!("Shell job {task_id}"), &output.summary),
            );
        }
        Err(error) => {
            app.set_status(format!("shell wait failed for {task_id}: {error}"));
        }
    }
}

fn run_tui_shell_resize(app: &mut TuiApp, task_id: &str, rows: u16, cols: u16) {
    let input = ToolInput::new()
        .with_arg("task_id", task_id.to_string())
        .with_arg("tty_rows", rows.to_string())
        .with_arg("tty_cols", cols.to_string());
    match ExecShellResizeTool.execute(input) {
        Ok(output) => {
            app.set_status(last_nonempty_line(
                &output.summary,
                "shell resize completed",
            ));
            app.set_mcp_detail(
                TuiMcpDetailKind::Shell,
                format_shell_detail(&format!("Shell job {task_id} resize"), &output.summary),
            );
        }
        Err(error) => {
            app.set_status(format!("shell resize failed for {task_id}: {error}"));
        }
    }
}

fn run_tui_shell_cancel(app: &mut TuiApp, task_id: Option<&str>, all: bool) {
    let mut input = ToolInput::new();
    if all {
        input = input.with_arg("all", "true");
    } else if let Some(task_id) = task_id {
        input = input.with_arg("task_id", task_id.to_string());
    }
    match ExecShellCancelTool.execute(input) {
        Ok(output) => {
            app.set_status(last_nonempty_line(
                &output.summary,
                "shell cancel completed",
            ));
            app.set_mcp_detail(
                TuiMcpDetailKind::Shell,
                format_shell_detail("Shell cancel", &output.summary),
            );
        }
        Err(error) => {
            let target = task_id.unwrap_or("all");
            app.set_status(format!("shell cancel failed for {target}: {error}"));
        }
    }
}

fn shell_task_id_from_summary(summary: &str) -> Option<&str> {
    summary
        .lines()
        .find_map(|line| line.trim().strip_prefix("task_id: "))
}

fn format_shell_detail(title: &str, summary: &str) -> String {
    format!("{title}\n\n{summary}")
}

fn run_remote_tui_diagnostics(
    client: &RuntimeHttpClient,
    app: &mut TuiApp,
    changed: bool,
    paths: Vec<String>,
) {
    let body = json_object([
        ("changed", JsonValue::Bool(changed)),
        (
            "paths",
            JsonValue::Array(paths.into_iter().map(JsonValue::String).collect()),
        ),
    ]);
    match client.post_json("/v1/diagnostics", body) {
        Ok(value) => app.set_status(remote_diagnostics_status(&value)),
        Err(error) => app.set_status(format!("remote diagnostics failed: {error}")),
    }
}

fn remote_diagnostics_status(value: &JsonValue) -> String {
    let Some(root) = json_as_object(value) else {
        return "remote diagnostics returned malformed response".to_string();
    };
    if matches!(root.get("skipped"), Some(JsonValue::Bool(true))) {
        return root
            .get("message")
            .and_then(json_as_string)
            .map(|message| format!("diagnostics: {message}"))
            .unwrap_or_else(|| "diagnostics skipped".to_string());
    }
    let Some(report) = root.get("report").and_then(json_as_object) else {
        return "remote diagnostics response missing report".to_string();
    };
    let status = report
        .get("status")
        .and_then(json_as_string)
        .unwrap_or("unknown");
    let engine = report
        .get("engine")
        .and_then(json_as_string)
        .filter(|value| !value.is_empty())
        .unwrap_or("none");
    let checked_files = report
        .get("checked_files")
        .and_then(json_as_array)
        .map(|items| items.len())
        .unwrap_or(0);
    let target = if checked_files == 0 {
        "workspace".to_string()
    } else {
        format!("{checked_files} checked files")
    };
    let mut summary = format!("remote diagnostics {status} via {engine} ({target})");
    if let Some(note) = report.get("note").and_then(json_as_string) {
        summary.push_str(": ");
        summary.push_str(&runtime_summary(note));
    }
    summary
}

fn rollback_store_for_config(
    config: Option<&AppConfig>,
    app: &mut TuiApp,
) -> Option<RollbackStore> {
    let Some(config) = config else {
        app.set_status("rollback commands require local TUI config".to_string());
        return None;
    };
    Some(RollbackStore::new(
        PathBuf::from(&config.workspace.config_dir).join("rollback"),
    ))
}

fn render_rollback_snapshot_list(snapshots: &[SnapshotRecord]) -> String {
    let mut detail = String::new();
    detail.push_str("Rollback snapshots\n");
    detail.push_str(&format!("count: {}\n\n", snapshots.len()));
    for snapshot in snapshots {
        detail.push_str(&format!("{}\n", snapshot.id));
        detail.push_str(&format!("  label: {}\n", snapshot.label));
        detail.push_str(&format!("  created: {}\n", snapshot.created_at));
        detail.push_str(&format!(
            "  turn: {}\n",
            snapshot.runtime_turn_id.as_deref().unwrap_or("-")
        ));
        detail.push_str(&format!(
            "  patch bytes: {} (staged {}, unstaged {})\n",
            snapshot.patch_bytes, snapshot.staged_patch_bytes, snapshot.unstaged_patch_bytes
        ));
        detail.push_str(&format!(
            "  untracked: files {}, dirs {}, dir-metadata {}, fifos {}, sockets {}, symlinks {}\n\n",
            snapshot.untracked_files.len(),
            snapshot.untracked_directories.len(),
            snapshot.untracked_directory_metadata.len(),
            snapshot.untracked_fifos.len(),
            snapshot.untracked_sockets.len(),
            snapshot.untracked_symlinks.len()
        ));
    }
    detail
}

fn render_rollback_snapshot_detail(snapshot: &SnapshotRecord, patch: Option<&str>) -> String {
    let mut detail = String::new();
    detail.push_str("Rollback snapshot\n");
    detail.push_str(&format!("id: {}\n", snapshot.id));
    detail.push_str(&format!("label: {}\n", snapshot.label));
    detail.push_str(&format!("created: {}\n", snapshot.created_at));
    detail.push_str(&format!("workspace: {}\n", snapshot.workspace));
    detail.push_str(&format!("git root: {}\n", snapshot.git_root));
    detail.push_str(&format!("git head: {}\n", snapshot.git_head));
    detail.push_str(&format!(
        "runtime thread: {}\n",
        snapshot.runtime_thread_id.as_deref().unwrap_or("-")
    ));
    detail.push_str(&format!(
        "runtime turn: {}\n",
        snapshot.runtime_turn_id.as_deref().unwrap_or("-")
    ));
    detail.push_str(&format!("status bytes: {}\n", snapshot.status_bytes));
    detail.push_str(&format!("patch bytes: {}\n", snapshot.patch_bytes));
    detail.push_str(&format!(
        "staged patch bytes: {}\n",
        snapshot.staged_patch_bytes
    ));
    detail.push_str(&format!(
        "unstaged patch bytes: {}\n",
        snapshot.unstaged_patch_bytes
    ));
    detail.push_str(&format!("untracked bytes: {}\n", snapshot.untracked_bytes));
    detail.push_str(&format!(
        "untracked files: {}\n",
        snapshot.untracked_files.len()
    ));
    for file in snapshot.untracked_files.iter().take(20) {
        detail.push_str(&format!("  - {file}\n"));
    }
    if snapshot.untracked_files.len() > 20 {
        detail.push_str(&format!(
            "  - ... {} more\n",
            snapshot.untracked_files.len() - 20
        ));
    }
    detail.push_str(&format!(
        "untracked directories: {}\n",
        snapshot.untracked_directories.len()
    ));
    for directory in snapshot.untracked_directories.iter().take(20) {
        detail.push_str(&format!("  - {directory}\n"));
    }
    if snapshot.untracked_directories.len() > 20 {
        detail.push_str(&format!(
            "  - ... {} more\n",
            snapshot.untracked_directories.len() - 20
        ));
    }
    detail.push_str(&format!(
        "untracked directory metadata: {}\n",
        snapshot.untracked_directory_metadata.len()
    ));
    for directory in snapshot.untracked_directory_metadata.iter().take(20) {
        detail.push_str(&format!(
            "  - {} mode={:o}\n",
            directory.path, directory.mode
        ));
    }
    if snapshot.untracked_directory_metadata.len() > 20 {
        detail.push_str(&format!(
            "  - ... {} more\n",
            snapshot.untracked_directory_metadata.len() - 20
        ));
    }
    detail.push_str(&format!(
        "untracked FIFOs: {}\n",
        snapshot.untracked_fifos.len()
    ));
    for fifo in snapshot.untracked_fifos.iter().take(20) {
        detail.push_str(&format!("  - {fifo}\n"));
    }
    if snapshot.untracked_fifos.len() > 20 {
        detail.push_str(&format!(
            "  - ... {} more\n",
            snapshot.untracked_fifos.len() - 20
        ));
    }
    detail.push_str(&format!(
        "untracked sockets: {}\n",
        snapshot.untracked_sockets.len()
    ));
    for socket in snapshot.untracked_sockets.iter().take(20) {
        detail.push_str(&format!("  - {socket}\n"));
    }
    if snapshot.untracked_sockets.len() > 20 {
        detail.push_str(&format!(
            "  - ... {} more\n",
            snapshot.untracked_sockets.len() - 20
        ));
    }
    detail.push_str(&format!(
        "untracked symlinks: {}\n",
        snapshot.untracked_symlinks.len()
    ));
    for link in snapshot.untracked_symlinks.iter().take(20) {
        detail.push_str(&format!("  - {} -> {}\n", link.path, link.target));
    }
    if snapshot.untracked_symlinks.len() > 20 {
        detail.push_str(&format!(
            "  - ... {} more\n",
            snapshot.untracked_symlinks.len() - 20
        ));
    }
    if let Some(patch) = patch {
        detail.push_str("\nPatch preview:\n");
        detail.push_str(&rollback_patch_preview(patch, 80));
    }
    detail
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RollbackPatchHunk {
    file: String,
    header: String,
    prelude: Vec<String>,
    added: usize,
    removed: usize,
    lines: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RollbackHunkRestorePlan {
    snapshot_id: String,
    applied: bool,
    git_root: String,
    git_head: String,
    hunk: usize,
    hunk_count: usize,
    file: String,
    patch_bytes: u64,
    changed_files: Vec<String>,
}

fn parse_rollback_patch_hunks(patch: &str) -> Vec<RollbackPatchHunk> {
    let mut hunks = Vec::new();
    let mut current_file = String::new();
    let mut current_prelude = Vec::new();
    let mut current: Option<RollbackPatchHunk> = None;

    for line in patch.lines() {
        if line.starts_with("diff --git ") {
            if let Some(hunk) = current.take() {
                hunks.push(hunk);
            }
            current_file =
                rollback_diff_file_from_header(line).unwrap_or_else(|| current_file.clone());
            current_prelude.clear();
            current_prelude.push(line.to_string());
            continue;
        }
        if current.is_none() && !current_prelude.is_empty() && !line.starts_with("@@ ") {
            current_prelude.push(line.to_string());
        }
        if line.starts_with("+++ ") {
            if let Some(file) = rollback_diff_file_from_marker(line) {
                current_file = file;
            }
            continue;
        }
        if line.starts_with("@@ ") {
            if let Some(hunk) = current.take() {
                hunks.push(hunk);
            }
            current = Some(RollbackPatchHunk {
                file: if current_file.is_empty() {
                    "(unknown file)".to_string()
                } else {
                    current_file.clone()
                },
                header: line.to_string(),
                prelude: current_prelude.clone(),
                added: 0,
                removed: 0,
                lines: vec![line.to_string()],
            });
            continue;
        }
        if let Some(hunk) = current.as_mut() {
            if line.starts_with('+') && !line.starts_with("+++") {
                hunk.added += 1;
            } else if line.starts_with('-') && !line.starts_with("---") {
                hunk.removed += 1;
            }
            hunk.lines.push(line.to_string());
        }
    }
    if let Some(hunk) = current {
        hunks.push(hunk);
    }
    hunks
}

fn restore_rollback_hunk(
    store: &RollbackStore,
    id: &str,
    hunk: usize,
    apply: bool,
) -> AppResult<RollbackHunkRestorePlan> {
    let snapshot = store.load_snapshot_or_turn(id)?;
    let root = Path::new(&snapshot.git_root);
    let current_head = rollback_git_stdout(root, &["rev-parse", "HEAD"])?;
    let current_head = current_head.trim();
    if current_head != snapshot.git_head {
        return Err(app_error(format!(
            "snapshot {} was captured at {}, current HEAD is {}; checkout the original commit before restoring",
            snapshot.id, snapshot.git_head, current_head
        )));
    }
    let patch = store.snapshot_patch(&snapshot.id)?;
    let hunks = parse_rollback_patch_hunks(&patch);
    if hunk == 0 || hunk > hunks.len() {
        return Err(app_error(format!(
            "rollback hunk {hunk} out of range for {} (hunks={})",
            snapshot.id,
            hunks.len()
        )));
    }
    let selected = &hunks[hunk - 1];
    let hunk_patch = rollback_single_hunk_patch(selected)?;
    rollback_git_apply(root, &hunk_patch, apply)?;
    let mut changed_files = if apply {
        rollback_git_changed_files(root)?
    } else {
        vec![selected.file.clone()]
    };
    normalize_rollback_files(&mut changed_files);
    Ok(RollbackHunkRestorePlan {
        snapshot_id: snapshot.id,
        applied: apply,
        git_root: snapshot.git_root,
        git_head: snapshot.git_head,
        hunk,
        hunk_count: hunks.len(),
        file: selected.file.clone(),
        patch_bytes: hunk_patch.len() as u64,
        changed_files,
    })
}

fn rollback_single_hunk_patch(hunk: &RollbackPatchHunk) -> AppResult<String> {
    if hunk.file == "(unknown file)" {
        return Err(app_error("rollback hunk has no file path"));
    }
    if !hunk.prelude.iter().any(|line| line.starts_with("--- "))
        || !hunk.prelude.iter().any(|line| line.starts_with("+++ "))
    {
        return Err(app_error(format!(
            "rollback hunk for {} is missing file diff headers",
            hunk.file
        )));
    }
    let mut patch = String::new();
    for line in &hunk.prelude {
        patch.push_str(line);
        patch.push('\n');
    }
    for line in &hunk.lines {
        patch.push_str(line);
        patch.push('\n');
    }
    Ok(patch)
}

fn rollback_git_stdout(cwd: &Path, args: &[&str]) -> AppResult<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .map_err(|error| app_error(format!("could not invoke git: {error}")))?;
    if !output.status.success() {
        return Err(app_error(format!(
            "git {} failed: {}",
            args.first().copied().unwrap_or(""),
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn rollback_git_apply(cwd: &Path, patch: &str, apply: bool) -> AppResult<()> {
    let mut command = Command::new("git");
    command.arg("apply").arg("--binary");
    if !apply {
        command.arg("--check");
    }
    let mut child = command
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| app_error(format!("could not invoke git apply: {error}")))?;
    {
        let stdin = child
            .stdin
            .as_mut()
            .ok_or_else(|| app_error("git apply produced no stdin pipe"))?;
        stdin
            .write_all(patch.as_bytes())
            .map_err(|error| app_error(format!("failed to write patch to git apply: {error}")))?;
    }
    let output = child
        .wait_with_output()
        .map_err(|error| app_error(format!("failed to await git apply: {error}")))?;
    if !output.status.success() {
        return Err(app_error(format!(
            "git apply failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(())
}

fn rollback_git_changed_files(cwd: &Path) -> AppResult<Vec<String>> {
    let output = rollback_git_stdout(
        cwd,
        &[
            "diff",
            "--name-only",
            "--diff-filter=ACMRTUXB",
            "HEAD",
            "--",
        ],
    )?;
    let mut files = output
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();
    normalize_rollback_files(&mut files);
    Ok(files)
}

fn normalize_rollback_files(files: &mut Vec<String>) {
    files.sort();
    files.dedup();
}

fn rollback_diff_file_from_header(line: &str) -> Option<String> {
    let candidate = line.split_whitespace().nth(3)?;
    rollback_clean_diff_path(candidate)
}

fn rollback_diff_file_from_marker(line: &str) -> Option<String> {
    let candidate = line
        .strip_prefix("+++ ")
        .map(str::trim)
        .filter(|value| !value.is_empty())?;
    rollback_clean_diff_path(candidate)
}

fn rollback_clean_diff_path(path: &str) -> Option<String> {
    if path == "/dev/null" {
        return None;
    }
    Some(
        path.strip_prefix("b/")
            .or_else(|| path.strip_prefix("a/"))
            .unwrap_or(path)
            .to_string(),
    )
}

fn render_rollback_hunk_detail(
    snapshot: &SnapshotRecord,
    patch: &str,
    selected_hunk: Option<usize>,
) -> String {
    let hunks = parse_rollback_patch_hunks(patch);
    let mut detail = String::new();
    detail.push_str("Rollback patch hunks\n");
    detail.push_str(&format!("snapshot: {}\n", snapshot.id));
    detail.push_str(&format!(
        "runtime turn: {}\n",
        snapshot.runtime_turn_id.as_deref().unwrap_or("-")
    ));
    detail.push_str(&format!("patch bytes: {}\n", snapshot.patch_bytes));
    detail.push_str(&format!("hunks: {}\n\n", hunks.len()));

    if hunks.is_empty() {
        detail.push_str("No unified diff hunks found in this snapshot patch.\n");
        return detail;
    }

    if let Some(index) = selected_hunk {
        if index == 0 || index > hunks.len() {
            detail.push_str(&format!(
                "Hunk {index} not found. Valid range: 1..{}\n\n",
                hunks.len()
            ));
            detail.push_str(&rollback_hunk_list(&hunks, 80));
            return detail;
        }
        let hunk = &hunks[index - 1];
        detail.push_str(&format!("Hunk {index}/{}\n", hunks.len()));
        detail.push_str(&format!("file: {}\n", hunk.file));
        detail.push_str(&format!("header: {}\n", hunk.header));
        detail.push_str(&format!(
            "added: {} removed: {}\n\n",
            hunk.added, hunk.removed
        ));
        detail.push_str(&rollback_hunk_body(hunk, 220));
        detail.push_str(
            "\nCommands: restore hunks <id|last> | restore hunk <id|last> <index> --check | restore hunk <id|last> <index> --apply\n",
        );
        return detail;
    }

    detail.push_str(&rollback_hunk_list(&hunks, 100));
    detail.push_str(
        "\nCommands: restore hunk <id|last> <index> | restore hunk <id|last> <index> --apply\n",
    );
    detail
}

fn rollback_hunk_list(hunks: &[RollbackPatchHunk], max_hunks: usize) -> String {
    let mut out = String::new();
    for (index, hunk) in hunks.iter().take(max_hunks).enumerate() {
        out.push_str(&format!(
            "#{} {} {} (+{} -{})\n",
            index + 1,
            hunk.file,
            hunk.header,
            hunk.added,
            hunk.removed
        ));
    }
    if hunks.len() > max_hunks {
        out.push_str(&format!(
            "... {} more hunks omitted\n",
            hunks.len() - max_hunks
        ));
    }
    out
}

fn rollback_hunk_body(hunk: &RollbackPatchHunk, max_lines: usize) -> String {
    let mut out = String::new();
    for line in hunk.lines.iter().take(max_lines) {
        out.push_str(line);
        out.push('\n');
    }
    if hunk.lines.len() > max_lines {
        out.push_str(&format!(
            "... {} more hunk lines omitted\n",
            hunk.lines.len() - max_lines
        ));
    }
    out
}

fn render_rollback_restore_plan(plan: &RestorePlan) -> String {
    let mut detail = String::new();
    detail.push_str("Rollback restore plan\n");
    detail.push_str(&format!("snapshot: {}\n", plan.snapshot_id));
    detail.push_str(&format!(
        "mode: {}\n",
        if plan.applied { "applied" } else { "dry-run" }
    ));
    detail.push_str(&format!("git root: {}\n", plan.git_root));
    detail.push_str(&format!("git head: {}\n", plan.git_head));
    detail.push_str(&format!("snapshot patch bytes: {}\n", plan.patch_bytes));
    detail.push_str(&format!(
        "staged patch bytes: {}\n",
        plan.staged_patch_bytes
    ));
    detail.push_str(&format!(
        "unstaged patch bytes: {}\n",
        plan.unstaged_patch_bytes
    ));
    detail.push_str(&format!(
        "current patch bytes: {}\n",
        plan.current_patch_bytes
    ));
    if plan.applied {
        detail.push_str(&format!("changed files: {}\n", plan.changed_files.len()));
        for file in plan.changed_files.iter().take(40) {
            detail.push_str(&format!("  - {file}\n"));
        }
        if plan.changed_files.len() > 40 {
            detail.push_str(&format!("  - ... {} more\n", plan.changed_files.len() - 40));
        }
    } else {
        detail.push_str("\nDry-run only. Re-run `revert turn <id> --apply` to restore files.\n");
    }
    detail
}

fn render_rollback_hunk_restore_plan(plan: &RollbackHunkRestorePlan) -> String {
    let mut detail = String::new();
    detail.push_str("Rollback hunk restore plan\n");
    detail.push_str(&format!("snapshot: {}\n", plan.snapshot_id));
    detail.push_str(&format!("hunk: {}/{}\n", plan.hunk, plan.hunk_count));
    detail.push_str(&format!("file: {}\n", plan.file));
    detail.push_str(&format!("applied: {}\n", plan.applied));
    detail.push_str(&format!("git_root: {}\n", plan.git_root));
    detail.push_str(&format!("git_head: {}\n", plan.git_head));
    detail.push_str(&format!("patch_bytes: {}\n", plan.patch_bytes));
    if plan.changed_files.is_empty() {
        detail.push_str("changed files: 0\n");
    } else {
        detail.push_str(&format!("changed files: {}\n", plan.changed_files.len()));
        for file in &plan.changed_files {
            detail.push_str(&format!("- {file}\n"));
        }
    }
    if !plan.applied {
        detail.push_str(
            "\nDry-run only. Use `restore hunk <id|last> <index> --apply` to apply this hunk.\n",
        );
    }
    detail
}

fn rollback_patch_preview(patch: &str, max_lines: usize) -> String {
    if patch.trim().is_empty() {
        return "(empty patch)\n".to_string();
    }
    let mut out = String::new();
    for line in patch.lines().take(max_lines) {
        out.push_str(line);
        out.push('\n');
    }
    let total_lines = patch.lines().count();
    if total_lines > max_lines {
        out.push_str(&format!(
            "... {} more patch lines omitted\n",
            total_lines - max_lines
        ));
    }
    out
}

fn start_tui_agent_run(
    store: RuntimeStore,
    config: AppConfig,
    thread_id: String,
    prompt: String,
    reasoning_replay_limit: usize,
    reasoning_replay_pinned_turn_ids: Vec<String>,
    translation_target_language: Option<String>,
    live_tx: Option<Sender<TuiLiveEvent>>,
) {
    let _ = thread::spawn(move || {
        if let Err(error) = run_tui_agent_turn(
            store.clone(),
            config,
            thread_id.clone(),
            prompt,
            reasoning_replay_limit,
            reasoning_replay_pinned_turn_ids,
            translation_target_language,
            live_tx,
        ) {
            let _ = record_tui_agent_failure(&store, &thread_id, &error.to_string());
        }
    });
}

fn run_tui_agent_turn(
    store: RuntimeStore,
    config: AppConfig,
    thread_id: String,
    prompt: String,
    reasoning_replay_limit: usize,
    reasoning_replay_pinned_turn_ids: Vec<String>,
    translation_target_language: Option<String>,
    live_tx: Option<Sender<TuiLiveEvent>>,
) -> AppResult<()> {
    let model = config.model.model.clone();
    let rollback_store =
        RollbackStore::new(PathBuf::from(&config.workspace.config_dir).join("rollback"));
    let rollback_snapshot_id = create_tui_rollback_snapshot(&rollback_store, &prompt);
    let assistant = store.append_turn(
        &thread_id,
        "assistant".to_string(),
        "(assistant response running)".to_string(),
    )?;
    if let Some(snapshot_id) = rollback_snapshot_id.as_deref() {
        let _ = rollback_store.bind_snapshot_runtime(
            snapshot_id,
            Some(&thread_id),
            Some(&assistant.id),
        );
    }
    let assistant_item = store.append_item(
        &thread_id,
        Some(&assistant.id),
        "message".to_string(),
        Some("assistant".to_string()),
        "".to_string(),
        "running".to_string(),
    )?;
    store.update_turn(
        &thread_id,
        &assistant.id,
        "(assistant response running)".to_string(),
        "running".to_string(),
    )?;
    let thread = store.load_thread(&thread_id)?;
    let running_task = store.create_task(
        thread.session_id.as_deref(),
        Some(&thread_id),
        None,
        "agent".to_string(),
        "running".to_string(),
        format!("agent run: {}", runtime_summary(&prompt)),
    )?;
    let cancel_since_seq = store.load_thread(&thread_id)?.event_seq;
    let resolver: SharedAgentApprovalResolver = Rc::new(RefCell::new(RuntimeApprovalResolver {
        store: store.clone(),
        thread_id: thread_id.clone(),
        turn_id: assistant.id.clone(),
        cancel_since_seq,
        poll_interval: Duration::from_millis(250),
    }));
    let user_input_resolver: SharedAgentUserInputResolver =
        Rc::new(RefCell::new(RuntimeUserInputResolver {
            store: store.clone(),
            thread_id: thread_id.clone(),
            turn_id: Some(assistant.id.clone()),
            cancel_since_seq: Some(cancel_since_seq),
            poll_interval: Duration::from_millis(250),
            max_polls: None,
        }));
    let cancel_check: SharedAgentCancelCheck = Rc::new(RefCell::new(RuntimeCancelCheck {
        store: store.clone(),
        thread_id: thread_id.clone(),
        turn_id: assistant.id.clone(),
        since_seq: cancel_since_seq,
    }));
    let stream_events = RuntimeItemStream::new(
        store.clone(),
        thread_id.clone(),
        assistant.id.clone(),
        assistant_item.id.clone(),
        live_tx.clone(),
    );
    let live_tool_result_count = Rc::new(RefCell::new(0_usize));
    let run_events: SharedAgentRunEvents = Rc::new(RefCell::new(RuntimeToolRunEvents::new(
        store.clone(),
        thread_id.clone(),
        assistant.id.clone(),
        live_tx,
        Rc::clone(&live_tool_result_count),
    )));
    let translation_config = config.clone();
    let agent = AgentLoop::new(config);
    let mut context = TaskContext::new(prompt, None);
    let translation_target_for_result = translation_target_language.clone();
    if let Some(target_language) = translation_target_language {
        context = context.with_translation_target_language(target_language);
    }
    let mut result = match agent.run_with(
        context,
        AgentLoopOptions {
            emit_progress: false,
            initial_recent_steps: store.reasoning_replay_entries_with_pinned_turns(
                &thread_id,
                reasoning_replay_limit,
                &reasoning_replay_pinned_turn_ids,
            )?,
            persist_session: false,
            stream_events: Some(Box::new(stream_events)),
            run_events: Some(run_events),
            approval_resolver: Some(resolver),
            user_input_resolver: Some(user_input_resolver),
            cancel_check: Some(cancel_check),
            ..AgentLoopOptions::default()
        },
    ) {
        Ok(result) => result,
        Err(error) => {
            if is_cancelled_error(&error.to_string()) {
                record_tui_agent_cancelled_into(
                    &store,
                    &thread_id,
                    &assistant.id,
                    &assistant_item.id,
                    Some(&running_task.id),
                )?;
            } else {
                record_tui_agent_failure_into(
                    &store,
                    &thread_id,
                    &assistant.id,
                    &assistant_item.id,
                    Some(&running_task.id),
                    &error.to_string(),
                )?;
            }
            return Ok(());
        }
    };
    apply_tui_posthoc_translation(
        &translation_config,
        &mut result,
        translation_target_for_result.as_deref(),
    );
    let live_tool_results = *live_tool_result_count.borrow();
    record_tui_agent_result_into(
        &store,
        &thread_id,
        &assistant.id,
        &assistant_item.id,
        Some(&running_task.id),
        &model,
        &result,
        live_tool_results,
    )?;
    Ok(())
}

fn apply_tui_posthoc_translation(
    config: &AppConfig,
    result: &mut RunResult,
    target_language: Option<&str>,
) {
    let Some(target_language) = target_language
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return;
    };
    if !tui_output_needs_translation(&result.final_message, target_language) {
        return;
    }

    let client = DeepSeekClient {
        config: config.model.clone(),
    };
    match client.translate_text(&result.final_message, target_language) {
        Ok(translated) if !translated.trim().is_empty() => {
            result.final_message = translated.trim().to_string();
            result.tool_events.push(translation_tool_event(
                target_language,
                "translated final assistant message",
                ObservationStatus::Ok,
            ));
        }
        Ok(_) => {
            result.tool_events.push(translation_tool_event(
                target_language,
                "translation returned empty output; kept original final assistant message",
                ObservationStatus::Failed,
            ));
        }
        Err(error) => {
            result.tool_events.push(translation_tool_event(
                target_language,
                &format!("translation failed; kept original final assistant message: {error}"),
                ObservationStatus::Failed,
            ));
        }
    }
}

fn translation_tool_event(
    target_language: &str,
    output: &str,
    status: ObservationStatus,
) -> ToolEvent {
    ToolEvent {
        tool_name: "posthoc_translate".to_string(),
        input: BTreeMap::from([("target_language".to_string(), target_language.to_string())]),
        output: output.to_string(),
        status,
    }
}

fn tui_output_needs_translation(text: &str, target_language: &str) -> bool {
    if target_language.eq_ignore_ascii_case("english") {
        return false;
    }
    let mut latin_count = 0usize;
    let mut cjk_count = 0usize;
    for ch in text.chars() {
        if ch.is_ascii_alphabetic() {
            latin_count += 1;
        } else if is_cjk_translation_char(ch) {
            cjk_count += 1;
        }
    }
    let weighted_total = latin_count + cjk_count.saturating_mul(3);
    if weighted_total < 10 {
        return false;
    }
    if cjk_count.saturating_mul(3) > latin_count {
        return false;
    }
    (latin_count as f64 / weighted_total as f64) >= 0.6
}

fn is_cjk_translation_char(ch: char) -> bool {
    matches!(
        ch,
        '\u{4E00}'..='\u{9FFF}'
            | '\u{3400}'..='\u{4DBF}'
            | '\u{2E80}'..='\u{2EFF}'
            | '\u{3000}'..='\u{303F}'
            | '\u{FF00}'..='\u{FFEF}'
            | '\u{3040}'..='\u{309F}'
            | '\u{30A0}'..='\u{30FF}'
    )
}

fn create_tui_rollback_snapshot(store: &RollbackStore, prompt: &str) -> Option<String> {
    let workspace = std::env::current_dir().ok()?;
    let label = format!("tui rollback: {}", runtime_summary(prompt));
    store
        .create_snapshot(&workspace, label)
        .ok()
        .map(|snapshot| snapshot.id)
}

struct RuntimeItemStream {
    store: RuntimeStore,
    thread_id: String,
    turn_id: String,
    item_id: String,
    content: String,
    reasoning_item_id: Option<String>,
    reasoning: String,
    live_tx: Option<Sender<TuiLiveEvent>>,
}

impl RuntimeItemStream {
    fn new(
        store: RuntimeStore,
        thread_id: String,
        turn_id: String,
        item_id: String,
        live_tx: Option<Sender<TuiLiveEvent>>,
    ) -> Self {
        Self {
            store,
            thread_id,
            turn_id,
            item_id,
            content: String::new(),
            reasoning_item_id: None,
            reasoning: String::new(),
            live_tx,
        }
    }

    fn flush_running(&self) {
        if let Ok(item) = self.store.update_item(
            &self.thread_id,
            &self.item_id,
            self.content.clone(),
            "running".to_string(),
        ) {
            self.emit_live_item(item);
        }
    }

    fn flush_reasoning(&mut self, status: &str) {
        if self.reasoning.trim().is_empty() {
            return;
        }
        if let Some(item_id) = self.reasoning_item_id.as_deref() {
            if let Ok(item) = self.store.update_item(
                &self.thread_id,
                item_id,
                self.reasoning.clone(),
                status.to_string(),
            ) {
                self.emit_live_item(item);
            }
            return;
        }
        if let Ok(item) = self.store.append_item(
            &self.thread_id,
            Some(&self.turn_id),
            "reasoning".to_string(),
            Some("assistant".to_string()),
            self.reasoning.clone(),
            status.to_string(),
        ) {
            self.reasoning_item_id = Some(item.id.clone());
            self.emit_live_item(item);
        }
    }

    fn emit_live_item(&self, item: crate::core::runtime::ItemRecord) {
        if let Some(tx) = self.live_tx.as_ref() {
            let _ = tx.send(TuiLiveEvent::UpsertItem(TuiItem::from(item)));
        }
    }
}

struct RuntimeToolRunEvents {
    store: RuntimeStore,
    thread_id: String,
    turn_id: String,
    live_tx: Option<Sender<TuiLiveEvent>>,
    active_tool_call_item_id: Option<String>,
    persisted_tool_results: Rc<RefCell<usize>>,
}

impl RuntimeToolRunEvents {
    fn new(
        store: RuntimeStore,
        thread_id: String,
        turn_id: String,
        live_tx: Option<Sender<TuiLiveEvent>>,
        persisted_tool_results: Rc<RefCell<usize>>,
    ) -> Self {
        Self {
            store,
            thread_id,
            turn_id,
            live_tx,
            active_tool_call_item_id: None,
            persisted_tool_results,
        }
    }

    fn emit_live_item(&self, item: ItemRecord) {
        if let Some(tx) = self.live_tx.as_ref() {
            let _ = tx.send(TuiLiveEvent::UpsertItem(TuiItem::from(item)));
        }
    }

    fn update_active_tool_call(
        &self,
        tool_name: &str,
        input: &BTreeMap<String, String>,
        status: &str,
        permission: Option<(&str, &str)>,
    ) {
        let Some(item_id) = self.active_tool_call_item_id.as_deref() else {
            return;
        };
        if let Ok(item) = self.store.update_item(
            &self.thread_id,
            item_id,
            format_live_tool_call_item(tool_name, input, status, permission),
            status.to_string(),
        ) {
            self.emit_live_item(item);
        }
    }
}

impl AgentRunEvents for RuntimeToolRunEvents {
    fn on_tool_call(&mut self, tool_name: &str, input: &BTreeMap<String, String>) {
        match self.store.append_item(
            &self.thread_id,
            Some(&self.turn_id),
            "tool_call".to_string(),
            Some("tool".to_string()),
            format_live_tool_call_item(tool_name, input, "running", None),
            "running".to_string(),
        ) {
            Ok(item) => {
                self.active_tool_call_item_id = Some(item.id.clone());
                self.emit_live_item(item);
            }
            Err(_) => {
                self.active_tool_call_item_id = None;
            }
        }
    }

    fn on_permission_request(
        &mut self,
        tool_name: &str,
        input: &BTreeMap<String, String>,
        kind: &str,
        target: &str,
    ) {
        self.update_active_tool_call(tool_name, input, "pending", Some((kind, target)));
    }

    fn on_tool_result(&mut self, event: &ToolEvent) {
        let status = tool_item_status(event);
        self.update_active_tool_call(&event.tool_name, &event.input, &status, None);
        if let Ok(item) = self.store.append_item(
            &self.thread_id,
            Some(&self.turn_id),
            "tool_result".to_string(),
            Some("tool".to_string()),
            format_tool_event(event),
            status,
        ) {
            *self.persisted_tool_results.borrow_mut() += 1;
            self.emit_live_item(item);
        }
        self.active_tool_call_item_id = None;
    }
}

impl StreamEvents for RuntimeItemStream {
    fn on_reasoning_delta(&mut self, chunk: &str) {
        if chunk.is_empty() {
            return;
        }
        self.reasoning.push_str(chunk);
        self.flush_reasoning("running");
    }

    fn on_text_delta(&mut self, chunk: &str) {
        if chunk.is_empty() {
            return;
        }
        self.content.push_str(chunk);
        self.flush_running();
    }

    fn on_assistant_done(&mut self, full_text: &str) {
        if !full_text.is_empty() {
            self.content = full_text.to_string();
        }
        self.flush_running();
        self.flush_reasoning("completed");
    }

    fn on_tool_call(&mut self, _name: &str, _input: &std::collections::BTreeMap<String, String>) {}
}

struct RuntimeApprovalResolver {
    store: RuntimeStore,
    thread_id: String,
    turn_id: String,
    cancel_since_seq: u64,
    poll_interval: Duration,
}

impl AgentApprovalResolver for RuntimeApprovalResolver {
    fn resolve(&mut self, request: &AgentApprovalRequest) -> AppResult<AgentApprovalDecision> {
        let approval = self.store.append_permission_request(
            &self.thread_id,
            Some(&self.turn_id),
            request.tool_name.clone(),
            request.kind.clone(),
            request.target.clone(),
            request.input.clone(),
        )?;
        if let Some(decision) =
            cached_runtime_permission_decision(&self.store, &self.thread_id, &approval)?
        {
            self.store.append_permission_response_with_scope(
                &self.thread_id,
                Some(&self.turn_id),
                approval.id.clone(),
                agent_approval_decision_label(decision).to_string(),
                Some("cached".to_string()),
            )?;
            return Ok(decision);
        }
        loop {
            if runtime_cancel_requested(
                &self.store,
                &self.thread_id,
                &self.turn_id,
                self.cancel_since_seq,
            )? {
                return Err(app_error("agent run cancelled"));
            }
            for event in self.store.read_events(&self.thread_id, approval.seq)? {
                if let Some(decision) = approval_response_decision(&event, &approval.id) {
                    return Ok(decision);
                }
            }
            thread::sleep(self.poll_interval);
        }
    }
}

#[derive(Debug, Clone)]
struct RuntimePermissionKeys {
    fingerprint: String,
    grouping_fingerprint: String,
}

fn cached_runtime_permission_decision(
    store: &RuntimeStore,
    thread_id: &str,
    approval: &RuntimeEvent,
) -> AppResult<Option<AgentApprovalDecision>> {
    let Some(current) = runtime_permission_request_keys(approval) else {
        return Ok(None);
    };
    let mut requests = BTreeMap::<String, RuntimePermissionKeys>::new();
    for event in store.read_events(thread_id, 0)? {
        if event.seq >= approval.seq {
            break;
        }
        if let Some(keys) = runtime_permission_request_keys(&event) {
            requests.insert(event.id.clone(), keys);
            continue;
        }
        let Some((request_id, decision, scope)) = runtime_permission_response(&event) else {
            continue;
        };
        let Some(keys) = requests.get(&request_id) else {
            continue;
        };
        match decision {
            AgentApprovalDecision::Approved
                if scope.as_deref() == Some("session")
                    && keys.grouping_fingerprint == current.grouping_fingerprint =>
            {
                return Ok(Some(AgentApprovalDecision::Approved));
            }
            AgentApprovalDecision::Denied if keys.fingerprint == current.fingerprint => {
                return Ok(Some(AgentApprovalDecision::Denied));
            }
            _ => {}
        }
    }
    Ok(None)
}

fn runtime_permission_request_keys(event: &RuntimeEvent) -> Option<RuntimePermissionKeys> {
    if event.kind != "permission_request" {
        return None;
    }
    let payload = json_as_object(&event.payload)?;
    let fingerprint = payload.get("fingerprint").and_then(json_as_string)?;
    let grouping_fingerprint = payload
        .get("grouping_fingerprint")
        .and_then(json_as_string)
        .unwrap_or(fingerprint);
    Some(RuntimePermissionKeys {
        fingerprint: fingerprint.to_string(),
        grouping_fingerprint: grouping_fingerprint.to_string(),
    })
}

fn runtime_permission_response(
    event: &RuntimeEvent,
) -> Option<(String, AgentApprovalDecision, Option<String>)> {
    if event.kind != "permission_response" {
        return None;
    }
    let payload = json_as_object(&event.payload)?;
    let request_id = payload.get("request_id").and_then(json_as_string)?;
    let decision = match payload.get("decision").and_then(json_as_string)? {
        "approved" => AgentApprovalDecision::Approved,
        "denied" => AgentApprovalDecision::Denied,
        _ => return None,
    };
    let scope = payload
        .get("scope")
        .and_then(json_as_string)
        .map(str::to_string);
    Some((request_id.to_string(), decision, scope))
}

fn agent_approval_decision_label(decision: AgentApprovalDecision) -> &'static str {
    match decision {
        AgentApprovalDecision::Approved => "approved",
        AgentApprovalDecision::Denied => "denied",
    }
}

struct RuntimeUserInputResolver {
    store: RuntimeStore,
    thread_id: String,
    turn_id: Option<String>,
    cancel_since_seq: Option<u64>,
    poll_interval: Duration,
    max_polls: Option<usize>,
}

impl AgentUserInputResolver for RuntimeUserInputResolver {
    fn resolve(&mut self, request: &AgentUserInputRequest) -> AppResult<AgentUserInputResponse> {
        let raw_questions = request
            .input
            .get("questions")
            .ok_or_else(|| app_error("request_user_input requires `questions`"))?;
        let questions = parse_json_value(raw_questions.trim())
            .map_err(|error| app_error(format!("Invalid request_user_input payload: {error}")))?;
        let user_input = self.store.append_user_input_request(
            &self.thread_id,
            self.turn_id.as_deref(),
            questions,
        )?;
        let mut polls = 0_usize;
        loop {
            if let (Some(turn_id), Some(since_seq)) =
                (self.turn_id.as_deref(), self.cancel_since_seq)
            {
                if runtime_cancel_requested(&self.store, &self.thread_id, turn_id, since_seq)? {
                    return Err(app_error("agent run cancelled"));
                }
            }
            for event in self.store.read_events(&self.thread_id, user_input.seq)? {
                if let Some(answers) = user_input_response_answers(&event, &user_input.id) {
                    return Ok(AgentUserInputResponse { answers });
                }
            }
            polls = polls.saturating_add(1);
            if self.max_polls.is_some_and(|max_polls| polls >= max_polls) {
                return Err(app_error(format!(
                    "timed out waiting for user input response {}",
                    user_input.id
                )));
            }
            thread::sleep(self.poll_interval);
        }
    }
}

struct RuntimeCancelCheck {
    store: RuntimeStore,
    thread_id: String,
    turn_id: String,
    since_seq: u64,
}

impl AgentCancelCheck for RuntimeCancelCheck {
    fn is_cancelled(&mut self) -> AppResult<bool> {
        runtime_cancel_requested(&self.store, &self.thread_id, &self.turn_id, self.since_seq)
    }
}

fn runtime_cancel_requested(
    store: &RuntimeStore,
    thread_id: &str,
    turn_id: &str,
    since_seq: u64,
) -> AppResult<bool> {
    Ok(store
        .read_events(thread_id, since_seq)?
        .iter()
        .any(|event| event.kind == "cancel_requested" && event.turn_id.as_deref() == Some(turn_id)))
}

fn user_input_response_answers(
    event: &RuntimeEvent,
    request_id: &str,
) -> Option<BTreeMap<String, String>> {
    if event.kind != "user_input_response" {
        return None;
    }
    let payload = json_as_object(&event.payload)?;
    let response_request_id = payload.get("request_id").and_then(json_as_string)?;
    if response_request_id != request_id {
        return None;
    }
    let answers = payload.get("answers").and_then(json_as_object)?;
    let answers = answers
        .iter()
        .filter_map(|(key, value)| Some((key.clone(), json_as_string(value)?.to_string())))
        .collect::<BTreeMap<_, _>>();
    if answers.is_empty() {
        None
    } else {
        Some(answers)
    }
}

fn approval_response_decision(
    event: &RuntimeEvent,
    request_id: &str,
) -> Option<AgentApprovalDecision> {
    if event.kind != "permission_response" {
        return None;
    }
    let payload = json_as_object(&event.payload)?;
    let response_request_id = payload.get("request_id").and_then(json_as_string)?;
    if response_request_id != request_id {
        return None;
    }
    match payload.get("decision").and_then(json_as_string)? {
        "approved" => Some(AgentApprovalDecision::Approved),
        "denied" => Some(AgentApprovalDecision::Denied),
        _ => None,
    }
}

#[cfg(test)]
fn record_tui_agent_result(
    store: &RuntimeStore,
    thread_id: &str,
    model: &str,
    result: &RunResult,
) -> AppResult<()> {
    let message = non_empty_agent_message(&result.final_message);
    let assistant = store.append_turn(thread_id, "assistant".to_string(), message.clone())?;
    let assistant_item = store.append_item(
        thread_id,
        Some(&assistant.id),
        "message".to_string(),
        Some("assistant".to_string()),
        message.clone(),
        "running".to_string(),
    )?;
    record_tui_agent_result_into(
        store,
        thread_id,
        &assistant.id,
        &assistant_item.id,
        None,
        model,
        result,
        0,
    )
}

fn record_tui_agent_result_into(
    store: &RuntimeStore,
    thread_id: &str,
    assistant_turn_id: &str,
    assistant_item_id: &str,
    task_id: Option<&str>,
    model: &str,
    result: &RunResult,
    already_persisted_tool_results: usize,
) -> AppResult<()> {
    let message = non_empty_agent_message(&result.final_message);
    store.update_turn(
        thread_id,
        assistant_turn_id,
        message.clone(),
        "completed".to_string(),
    )?;
    store.update_item(
        thread_id,
        assistant_item_id,
        message.clone(),
        "completed".to_string(),
    )?;
    for event in result
        .tool_events
        .iter()
        .skip(already_persisted_tool_results)
    {
        store.append_item(
            thread_id,
            Some(assistant_turn_id),
            "tool_result".to_string(),
            Some("tool".to_string()),
            format_tool_event(event),
            tool_item_status(event),
        )?;
    }
    let usage_model = result.usage.model.as_deref().unwrap_or(model);
    store.append_usage_with_cache(
        thread_id,
        Some(assistant_turn_id),
        usage_model.to_string(),
        "tui".to_string(),
        result.usage.prompt,
        result.usage.completion,
        result.usage.prompt_cache_hit,
        result.usage.prompt_cache_miss,
    )?;
    finish_tui_agent_task(store, thread_id, task_id, "completed", message)?;
    Ok(())
}

fn record_tui_agent_failure(store: &RuntimeStore, thread_id: &str, error: &str) -> AppResult<()> {
    let message = format!("agent run failed: {error}");
    let assistant = store.append_turn(thread_id, "assistant".to_string(), message.clone())?;
    let assistant_item = store.append_item(
        thread_id,
        Some(&assistant.id),
        "message".to_string(),
        Some("assistant".to_string()),
        message.clone(),
        "running".to_string(),
    )?;
    record_tui_agent_failure_into(
        store,
        thread_id,
        &assistant.id,
        &assistant_item.id,
        None,
        error,
    )
}

fn record_tui_agent_failure_into(
    store: &RuntimeStore,
    thread_id: &str,
    assistant_turn_id: &str,
    assistant_item_id: &str,
    task_id: Option<&str>,
    error: &str,
) -> AppResult<()> {
    let message = format!("agent run failed: {error}");
    store.update_turn(
        thread_id,
        assistant_turn_id,
        message.clone(),
        "failed".to_string(),
    )?;
    store.update_item(
        thread_id,
        assistant_item_id,
        message.clone(),
        "failed".to_string(),
    )?;
    finish_tui_agent_task(store, thread_id, task_id, "failed", message)?;
    Ok(())
}

fn record_tui_agent_cancelled_into(
    store: &RuntimeStore,
    thread_id: &str,
    assistant_turn_id: &str,
    assistant_item_id: &str,
    task_id: Option<&str>,
) -> AppResult<()> {
    let running_item = store.load_item(thread_id, assistant_item_id)?;
    let message = if running_item.content.trim().is_empty() {
        "agent run cancelled".to_string()
    } else {
        format!("{}\n\n(agent run cancelled)", running_item.content)
    };
    store.update_turn(
        thread_id,
        assistant_turn_id,
        message.clone(),
        "cancelled".to_string(),
    )?;
    store.update_item(
        thread_id,
        assistant_item_id,
        message.clone(),
        "cancelled".to_string(),
    )?;
    finish_tui_agent_task(store, thread_id, task_id, "cancelled", message)?;
    Ok(())
}

fn finish_tui_agent_task(
    store: &RuntimeStore,
    thread_id: &str,
    task_id: Option<&str>,
    status: &str,
    summary: String,
) -> AppResult<()> {
    if let Some(task_id) = task_id {
        store.update_task(task_id, status.to_string(), summary)?;
        return Ok(());
    }
    let thread = store.load_thread(thread_id)?;
    store.create_task(
        thread.session_id.as_deref(),
        Some(thread_id),
        None,
        "agent".to_string(),
        status.to_string(),
        summary,
    )?;
    Ok(())
}

fn non_empty_agent_message(message: &str) -> String {
    let trimmed = message.trim();
    if trimmed.is_empty() {
        "(assistant finished without a final message)".to_string()
    } else {
        message.to_string()
    }
}

fn runtime_summary(value: &str) -> String {
    let mut summary = value.lines().next().unwrap_or("").trim().to_string();
    if summary.chars().count() > 80 {
        summary = summary.chars().take(80).collect::<String>();
        summary.push_str("...");
    }
    if summary.is_empty() {
        "(empty prompt)".to_string()
    } else {
        summary
    }
}

fn last_nonempty_line(value: &str, fallback: &str) -> String {
    value
        .lines()
        .rev()
        .find_map(|line| {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        })
        .unwrap_or_else(|| fallback.to_string())
}

fn is_cancelled_error(error: &str) -> bool {
    error.contains("agent run cancelled")
}

fn format_tool_event(event: &ToolEvent) -> String {
    let status = match event.status {
        crate::model::protocol::ObservationStatus::Ok => "ok",
        crate::model::protocol::ObservationStatus::Failed => "failed",
    };
    let input = format_tool_input(&event.input);
    format!(
        "tool: {}\nstatus: {status}\ninput: {input}\n{}",
        event.tool_name, event.output
    )
}

fn format_live_tool_call_item(
    tool_name: &str,
    input: &BTreeMap<String, String>,
    status: &str,
    permission: Option<(&str, &str)>,
) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "tool: {tool_name}");
    if let Some(target) = live_tool_target(tool_name, input) {
        let _ = writeln!(out, "target: {target}");
    }
    let _ = writeln!(out, "status: {status}");
    if let Some((kind, target)) = permission {
        let _ = writeln!(out, "approval: {kind} {}", clip_cli_line(target, 120));
    }
    let _ = write!(out, "input: {}", format_tool_input(input));
    out
}

fn live_tool_target(tool_name: &str, input: &BTreeMap<String, String>) -> Option<String> {
    if matches!(tool_name, "run_shell" | "exec_shell" | "task_shell_start") {
        return input
            .get("command")
            .map(|command| concise_shell_command_label(command, 100));
    }
    input
        .get("path")
        .or_else(|| input.get("target"))
        .or_else(|| input.get("query"))
        .map(|value| clip_cli_line(value, 100))
}

fn concise_shell_command_label(command: &str, max_chars: usize) -> String {
    let normalized = command.split_whitespace().collect::<Vec<_>>().join(" ");
    if let Some(label) = concise_gh_command_label(&normalized) {
        return clip_cli_line(&label, max_chars);
    }
    let segmented = normalized
        .replace("&&", "\n")
        .replace("||", "\n")
        .replace('|', "\n");
    let segment = segmented
        .split(['\n', ';'])
        .map(str::trim)
        .find(|segment| {
            !segment.is_empty()
                && !segment.starts_with("cd ")
                && !segment.starts_with("sleep ")
                && !segment.starts_with("export ")
                && *segment != "true"
                && *segment != ":"
        })
        .unwrap_or(&normalized);
    clip_cli_line(segment, max_chars)
}

fn concise_gh_command_label(command: &str) -> Option<String> {
    let tokens = command
        .split_whitespace()
        .map(|token| {
            token
                .trim_matches(|ch: char| matches!(ch, '\'' | '"' | '(' | ')' | ';' | ','))
                .to_string()
        })
        .filter(|token| !token.is_empty())
        .collect::<Vec<_>>();
    for index in 0..tokens.len() {
        let token = tokens[index].as_str();
        if token != "gh" && !token.ends_with("/gh") {
            continue;
        }
        let Some(area) = tokens.get(index + 1).map(String::as_str) else {
            continue;
        };
        let Some(action) = tokens.get(index + 2).map(String::as_str) else {
            continue;
        };
        if !matches!(area, "pr" | "run")
            || !matches!(
                action,
                "checks" | "view" | "status" | "list" | "watch" | "rerun"
            )
        {
            continue;
        }
        let mut label = format!("gh {area} {action}");
        if let Some(target) = tokens
            .iter()
            .skip(index + 3)
            .map(String::as_str)
            .find(|token| !token.starts_with('-') && *token != "&&" && *token != ";")
        {
            label.push(' ');
            label.push_str(target);
        }
        return Some(label);
    }
    None
}

fn format_tool_input(input: &BTreeMap<String, String>) -> String {
    if input.is_empty() {
        "{}".to_string()
    } else {
        input
            .iter()
            .map(|(key, value)| format!("{key}={value}"))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

fn clip_cli_line(value: &str, max_chars: usize) -> String {
    let line = value.lines().next().unwrap_or("").trim();
    if line.chars().count() <= max_chars {
        line.to_string()
    } else {
        format!("{}...", line.chars().take(max_chars).collect::<String>())
    }
}

fn tool_item_status(event: &ToolEvent) -> String {
    match event.status {
        crate::model::protocol::ObservationStatus::Ok => "completed".to_string(),
        crate::model::protocol::ObservationStatus::Failed => "failed".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::fs;
    use std::net::TcpListener;
    use std::path::Path;
    use std::process::Command;
    use std::sync::{Mutex, OnceLock};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_root(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "deepseek-tui-runtime-{label}-{}-{nanos}",
            std::process::id()
        ))
    }

    fn temp_store(label: &str) -> RuntimeStore {
        RuntimeStore::new(temp_root(label))
    }

    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn shell_tool_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    #[test]
    fn entrypoint_smoke_report_detects_tui_terminal_takeover() {
        let report = entrypoint_smoke_report(
            Path::new("/tmp/deepseek"),
            EntrypointSmokeOutput {
                backend: "script-linux".to_string(),
                status_code: Some(0),
                timed_out: false,
                stdout: "\x1b[?1049h DeepSeekCode TUI \x1b[?1049l".to_string(),
                stderr: String::new(),
            },
        );
        assert!(report.ok);
        assert!(report.entered_alternate_screen);
        assert!(report.left_alternate_screen);
        assert!(report.rendered_tui);
        let json = json_value_to_string(&entrypoint_smoke_json(&report));
        assert!(json.contains("\"schema\":\"deepseek.tui.entrypoint_smoke.v1\""));
        assert!(json.contains("\"ok\":true"));
    }

    #[test]
    fn entrypoint_smoke_report_fails_without_alternate_screen() {
        let report = entrypoint_smoke_report(
            Path::new("/tmp/deepseek"),
            EntrypointSmokeOutput {
                backend: "script-linux".to_string(),
                status_code: Some(0),
                timed_out: false,
                stdout: "DeepSeekCode TUI".to_string(),
                stderr: String::new(),
            },
        );
        assert!(!report.ok);
        assert!(!report.entered_alternate_screen);
        assert!(!report.left_alternate_screen);
        assert!(report.rendered_tui);
    }

    #[cfg(unix)]
    #[test]
    fn entrypoint_smoke_shell_quote_escapes_single_quotes() {
        assert_eq!(
            shell_quote(Path::new("/tmp/deep'seek")),
            "'/tmp/deep'\\''seek'"
        );
        assert_eq!(
            entrypoint_smoke_shell_command(Path::new("/tmp/deepseek")),
            "stty rows 36 cols 120; exec '/tmp/deepseek'"
        );
    }

    fn with_workspace_trust_file<F: FnOnce()>(path: &Path, f: F) {
        let _guard = env_lock();
        let previous = std::env::var("DSCODE_WORKSPACE_TRUST_FILE").ok();
        std::env::set_var("DSCODE_WORKSPACE_TRUST_FILE", path);
        f();
        match previous {
            Some(value) => std::env::set_var("DSCODE_WORKSPACE_TRUST_FILE", value),
            None => std::env::remove_var("DSCODE_WORKSPACE_TRUST_FILE"),
        }
    }

    fn temp_config(root: &Path) -> AppConfig {
        let mut config = AppConfig::default();
        config.workspace.config_dir = root.join(".dscode").display().to_string();
        config
    }

    fn make_executable(path: &Path) {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = fs::metadata(path).unwrap().permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(path, permissions).unwrap();
        }
        let _ = path;
    }

    #[test]
    fn configure_tui_slash_completions_includes_user_commands_and_skills() {
        let root = temp_root("slash-completions-config");
        let user_commands = root.join("user-commands");
        let user_skills = root.join("user-skills");
        fs::create_dir_all(user_commands.join("global")).unwrap();
        fs::create_dir_all(&user_skills).unwrap();
        fs::write(user_commands.join("global/fix.md"), "Fix: $ARGUMENTS").unwrap();
        fs::write(
            user_skills.join("pr-review.toml"),
            r#"
name = "pr-review"
description = "Review pull requests."
"#,
        )
        .unwrap();

        let mut config = temp_config(&root);
        config.workspace.user_commands_dir = user_commands.display().to_string();
        config.workspace.user_skills_dir = user_skills.display().to_string();
        let mut app = TuiApp::new(Vec::new());

        configure_tui_slash_completions(&mut app, &config);

        let completions = app.extra_slash_completions_for_test();
        assert!(completions
            .iter()
            .any(|value| value == "/model deepseek-v4-pro"));
        assert!(completions
            .iter()
            .any(|value| value == "/config model deepseek-v4-flash"));
        assert!(completions.iter().any(|value| value == "/global/fix"));
        assert!(completions.iter().any(|value| value == "/skill pr-review"));
        assert!(completions.iter().any(|value| value == "/pr-review"));
        let command_completions = app.extra_command_completions_for_test();
        assert!(command_completions
            .iter()
            .any(|value| value == "model deepseek-v4-pro"));
        assert!(command_completions
            .iter()
            .any(|value| value == "config model deepseek-v4-flash"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn configure_tui_slash_completions_uses_provider_model_ids() {
        let root = temp_root("slash-completions-provider-models");
        let mut config = temp_config(&root);
        config.model.base_url = "https://integrate.api.nvidia.com/v1".to_string();
        let mut app = TuiApp::new(Vec::new());

        configure_tui_slash_completions(&mut app, &config);

        let completions = app.extra_slash_completions_for_test();
        assert!(completions
            .iter()
            .any(|value| value == "/model deepseek-ai/deepseek-v4-pro"));
        assert!(completions
            .iter()
            .any(|value| value == "/model deepseek-ai/deepseek-v4-flash"));
        assert!(!completions
            .iter()
            .any(|value| value == "/model deepseek/deepseek-v4-pro"));
        let command_completions = app.extra_command_completions_for_test();
        assert!(command_completions
            .iter()
            .any(|value| value == "model deepseek-ai/deepseek-v4-pro"));
        assert!(command_completions
            .iter()
            .any(|value| value == "config model deepseek-ai/deepseek-v4-flash"));
        assert!(!command_completions
            .iter()
            .any(|value| value == "model deepseek/deepseek-v4-pro"));
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

    fn runtime_http_client(
        store: &RuntimeStore,
        request_limit: usize,
    ) -> (RuntimeHttpClient, JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let worker_store = store.clone();
        let handle = thread::spawn(move || {
            crate::cli::commands::serve::serve_http_listener_with_limit(
                listener,
                Some(request_limit),
                &worker_store,
            )
            .unwrap();
        });
        let client = RuntimeHttpClient::from_url(&format!("http://{addr}")).unwrap();
        (client, handle)
    }

    fn serve_once_text(body: String) -> String {
        serve_once_bytes(body.into_bytes(), "application/json")
    }

    fn serve_once_bytes(body: Vec<u8>, content_type: &str) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let content_type = content_type.to_string();
        thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0u8; 1024];
            let _ = stream.read(&mut request);
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            stream.write_all(response.as_bytes()).unwrap();
            stream.write_all(&body).unwrap();
        });
        format!("http://{addr}/index.json")
    }

    fn restore_env_var(key: &str, value: Option<String>) {
        if let Some(value) = value {
            std::env::set_var(key, value);
        } else {
            std::env::remove_var(key);
        }
    }

    fn test_skill_toml(name: &str, description: &str) -> String {
        format!(
            r#"name = "{name}"
description = "{description}"
allowed_tools = ["read_file"]
system_append = "Use this skill carefully."

[policy]
require_write_confirmation = true
require_shell_confirmation = false
shell_allowlist = []
"#
        )
    }

    fn test_skill_md(name: &str, description: &str, body: &str) -> String {
        format!(
            r#"---
name: {name}
description: {description}
---
{body}
"#
        )
    }

    fn test_skill_tarball(path: &str, skill_md: &str) -> Vec<u8> {
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        {
            let mut builder = tar::Builder::new(&mut encoder);
            let mut header = tar::Header::new_gnu();
            header.set_size(skill_md.as_bytes().len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            let mut body = skill_md.as_bytes();
            builder.append_data(&mut header, path, &mut body).unwrap();
            builder.finish().unwrap();
        }
        encoder.finish().unwrap()
    }

    fn test_skill_zip(path: &str, skill_md: &str) -> Vec<u8> {
        let cursor = std::io::Cursor::new(Vec::new());
        let mut writer = zip::ZipWriter::new(cursor);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored)
            .unix_permissions(0o644);
        writer.start_file(path, options).unwrap();
        writer.write_all(skill_md.as_bytes()).unwrap();
        writer.finish().unwrap().into_inner()
    }

    #[test]
    fn runtime_http_watcher_formats_rlm_live_event_status() {
        let event = RuntimeEvent {
            id: "event_rlm".to_string(),
            thread_id: "thread_1".to_string(),
            turn_id: None,
            seq: 3,
            kind: "rlm_live_event".to_string(),
            created_at: "epoch+1".to_string(),
            payload: JsonValue::Object(BTreeMap::from([
                (
                    "session_id".to_string(),
                    JsonValue::String("live.1".to_string()),
                ),
                (
                    "event".to_string(),
                    JsonValue::Object(BTreeMap::from([
                        (
                            "kind".to_string(),
                            JsonValue::String("turn_queued".to_string()),
                        ),
                        (
                            "task_id".to_string(),
                            JsonValue::String("task_1".to_string()),
                        ),
                    ])),
                ),
            ])),
        };

        assert_eq!(
            rlm_live_status_from_runtime_event(&event).as_deref(),
            Some("rlm live.1: turn_queued task=task_1")
        );
    }

    #[test]
    fn app_from_store_loads_session_threads_and_items() {
        let store = temp_store("items");
        let session = store
            .create_session("Daily work".to_string(), ".".to_string())
            .unwrap();
        let thread = store
            .create_thread_for_session(
                &session.id,
                "Runtime timeline".to_string(),
                ".".to_string(),
                "deepseek-coder".to_string(),
                "agent".to_string(),
            )
            .unwrap();
        let turn = store
            .append_turn(&thread.id, "assistant".to_string(), "done".to_string())
            .unwrap();
        store
            .append_item(
                &thread.id,
                Some(&turn.id),
                "message".to_string(),
                Some("assistant".to_string()),
                "done from runtime".to_string(),
                "completed".to_string(),
            )
            .unwrap();

        let app = app_from_store(&store).unwrap();
        let output = render_once(&app, 120, 36).unwrap();

        assert!(output.contains("Daily work"));
        assert!(output.contains("assistant [completed]: done from runtime"));
    }

    #[test]
    fn runtime_live_watcher_emits_snapshot_for_external_runtime_write() {
        let store = temp_store("watcher");
        let session = store
            .create_session("Daily work".to_string(), ".".to_string())
            .unwrap();
        let thread = store
            .create_thread_for_session(
                &session.id,
                "Runtime watcher".to_string(),
                ".".to_string(),
                "deepseek-coder".to_string(),
                "agent".to_string(),
            )
            .unwrap();
        let watched_thread_id = thread.id.clone();
        let (tx, rx) = mpsc::channel();
        let watcher = start_runtime_live_watcher(store.clone(), tx, Duration::from_millis(10));

        let turn = store
            .append_turn(
                &thread.id,
                "user".to_string(),
                "external runtime write".to_string(),
            )
            .unwrap();
        store
            .append_item(
                &thread.id,
                Some(&turn.id),
                "message".to_string(),
                Some("user".to_string()),
                "external runtime write".to_string(),
                "completed".to_string(),
            )
            .unwrap();

        let mut matched = false;
        for _ in 0..10 {
            let event = rx.recv_timeout(Duration::from_millis(100)).unwrap();
            if let TuiLiveEvent::ReplaceRuntime { items, threads, .. } = event {
                matched = threads
                    .iter()
                    .any(|thread| thread.id == watched_thread_id && thread.event_seq >= 3)
                    && items
                        .iter()
                        .any(|item| item.content == "external runtime write");
                if matched {
                    break;
                }
            }
        }
        drop(watcher);
        assert!(matched);
    }

    #[test]
    fn runtime_http_snapshot_loads_remote_runtime() {
        let store = temp_store("http-snapshot");
        let session = store
            .create_session("Remote work".to_string(), ".".to_string())
            .unwrap();
        let thread = store
            .create_thread_for_session(
                &session.id,
                "Remote timeline".to_string(),
                ".".to_string(),
                "deepseek-coder".to_string(),
                "agent".to_string(),
            )
            .unwrap();
        let turn = store
            .append_turn(
                &thread.id,
                "assistant".to_string(),
                "remote done".to_string(),
            )
            .unwrap();
        store
            .append_item(
                &thread.id,
                Some(&turn.id),
                "message".to_string(),
                Some("assistant".to_string()),
                "remote done".to_string(),
                "completed".to_string(),
            )
            .unwrap();
        store
            .create_task(
                Some(&session.id),
                Some(&thread.id),
                None,
                "agent".to_string(),
                "running".to_string(),
                "agent run: remote snapshot".to_string(),
            )
            .unwrap();
        store
            .create_automation(
                Some(&session.id),
                Some(&thread.id),
                "Remote automation".to_string(),
                "active".to_string(),
                "manual".to_string(),
                "run remote task".to_string(),
                None,
                None,
            )
            .unwrap();
        store
            .append_usage_with_cache(
                &thread.id,
                Some(&turn.id),
                "deepseek-v4-flash".to_string(),
                "remote".to_string(),
                12,
                3,
                7,
                5,
            )
            .unwrap();
        store
            .append_permission_request(
                &thread.id,
                Some(&turn.id),
                "run_shell".to_string(),
                "shell".to_string(),
                "cargo test".to_string(),
                BTreeMap::new(),
            )
            .unwrap();

        let (client, handle) = runtime_http_client(&store, 7);
        let snapshot = runtime_http_snapshot(&client).unwrap();
        handle.join().unwrap();

        assert_eq!(snapshot.sessions[0].title, "Remote work");
        assert_eq!(snapshot.threads[0].title, "Remote timeline");
        assert!(snapshot
            .items
            .iter()
            .any(|item| item.content == "remote done"));
        assert_eq!(snapshot.tasks.len(), 1);
        assert_eq!(snapshot.automations.len(), 1);
        assert_eq!(snapshot.usage_summaries[0].total_tokens, 15);
        assert_eq!(snapshot.approvals.len(), 1);
    }

    #[test]
    fn runtime_http_live_watcher_detects_new_remote_threads_from_global_sse() {
        let store = temp_store("http-global-sse");
        let session = store
            .create_session("Remote live".to_string(), ".".to_string())
            .unwrap();
        store
            .create_thread_for_session(
                &session.id,
                "Known remote thread".to_string(),
                ".".to_string(),
                "deepseek-coder".to_string(),
                "agent".to_string(),
            )
            .unwrap();
        let initial_snapshot = runtime_snapshot(&store).unwrap();
        let (client, handle) = runtime_http_client(&store, 13);
        let (tx, rx) = mpsc::channel();
        let watcher = start_runtime_http_live_watcher(
            client,
            runtime_http_subscriptions(&initial_snapshot),
            tx,
            1_000,
        );
        thread::sleep(Duration::from_millis(50));

        let created = store
            .create_thread_for_session(
                &session.id,
                "Created after TUI start".to_string(),
                ".".to_string(),
                "deepseek-coder".to_string(),
                "agent".to_string(),
            )
            .unwrap();

        let mut matched = false;
        for _ in 0..10 {
            let event = rx.recv_timeout(Duration::from_millis(200)).unwrap();
            if let TuiLiveEvent::ReplaceRuntime { threads, .. } = event {
                matched = threads.iter().any(|thread| {
                    thread.id == created.id && thread.title == "Created after TUI start"
                });
                if matched {
                    break;
                }
            }
        }
        drop(watcher);
        handle.join().unwrap();

        assert!(matched);
    }

    #[test]
    fn handle_tui_http_action_submits_remote_user_turn() {
        let store = temp_store("http-action");
        let session = store
            .create_session("Remote action".to_string(), ".".to_string())
            .unwrap();
        let thread = store
            .create_thread_for_session(
                &session.id,
                "Remote action thread".to_string(),
                ".".to_string(),
                "deepseek-coder".to_string(),
                "agent".to_string(),
            )
            .unwrap();
        let (client, handle) = runtime_http_client(&store, 1);
        let mut app = TuiApp::with_runtime(
            vec![TuiSession::from(session)],
            vec![TuiThread::from(thread.clone())],
            Vec::new(),
        );

        handle_tui_http_action(
            &client,
            &mut app,
            TuiAction::SubmitUserMessage {
                thread_id: thread.id.clone(),
                content: "hello remote runtime".to_string(),
            },
        )
        .unwrap();
        handle.join().unwrap();

        let turns = store.list_turns(&thread.id).unwrap();
        assert!(turns
            .iter()
            .any(|turn| turn.role == "user" && turn.content == "hello remote runtime"));
    }

    #[test]
    fn handle_tui_http_action_creates_remote_subagent_task() {
        let store = temp_store("http-subagent-action");
        let session = store
            .create_session("Remote action".to_string(), ".".to_string())
            .unwrap();
        let thread = store
            .create_thread_for_session(
                &session.id,
                "Remote subagent thread".to_string(),
                ".".to_string(),
                "deepseek-coder".to_string(),
                "agent".to_string(),
            )
            .unwrap();
        let (client, handle) = runtime_http_client(&store, 1);
        let mut app = TuiApp::with_runtime(
            vec![TuiSession::from(session.clone())],
            vec![TuiThread::from(thread.clone())],
            Vec::new(),
        );

        handle_tui_http_action(
            &client,
            &mut app,
            TuiAction::CreateSubagentTask {
                thread_id: thread.id.clone(),
                task: "inspect remote parity".to_string(),
                max_depth: 3,
            },
        )
        .unwrap();
        handle.join().unwrap();

        let tasks = store
            .list_tasks(Some(&session.id), Some(&thread.id), 10)
            .unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].kind, "subagent");
        assert_eq!(tasks[0].status, "pending");
        assert_eq!(tasks[0].summary, "max_depth=3: inspect remote parity");
        assert!(render_once(&app, 160, 48)
            .unwrap()
            .contains("created remote subagent task"));
    }

    #[test]
    fn handle_tui_http_action_rejects_shell_commands_as_local_only() {
        let client = RuntimeHttpClient::from_url("http://127.0.0.1:9").unwrap();
        let mut app = TuiApp::new(Vec::new());

        handle_tui_http_action(
            &client,
            &mut app,
            TuiAction::RunShell {
                command: "echo remote".to_string(),
            },
        )
        .unwrap();

        assert!(render_once(&app, 120, 36)
            .unwrap()
            .contains("shell commands require local file-backed TUI"));

        handle_tui_http_action(&client, &mut app, TuiAction::ShellSupervisorStatus).unwrap();

        assert!(render_once(&app, 120, 36)
            .unwrap()
            .contains("shell commands require local file-backed TUI"));
    }

    #[test]
    fn handle_tui_http_action_rejects_custom_slash_commands_as_local_only() {
        let client = RuntimeHttpClient::from_url("http://127.0.0.1:9").unwrap();
        let mut app = TuiApp::new(Vec::new());

        handle_tui_http_action(
            &client,
            &mut app,
            TuiAction::RunCustomSlashCommand {
                thread_id: "thread-one".to_string(),
                command: "/review".to_string(),
                args: vec!["src/lib.rs".to_string()],
            },
        )
        .unwrap();

        assert!(render_once(&app, 120, 36)
            .unwrap()
            .contains("custom slash commands require local file-backed TUI"));
    }

    #[test]
    fn handle_tui_http_action_rejects_anchor_commands_as_local_only() {
        let client = RuntimeHttpClient::from_url("http://127.0.0.1:9").unwrap();
        let mut app = TuiApp::new(Vec::new());

        handle_tui_http_action(
            &client,
            &mut app,
            TuiAction::Anchor {
                workspace: ".".to_string(),
                command: TuiAnchorCommand::List,
            },
        )
        .unwrap();

        assert!(render_once(&app, 120, 36)
            .unwrap()
            .contains("anchor commands require local file-backed TUI"));
    }

    #[test]
    fn handle_tui_http_action_rejects_share_commands_as_local_only() {
        let client = RuntimeHttpClient::from_url("http://127.0.0.1:9").unwrap();
        let mut app = TuiApp::new(Vec::new());

        handle_tui_http_action(
            &client,
            &mut app,
            TuiAction::ShareSession {
                thread_id: "thread-one".to_string(),
            },
        )
        .unwrap();

        assert!(render_once(&app, 120, 36)
            .unwrap()
            .contains("share commands require local file-backed TUI"));
    }

    #[test]
    fn handle_tui_http_action_rejects_export_commands_as_local_only() {
        let client = RuntimeHttpClient::from_url("http://127.0.0.1:9").unwrap();
        let mut app = TuiApp::new(Vec::new());

        handle_tui_http_action(
            &client,
            &mut app,
            TuiAction::ExportThread {
                thread_id: "thread-one".to_string(),
                path: Some("chat.md".to_string()),
            },
        )
        .unwrap();

        assert!(render_once(&app, 120, 36)
            .unwrap()
            .contains("export commands require local file-backed TUI"));
    }

    #[test]
    fn handle_tui_http_action_rejects_save_load_commands_as_local_only() {
        let client = RuntimeHttpClient::from_url("http://127.0.0.1:9").unwrap();
        let mut app = TuiApp::new(Vec::new());

        handle_tui_http_action(
            &client,
            &mut app,
            TuiAction::SaveSession {
                session_id: "session-one".to_string(),
                thread_id: "thread-one".to_string(),
                path: Some("session.json".to_string()),
            },
        )
        .unwrap();
        assert!(render_once(&app, 120, 36)
            .unwrap()
            .contains("save commands require local file-backed TUI"));

        handle_tui_http_action(&client, &mut app, TuiAction::PruneSessions { days: 30 }).unwrap();
        assert!(render_once(&app, 120, 36)
            .unwrap()
            .contains("session prune requires local file-backed TUI"));

        handle_tui_http_action(
            &client,
            &mut app,
            TuiAction::LoadSession {
                workspace: ".".to_string(),
                path: "session.json".to_string(),
            },
        )
        .unwrap();
        assert!(render_once(&app, 120, 36)
            .unwrap()
            .contains("load commands require local file-backed TUI"));
    }

    #[test]
    fn handle_tui_http_action_rejects_clear_conversation_as_local_only() {
        let client = RuntimeHttpClient::from_url("http://127.0.0.1:9").unwrap();
        let mut app = TuiApp::new(Vec::new());

        handle_tui_http_action(
            &client,
            &mut app,
            TuiAction::ClearConversation {
                session_id: "session-one".to_string(),
                previous_thread_id: Some("thread-one".to_string()),
            },
        )
        .unwrap();

        assert!(render_once(&app, 120, 36)
            .unwrap()
            .contains("clear conversation requires local file-backed TUI"));
    }

    #[test]
    fn handle_tui_http_action_rejects_diff_commands_as_local_only() {
        let client = RuntimeHttpClient::from_url("http://127.0.0.1:9").unwrap();
        let mut app = TuiApp::new(Vec::new());

        handle_tui_http_action(
            &client,
            &mut app,
            TuiAction::ShowDiff {
                workspace: ".".to_string(),
            },
        )
        .unwrap();

        assert!(render_once(&app, 120, 36)
            .unwrap()
            .contains("diff commands require local file-backed TUI"));
    }

    #[test]
    fn handle_tui_http_action_rejects_session_rename_as_local_only() {
        let client = RuntimeHttpClient::from_url("http://127.0.0.1:9").unwrap();
        let mut app = TuiApp::new(Vec::new());

        handle_tui_http_action(
            &client,
            &mut app,
            TuiAction::RenameSession {
                session_id: "session-one".to_string(),
                title: "Focused Work".to_string(),
            },
        )
        .unwrap();

        assert!(render_once(&app, 120, 36)
            .unwrap()
            .contains("session rename requires local file-backed TUI"));
    }

    #[test]
    fn handle_tui_http_action_rejects_project_init_as_local_only() {
        let client = RuntimeHttpClient::from_url("http://127.0.0.1:9").unwrap();
        let mut app = TuiApp::new(Vec::new());

        handle_tui_http_action(
            &client,
            &mut app,
            TuiAction::InitProjectInstructions {
                workspace: ".".to_string(),
            },
        )
        .unwrap();

        assert!(render_once(&app, 120, 36)
            .unwrap()
            .contains("project instructions init requires local file-backed TUI"));
    }

    #[test]
    fn handle_tui_http_action_rejects_network_commands_as_local_only() {
        let client = RuntimeHttpClient::from_url("http://127.0.0.1:9").unwrap();
        let mut app = TuiApp::new(Vec::new());

        handle_tui_http_action(
            &client,
            &mut app,
            TuiAction::Network {
                workspace: ".".to_string(),
                command: TuiNetworkCommand::List,
            },
        )
        .unwrap();

        assert!(render_once(&app, 120, 36)
            .unwrap()
            .contains("network commands require local file-backed TUI"));
    }

    #[test]
    fn handle_tui_http_action_rejects_lsp_commands_as_local_only() {
        let client = RuntimeHttpClient::from_url("http://127.0.0.1:9").unwrap();
        let mut app = TuiApp::new(Vec::new());

        handle_tui_http_action(
            &client,
            &mut app,
            TuiAction::Lsp {
                workspace: ".".to_string(),
                command: TuiLspCommand::Status,
            },
        )
        .unwrap();

        assert!(render_once(&app, 120, 36)
            .unwrap()
            .contains("lsp commands require local file-backed TUI"));
    }

    #[test]
    fn handle_tui_http_action_rejects_skills_commands_as_local_only() {
        let client = RuntimeHttpClient::from_url("http://127.0.0.1:9").unwrap();
        let mut app = TuiApp::new(Vec::new());

        handle_tui_http_action(
            &client,
            &mut app,
            TuiAction::Skills {
                command: TuiSkillsCommand::List { prefix: None },
            },
        )
        .unwrap();

        assert!(render_once(&app, 120, 36)
            .unwrap()
            .contains("skills commands require local file-backed TUI"));
    }

    #[test]
    fn handle_tui_http_action_cancels_remote_task() {
        let store = temp_store("http-task-cancel-action");
        let session = store
            .create_session("Remote action".to_string(), ".".to_string())
            .unwrap();
        let thread = store
            .create_thread_for_session(
                &session.id,
                "Remote action thread".to_string(),
                ".".to_string(),
                "deepseek-coder".to_string(),
                "agent".to_string(),
            )
            .unwrap();
        let task = store
            .create_task(
                Some(&session.id),
                Some(&thread.id),
                None,
                "agent".to_string(),
                "running".to_string(),
                "remote task".to_string(),
            )
            .unwrap();
        let (client, handle) = runtime_http_client(&store, 1);
        let mut app = TuiApp::with_runtime(
            vec![TuiSession::from(session)],
            vec![TuiThread::from(thread.clone())],
            Vec::new(),
        );

        handle_tui_http_action(
            &client,
            &mut app,
            TuiAction::CancelTask {
                task_id: task.id.clone(),
            },
        )
        .unwrap();
        handle.join().unwrap();

        assert_eq!(store.load_task(&task.id).unwrap().status, "cancelled");
        let events = store.read_events(&thread.id, 0).unwrap();
        assert!(events.iter().any(|event| event.kind == "cancel_requested"
            && json_as_object(&event.payload)
                .and_then(|payload| payload.get("task_id"))
                .and_then(json_as_string)
                .is_some_and(|task_id| task_id == task.id)));
        assert!(render_once(&app, 160, 48)
            .unwrap()
            .contains("cancelled remote task"));
    }

    #[test]
    fn app_from_store_loads_usage_summary_into_task_panel() {
        let store = temp_store("usage");
        let session = store
            .create_session("Cost watch".to_string(), ".".to_string())
            .unwrap();
        let thread = store
            .create_thread_for_session(
                &session.id,
                "Runtime usage".to_string(),
                ".".to_string(),
                "deepseek-v4-flash".to_string(),
                "agent".to_string(),
            )
            .unwrap();
        let turn = store
            .append_turn(&thread.id, "assistant".to_string(), "done".to_string())
            .unwrap();
        store
            .append_usage_with_cache(
                &thread.id,
                Some(&turn.id),
                "deepseek-v4-flash".to_string(),
                "tui".to_string(),
                12,
                3,
                7,
                5,
            )
            .unwrap();

        let app = app_from_store(&store).unwrap();
        let output = render_once(&app, 160, 48).unwrap();

        assert!(output.contains("Usage total: 15 tokens"));
        assert!(output.contains("Cache hit: 7 / 12"));
        assert!(output.contains("Cache chart: ["));
        assert!(output.contains("Est. cost: $0.000002"));
        assert!(output.contains("Cost split: in"));
        assert!(output.contains("Cost chart: ["));
    }

    #[test]
    fn app_from_store_loads_runtime_tasks_into_task_panel() {
        let store = temp_store("tasks");
        let session = store
            .create_session("Progress watch".to_string(), ".".to_string())
            .unwrap();
        let thread = store
            .create_thread_for_session(
                &session.id,
                "Runtime tasks".to_string(),
                ".".to_string(),
                "deepseek-coder".to_string(),
                "agent".to_string(),
            )
            .unwrap();
        store
            .create_task(
                Some(&session.id),
                Some(&thread.id),
                None,
                "agent".to_string(),
                "running".to_string(),
                "agent run: stream progress".to_string(),
            )
            .unwrap();

        let app = app_from_store(&store).unwrap();
        let output = render_once(&app, 160, 48).unwrap();

        assert!(output.contains("Runtime tasks: 1"));
        assert!(output.contains("Task states: running=1"));
        assert!(output.contains("[running]"));
        assert!(output.contains("updated"));
    }

    #[test]
    fn app_from_store_loads_automations_into_task_panel() {
        let store = temp_store("automations");
        let session = store
            .create_session("Automation watch".to_string(), ".".to_string())
            .unwrap();
        let thread = store
            .create_thread_for_session(
                &session.id,
                "Runtime automations".to_string(),
                ".".to_string(),
                "deepseek-coder".to_string(),
                "agent".to_string(),
            )
            .unwrap();
        store
            .create_automation(
                Some(&session.id),
                Some(&thread.id),
                "Nightly diagnostics".to_string(),
                "active".to_string(),
                "daily".to_string(),
                "run diagnostics".to_string(),
                None,
                Some("epoch+100".to_string()),
            )
            .unwrap();

        let app = app_from_store(&store).unwrap();
        let output = render_once(&app, 160, 48).unwrap();

        assert!(output.contains("Automations: 1"));
        assert!(output.contains("Automation Nightly"));
    }

    #[test]
    fn refresh_app_from_store_updates_existing_app() {
        let store = temp_store("refresh");
        let session = store
            .create_session("Daily work".to_string(), ".".to_string())
            .unwrap();
        let thread = store
            .create_thread_for_session(
                &session.id,
                "Runtime timeline".to_string(),
                ".".to_string(),
                "deepseek-coder".to_string(),
                "agent".to_string(),
            )
            .unwrap();
        let mut app = app_from_store(&store).unwrap();
        assert!(render_once(&app, 120, 36)
            .unwrap()
            .contains("No durable items recorded"));

        let turn = store
            .append_turn(&thread.id, "assistant".to_string(), "done".to_string())
            .unwrap();
        store
            .append_item(
                &thread.id,
                Some(&turn.id),
                "message".to_string(),
                Some("assistant".to_string()),
                "fresh runtime item".to_string(),
                "completed".to_string(),
            )
            .unwrap();

        refresh_app_from_store(&store, &mut app).unwrap();
        let output = render_once(&app, 120, 36).unwrap();

        assert!(output.contains("assistant [completed]: fresh runtime item"));
        assert!(output.contains("runtime refreshed: sessions=1 threads=1 items=1"));
    }

    #[test]
    fn app_from_store_loads_permission_request_events() {
        let store = temp_store("approval");
        let session = store
            .create_session("Daily work".to_string(), ".".to_string())
            .unwrap();
        let thread = store
            .create_thread_for_session(
                &session.id,
                "Runtime permissions".to_string(),
                ".".to_string(),
                "deepseek-coder".to_string(),
                "agent".to_string(),
            )
            .unwrap();
        let mut input = BTreeMap::new();
        input.insert("command".to_string(), "cargo test".to_string());
        store
            .append_permission_request(
                &thread.id,
                None,
                "run_shell".to_string(),
                "shell".to_string(),
                "cargo test".to_string(),
                input,
            )
            .unwrap();

        let app = app_from_store(&store).unwrap();
        let output = render_once(&app, 120, 36).unwrap();

        assert!(output.contains("Approval Modal"));
        assert!(output.contains("run_shell"));
        assert!(output.contains("cargo test"));
        assert!(output.contains("perm:shell:run_shell:"));
    }

    #[test]
    fn app_from_store_hides_answered_permission_request_events() {
        let store = temp_store("approval-answered");
        let session = store
            .create_session("Daily work".to_string(), ".".to_string())
            .unwrap();
        let thread = store
            .create_thread_for_session(
                &session.id,
                "Runtime permissions".to_string(),
                ".".to_string(),
                "deepseek-coder".to_string(),
                "agent".to_string(),
            )
            .unwrap();
        let request = store
            .append_permission_request(
                &thread.id,
                None,
                "run_shell".to_string(),
                "shell".to_string(),
                "cargo test".to_string(),
                BTreeMap::new(),
            )
            .unwrap();
        store
            .append_permission_response(&thread.id, None, request.id, "denied".to_string())
            .unwrap();

        let app = app_from_store(&store).unwrap();
        let output = render_once(&app, 120, 36).unwrap();

        assert!(!output.contains("Approval Modal"));
        assert!(!output.contains("cargo test"));
    }

    #[test]
    fn app_from_store_hides_answered_user_input_events() {
        let store = temp_store("user-input-answered");
        let session = store
            .create_session("Daily work".to_string(), ".".to_string())
            .unwrap();
        let thread = store
            .create_thread_for_session(
                &session.id,
                "Runtime input".to_string(),
                ".".to_string(),
                "deepseek-coder".to_string(),
                "agent".to_string(),
            )
            .unwrap();
        let request = store
            .append_user_input_request(
                &thread.id,
                None,
                JsonValue::Array(vec![json_object([
                    ("header", JsonValue::String("Mode".to_string())),
                    ("id", JsonValue::String("mode".to_string())),
                    ("question", JsonValue::String("Which mode?".to_string())),
                    (
                        "options",
                        JsonValue::Array(vec![
                            json_object([
                                ("label", JsonValue::String("Plan".to_string())),
                                ("description", JsonValue::String("Plan first.".to_string())),
                            ]),
                            json_object([
                                ("label", JsonValue::String("Apply".to_string())),
                                ("description", JsonValue::String("Apply now.".to_string())),
                            ]),
                        ]),
                    ),
                ])]),
            )
            .unwrap();
        store
            .append_user_input_response(
                &thread.id,
                None,
                request.id,
                BTreeMap::from([("mode".to_string(), "Plan".to_string())]),
            )
            .unwrap();

        let app = app_from_store(&store).unwrap();
        let output = render_once(&app, 120, 36).unwrap();

        assert!(!output.contains("User Input Modal"));
        assert!(!output.contains("Which mode?"));
    }

    #[test]
    fn handle_tui_action_records_composer_message() {
        let store = temp_store("composer");
        let session = store
            .create_session("Daily work".to_string(), ".".to_string())
            .unwrap();
        let thread = store
            .create_thread_for_session(
                &session.id,
                "Runtime composer".to_string(),
                ".".to_string(),
                "deepseek-coder".to_string(),
                "agent".to_string(),
            )
            .unwrap();
        let mut app = app_from_store(&store).unwrap();

        handle_tui_action(
            &store,
            None,
            &mut app,
            TuiAction::SubmitUserMessage {
                thread_id: thread.id.clone(),
                content: "hello from composer".to_string(),
            },
        )
        .unwrap();
        refresh_app_from_store(&store, &mut app).unwrap();
        let output = render_once(&app, 120, 36).unwrap();

        assert!(output.contains("user [completed]: hello from composer"));
        let turns = store.list_turns(&thread.id).unwrap();
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].role, "user");
        assert_eq!(store.list_items(&thread.id, None).unwrap().len(), 1);
    }

    #[test]
    fn handle_tui_action_reports_missing_custom_slash_command() {
        let root = temp_root("custom-slash-missing");
        let store = RuntimeStore::new(root.join("runtime"));
        let config = temp_config(&root);
        let session = store
            .create_session("Daily work".to_string(), ".".to_string())
            .unwrap();
        let thread = store
            .create_thread_for_session(
                &session.id,
                "Runtime custom slash".to_string(),
                ".".to_string(),
                "deepseek-coder".to_string(),
                "agent".to_string(),
            )
            .unwrap();
        let mut app = app_from_store(&store).unwrap();

        handle_tui_action(
            &store,
            Some(&config),
            &mut app,
            TuiAction::RunCustomSlashCommand {
                thread_id: thread.id.clone(),
                command: "/missing".to_string(),
                args: Vec::new(),
            },
        )
        .unwrap();

        assert!(render_once(&app, 120, 36)
            .unwrap()
            .contains("custom slash command not found: /missing"));
        assert!(store.list_turns(&thread.id).unwrap().is_empty());
    }

    #[test]
    fn handle_tui_action_falls_back_to_direct_skill_slash_command() {
        let root = temp_root("custom-slash-skill-fallback");
        let store = RuntimeStore::new(root.join("runtime"));
        let user_skills = root.join("user-skills");
        fs::create_dir_all(&user_skills).unwrap();
        fs::write(
            user_skills.join("pr-review.toml"),
            r#"
name = "pr-review"
description = "Review pull requests."
allowed_tools = ["read_file"]
"#,
        )
        .unwrap();
        let mut config = temp_config(&root);
        config.workspace.user_skills_dir = user_skills.display().to_string();
        let session = store
            .create_session("Daily work".to_string(), ".".to_string())
            .unwrap();
        let thread = store
            .create_thread_for_session(
                &session.id,
                "Runtime skill slash".to_string(),
                ".".to_string(),
                "deepseek-coder".to_string(),
                "agent".to_string(),
            )
            .unwrap();
        let mut app = app_from_store(&store).unwrap();

        handle_tui_action(
            &store,
            Some(&config),
            &mut app,
            TuiAction::RunCustomSlashCommand {
                thread_id: thread.id.clone(),
                command: "/pr-review".to_string(),
                args: Vec::new(),
            },
        )
        .unwrap();

        assert!(render_once(&app, 120, 36)
            .unwrap()
            .contains("skill shown: pr-review"));
        let (kind, detail) = app.mcp_detail_for_test().expect("skill detail");
        assert_eq!(kind, TuiMcpDetailKind::Skills);
        assert!(detail.contains("# Skill: pr-review"));
        assert!(detail.contains("Review pull requests."));
        assert!(store.list_turns(&thread.id).unwrap().is_empty());

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn handle_tui_action_renames_session_title() {
        let store = temp_store("rename-session");
        let session = store
            .create_session("Daily work".to_string(), ".".to_string())
            .unwrap();
        store
            .create_thread_for_session(
                &session.id,
                "Runtime thread".to_string(),
                ".".to_string(),
                "deepseek-coder".to_string(),
                "agent".to_string(),
            )
            .unwrap();
        let mut app = app_from_store(&store).unwrap();

        handle_tui_action(
            &store,
            None,
            &mut app,
            TuiAction::RenameSession {
                session_id: session.id.clone(),
                title: "Focused Work".to_string(),
            },
        )
        .unwrap();

        assert_eq!(
            store.load_session(&session.id).unwrap().title,
            "Focused Work"
        );
        assert!(render_once(&app, 120, 36)
            .unwrap()
            .contains("title: Focused Work"));
    }

    #[test]
    fn handle_tui_action_initializes_project_instructions() {
        let root = temp_root("project-init");
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::write(
            root.join("Cargo.toml"),
            "[package]\nname = \"project_init_sample\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        let store = RuntimeStore::new(root.join("runtime"));
        let mut app = TuiApp::new(Vec::new());

        handle_tui_action(
            &store,
            None,
            &mut app,
            TuiAction::InitProjectInstructions {
                workspace: root.display().to_string(),
            },
        )
        .unwrap();

        let agents = std::fs::read_to_string(root.join("AGENTS.md")).unwrap();
        assert!(agents.contains("project_init_sample"));
        assert!(std::fs::read_to_string(root.join(".gitignore"))
            .unwrap()
            .contains(".dscode/"));
        assert!(render_once(&app, 120, 36)
            .unwrap()
            .contains("created project instructions"));

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn handle_tui_action_manages_network_policy() {
        let root = temp_root("network-policy");
        let store = RuntimeStore::new(root.join("runtime"));
        let mut app = TuiApp::new(Vec::new());

        handle_tui_action(
            &store,
            None,
            &mut app,
            TuiAction::Network {
                workspace: root.display().to_string(),
                command: TuiNetworkCommand::Deny {
                    host: "*.Example.com".to_string(),
                },
            },
        )
        .unwrap();

        let config = std::fs::read_to_string(root.join(".dscode/config.toml")).unwrap();
        assert!(config.contains(r#"network.deny = [".example.com"]"#));
        assert!(render_once(&app, 120, 36)
            .unwrap()
            .contains("network host denied: .example.com"));

        handle_tui_action(
            &store,
            None,
            &mut app,
            TuiAction::Network {
                workspace: root.display().to_string(),
                command: TuiNetworkCommand::Allow {
                    host: "*.example.com".to_string(),
                },
            },
        )
        .unwrap();

        let config = std::fs::read_to_string(root.join(".dscode/config.toml")).unwrap();
        assert!(config.contains(r#"network.allow = [".example.com"]"#));
        assert!(config.contains("network.deny = []"));

        handle_tui_action(
            &store,
            None,
            &mut app,
            TuiAction::Network {
                workspace: root.display().to_string(),
                command: TuiNetworkCommand::Default {
                    value: "prompt".to_string(),
                },
            },
        )
        .unwrap();

        let output = render_once(&app, 120, 36).unwrap();
        assert!(output.contains("network default set: prompt"));
        assert!(output.contains("network.default = prompt"));

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn handle_tui_action_manages_lsp_diagnostics_config() {
        let root = temp_root("lsp-diagnostics");
        let store = RuntimeStore::new(root.join("runtime"));
        let mut app = TuiApp::new(Vec::new());

        handle_tui_action(
            &store,
            None,
            &mut app,
            TuiAction::Lsp {
                workspace: root.display().to_string(),
                command: TuiLspCommand::Set { enabled: true },
            },
        )
        .unwrap();

        let config = std::fs::read_to_string(root.join(".dscode/config.toml")).unwrap();
        assert!(config.contains("diagnostics.post_edit = true"));
        let output = render_once(&app, 120, 36).unwrap();
        assert!(output.contains("lsp diagnostics enabled"));
        assert!(output.contains("diagnostics.post_edit = true"));

        handle_tui_action(
            &store,
            None,
            &mut app,
            TuiAction::Lsp {
                workspace: root.display().to_string(),
                command: TuiLspCommand::Status,
            },
        )
        .unwrap();

        assert!(render_once(&app, 120, 36)
            .unwrap()
            .contains("lsp diagnostics status shown"));

        handle_tui_action(
            &store,
            None,
            &mut app,
            TuiAction::Lsp {
                workspace: root.display().to_string(),
                command: TuiLspCommand::Set { enabled: false },
            },
        )
        .unwrap();

        let config = std::fs::read_to_string(root.join(".dscode/config.toml")).unwrap();
        assert!(config.contains("diagnostics.post_edit = false"));
        let output = render_once(&app, 120, 36).unwrap();
        assert!(output.contains("lsp diagnostics disabled"));
        assert!(output.contains("diagnostics.post_edit = false"));

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn handle_tui_action_renders_system_prompt_preview() {
        let root = temp_root("system-prompt");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("AGENTS.md"), "Prefer cargo test before commit.").unwrap();
        let memory_path = root.join("memory.md");
        fs::write(&memory_path, "prefer short answers").unwrap();
        let store = RuntimeStore::new(root.join("runtime"));
        let mut config = temp_config(&root);
        config.workspace.user_instructions_file =
            root.join("missing-user-agents.md").display().to_string();
        config.workspace.user_skills_dir = root.join("missing-skills").display().to_string();
        config.memory.enabled = true;
        config.memory.memory_path = memory_path.display().to_string();
        let mut app = TuiApp::new(Vec::new());

        handle_tui_action(
            &store,
            Some(&config),
            &mut app,
            TuiAction::ShowSystemPrompt {
                workspace: root.display().to_string(),
                mode: TuiMode::Agent,
                task: Some("inspect current repo".to_string()),
            },
        )
        .unwrap();

        let (kind, detail) = app.mcp_detail_for_test().expect("system detail");
        assert_eq!(kind, TuiMcpDetailKind::System);
        assert!(detail.contains("DeepSeekCode System Prompt"));
        assert!(detail.contains("Task: inspect current repo"));
        assert!(detail.contains("Prefer cargo test before commit."));
        assert!(detail.contains("prefer short answers"));
        assert!(render_once(&app, 120, 36)
            .unwrap()
            .contains("system prompt shown"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn handle_tui_action_manages_model_config() {
        let root = temp_root("model-config");
        let store = RuntimeStore::new(root.join("runtime"));
        let mut app = TuiApp::new(Vec::new());

        handle_tui_action(
            &store,
            None,
            &mut app,
            TuiAction::Model {
                workspace: root.display().to_string(),
                command: TuiModelCommand::Set {
                    model: "pro".to_string(),
                },
            },
        )
        .unwrap();

        let config = std::fs::read_to_string(root.join(".dscode/config.toml")).unwrap();
        assert!(config.contains(r#"model.model = "deepseek-v4-pro""#));
        let output = render_once(&app, 120, 36).unwrap();
        assert!(output.contains("model set:"));
        assert!(output.contains("model.model = deepseek-v4-pro"));

        handle_tui_action(
            &store,
            None,
            &mut app,
            TuiAction::Model {
                workspace: root.display().to_string(),
                command: TuiModelCommand::List,
            },
        )
        .unwrap();

        let output = render_once(&app, 120, 36).unwrap();
        assert!(output.contains("model catalog shown"));
        assert!(output.contains("deepseek-v4-pro (current)"));
        assert!(output.contains("auto"));

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn handle_tui_action_manages_provider_config() {
        let root = temp_root("provider-config");
        let store = RuntimeStore::new(root.join("runtime"));
        let mut app = TuiApp::new(Vec::new());

        handle_tui_action(
            &store,
            None,
            &mut app,
            TuiAction::Provider {
                workspace: root.display().to_string(),
                command: TuiProviderCommand::Set {
                    provider: "nvidia-nim".to_string(),
                    model: Some("flash".to_string()),
                },
            },
        )
        .unwrap();

        let config = std::fs::read_to_string(root.join(".dscode/config.toml")).unwrap();
        assert!(config.contains(r#"model.base_url = "https://integrate.api.nvidia.com/v1""#));
        assert!(config.contains(r#"model.api_key_env = "NVIDIA_API_KEY""#));
        assert!(config.contains(r#"model.model = "deepseek-ai/deepseek-v4-flash""#));
        let output = render_once(&app, 120, 36).unwrap();
        assert!(output.contains("provider set:"));
        assert!(output.contains("provider = nvidia-nim"));
        assert!(output.contains("deepseek-ai/deepseek-v4-flash"));

        handle_tui_action(
            &store,
            None,
            &mut app,
            TuiAction::Provider {
                workspace: root.display().to_string(),
                command: TuiProviderCommand::List,
            },
        )
        .unwrap();

        let output = render_once(&app, 120, 36).unwrap();
        assert!(output.contains("provider catalog shown"));
        assert!(output.contains("nvidia-nim"));
        assert!(output.contains("(current)"));
        assert!(output.contains("openrouter"));

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn handle_tui_action_manages_profile_config() {
        let root = temp_root("profile-config");
        let config_dir = root.join(".dscode");
        fs::create_dir_all(&config_dir).unwrap();
        fs::write(
            config_dir.join("config.toml"),
            r#"model.model = "base-model"

[profiles.work]
model.model = "deepseek-v4-pro"
model.reasoning_effort = "max"

[profiles.flash]
model.model = "deepseek-v4-flash"
"#,
        )
        .unwrap();
        let store = RuntimeStore::new(root.join("runtime"));
        let mut app = TuiApp::new(Vec::new());

        handle_tui_action(
            &store,
            None,
            &mut app,
            TuiAction::Profile {
                workspace: root.display().to_string(),
                command: TuiProfileCommand::Switch {
                    profile: "work".to_string(),
                },
            },
        )
        .unwrap();

        let config = fs::read_to_string(config_dir.join("config.toml")).unwrap();
        assert!(config.contains(r#"workspace.active_profile = "work""#));
        let (kind, detail) = app.mcp_detail_for_test().expect("profile detail");
        assert_eq!(kind, TuiMcpDetailKind::Profile);
        assert!(detail.contains("work (active)"));
        assert!(detail.contains(r#"model.model = "deepseek-v4-pro""#));
        assert!(render_once(&app, 120, 36)
            .unwrap()
            .contains("profile switched: work"));

        handle_tui_action(
            &store,
            None,
            &mut app,
            TuiAction::Profile {
                workspace: root.display().to_string(),
                command: TuiProfileCommand::Clear,
            },
        )
        .unwrap();

        let config = fs::read_to_string(config_dir.join("config.toml")).unwrap();
        assert!(config.contains(r#"workspace.active_profile = """#));
        assert!(render_once(&app, 120, 36)
            .unwrap()
            .contains("profile cleared: work"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn handle_tui_action_manages_workspace_trust() {
        let root = temp_root("trust-config");
        let workspace = root.join("workspace");
        let external = root.join("external");
        fs::create_dir_all(&workspace).unwrap();
        fs::create_dir_all(&external).unwrap();
        let trust_file = root.join("trust.json");
        let store = RuntimeStore::new(root.join("runtime"));
        let mut app = TuiApp::new(Vec::new());

        with_workspace_trust_file(&trust_file, || {
            handle_tui_action(
                &store,
                None,
                &mut app,
                TuiAction::Trust {
                    workspace: workspace.display().to_string(),
                    command: TuiTrustCommand::Add {
                        path: external.display().to_string(),
                    },
                },
            )
            .unwrap();

            let (kind, detail) = app.mcp_detail_for_test().expect("trust detail");
            assert_eq!(kind, TuiMcpDetailKind::Trust);
            assert!(detail.contains("Workspace trust mode: disabled"));
            assert!(detail.contains(&external.canonicalize().unwrap().display().to_string()));
            assert!(render_once(&app, 120, 36)
                .unwrap()
                .contains("trusted path added"));

            handle_tui_action(
                &store,
                None,
                &mut app,
                TuiAction::Trust {
                    workspace: workspace.display().to_string(),
                    command: TuiTrustCommand::SetMode { enabled: true },
                },
            )
            .unwrap();
            let (_, detail) = app.mcp_detail_for_test().expect("trust detail");
            assert!(detail.contains("Workspace trust mode: enabled"));
            assert!(render_once(&app, 120, 36)
                .unwrap()
                .contains("workspace trust mode enabled"));

            handle_tui_action(
                &store,
                None,
                &mut app,
                TuiAction::Trust {
                    workspace: workspace.display().to_string(),
                    command: TuiTrustCommand::Remove {
                        path: external.display().to_string(),
                    },
                },
            )
            .unwrap();
            let (_, detail) = app.mcp_detail_for_test().expect("trust detail");
            assert!(!detail.contains(&external.canonicalize().unwrap().display().to_string()));
            assert!(render_once(&app, 120, 36)
                .unwrap()
                .contains("trusted path removed"));
        });

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn handle_tui_action_logs_out_local_api_key_state() {
        let _guard = env_lock();
        let root = temp_root("logout-config");
        fs::create_dir_all(root.join(".dscode")).unwrap();
        fs::write(
            root.join(".dscode/config.toml"),
            r#"workspace.active_profile = "work"
model.api_key_env = "DSCODE_TEST_LOGOUT_BASE_MODEL_KEY"
vision.api_key_env = "DSCODE_TEST_LOGOUT_BASE_VISION_KEY"

[profiles.work]
model.api_key_env = "DSCODE_TEST_LOGOUT_MODEL_KEY"
vision.api_key_env = "DSCODE_TEST_LOGOUT_VISION_KEY"
"#,
        )
        .unwrap();
        fs::write(
            root.join(".env"),
            r#"DSCODE_TEST_LOGOUT_MODEL_KEY=model-secret
DSCODE_TEST_LOGOUT_BASE_MODEL_KEY=base-model-secret
KEEP_ME=1
export DSCODE_TEST_LOGOUT_VISION_KEY="vision-secret"
"#,
        )
        .unwrap();
        std::env::set_var("DSCODE_TEST_LOGOUT_MODEL_KEY", "model-secret");
        std::env::set_var("DSCODE_TEST_LOGOUT_VISION_KEY", "vision-secret");
        let store = RuntimeStore::new(root.join("runtime"));
        let mut app = TuiApp::new(Vec::new());

        handle_tui_action(
            &store,
            None,
            &mut app,
            TuiAction::Logout {
                workspace: root.display().to_string(),
            },
        )
        .unwrap();

        assert!(std::env::var_os("DSCODE_TEST_LOGOUT_MODEL_KEY").is_none());
        assert!(std::env::var_os("DSCODE_TEST_LOGOUT_VISION_KEY").is_none());
        let dotenv = fs::read_to_string(root.join(".env")).unwrap();
        assert!(!dotenv.contains("DSCODE_TEST_LOGOUT_MODEL_KEY"));
        assert!(!dotenv.contains("DSCODE_TEST_LOGOUT_VISION_KEY"));
        assert!(dotenv.contains("DSCODE_TEST_LOGOUT_BASE_MODEL_KEY"));
        assert!(dotenv.contains("KEEP_ME=1"));
        let (kind, detail) = app.mcp_detail_for_test().expect("logout detail");
        assert_eq!(kind, TuiMcpDetailKind::Logout);
        assert!(detail.contains("DSCODE_TEST_LOGOUT_MODEL_KEY: cleared"));
        assert!(render_once(&app, 120, 36)
            .unwrap()
            .contains("logged out: cleared"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn handle_tui_action_persists_masked_auth_credential() {
        let root = temp_root("auth-credential");
        fs::create_dir_all(root.join(".dscode")).unwrap();
        fs::write(root.join(".env"), "KEEP_ME=1\nDEEPSEEK_API_KEY=old\n").unwrap();
        let store = RuntimeStore::new(root.join("runtime"));
        let mut app = TuiApp::new(Vec::new());

        handle_tui_action(
            &store,
            None,
            &mut app,
            TuiAction::AuthCredential {
                workspace: root.display().to_string(),
                env_name: "DEEPSEEK_API_KEY".to_string(),
                secret: crate::tui::TuiSecretString::new("sk-tui-secret".to_string()),
            },
        )
        .unwrap();

        let dotenv = fs::read_to_string(root.join(".env")).unwrap();
        assert!(dotenv.contains("KEEP_ME=1"));
        assert!(dotenv.contains("DEEPSEEK_API_KEY=sk-tui-secret"));
        assert!(!dotenv.contains("old"));
        let (kind, detail) = app.mcp_detail_for_test().expect("auth detail");
        assert_eq!(kind, TuiMcpDetailKind::Setup);
        assert!(detail.contains("Value: present (hidden)"));
        assert!(!detail.contains("sk-tui-secret"));
        assert!(render_once(&app, 120, 36)
            .unwrap()
            .contains("auth credential stored: DEEPSEEK_API_KEY"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn posthoc_translation_heuristic_detects_english_leaks() {
        assert!(tui_output_needs_translation(
            "This answer is mostly English prose that should be translated for the user.",
            "Simplified Chinese",
        ));
        assert!(!tui_output_needs_translation(
            "这个回答已经是中文，只有 API 和 run_shell 这类技术词。",
            "Simplified Chinese",
        ));
        assert!(!tui_output_needs_translation(
            "This answer is already English.",
            "English",
        ));
    }

    #[test]
    fn posthoc_translation_failure_keeps_original_message() {
        let _guard = env_lock();
        std::env::remove_var("DSCODE_TEST_TRANSLATE_KEY");
        let mut config = AppConfig::default();
        config.model.api_key_env = "DSCODE_TEST_TRANSLATE_KEY".to_string();
        let mut result = RunResult {
            final_message: "This final answer is mostly English and needs fallback translation."
                .to_string(),
            ..RunResult::default()
        };

        apply_tui_posthoc_translation(&config, &mut result, Some("Simplified Chinese"));

        assert_eq!(
            result.final_message,
            "This final answer is mostly English and needs fallback translation."
        );
        assert_eq!(result.tool_events.len(), 1);
        assert_eq!(result.tool_events[0].tool_name, "posthoc_translate");
        assert_eq!(result.tool_events[0].status, ObservationStatus::Failed);
        assert!(result.tool_events[0].output.contains("kept original"));
    }

    #[test]
    fn handle_tui_action_lists_and_shows_skills() {
        let root = temp_root("skills-action");
        let store = RuntimeStore::new(root.join("runtime"));
        let user_skills = root.join("user-skills");
        fs::create_dir_all(&user_skills).unwrap();
        fs::write(
            user_skills.join("pr-review.toml"),
            r#"name = "pr-review"
description = "Review pull request changes"
allowed_tools = ["read_file", "git_diff"]
system_append = "Review carefully."
suggested_steps = ["inspect diff"]
triggers = ["review"]
references = ["docs/review.md"]

[policy]
require_write_confirmation = true
require_shell_confirmation = true
shell_allowlist = ["git diff"]
"#,
        )
        .unwrap();
        let mut config = temp_config(&root);
        config.workspace.user_skills_dir = user_skills.display().to_string();
        let mut app = TuiApp::new(Vec::new());

        handle_tui_action(
            &store,
            Some(&config),
            &mut app,
            TuiAction::Skills {
                command: TuiSkillsCommand::List {
                    prefix: Some("pr".to_string()),
                },
            },
        )
        .unwrap();

        {
            let (_, detail) = app.mcp_detail_for_test().expect("skills list detail");
            assert!(detail.contains("Available skills matching `pr`"));
            assert!(detail.contains("pr-review"));
        }
        let output = render_once(&app, 120, 36).unwrap();
        assert!(output.contains("pr-review"));

        handle_tui_action(
            &store,
            Some(&config),
            &mut app,
            TuiAction::Skills {
                command: TuiSkillsCommand::Show {
                    name: "pr-review".to_string(),
                },
            },
        )
        .unwrap();

        let (_, detail) = app.mcp_detail_for_test().expect("skill detail");
        assert!(detail.contains("# Skill: pr-review"));
        assert!(detail.contains("Review pull request changes"));
        assert!(detail.contains("Allowed tools"));
        assert!(detail.contains("git_diff"));
        assert!(detail.contains("require_write_confirmation: true"));

        handle_tui_action(
            &store,
            Some(&config),
            &mut app,
            TuiAction::Skills {
                command: TuiSkillsCommand::Show {
                    name: "skill-creator".to_string(),
                },
            },
        )
        .unwrap();

        let (_, detail) = app.mcp_detail_for_test().expect("skill creator detail");
        assert!(detail.contains("# Skill: skill-creator"));
        assert!(detail.contains("Create or refine DeepSeekCode TOML skills"));

        handle_tui_action(
            &store,
            Some(&config),
            &mut app,
            TuiAction::Skills {
                command: TuiSkillsCommand::Trust {
                    name: "pr-review".to_string(),
                },
            },
        )
        .unwrap();
        let marker = user_skills.join("pr-review.trusted");
        assert!(marker.is_file());
        let (_, detail) = app.mcp_detail_for_test().expect("skill trust detail");
        assert!(detail.contains("Skill `pr-review` trusted."));
        assert!(detail.contains("Trust marker:"));

        handle_tui_action(
            &store,
            Some(&config),
            &mut app,
            TuiAction::Skills {
                command: TuiSkillsCommand::Uninstall {
                    name: "pr-review".to_string(),
                },
            },
        )
        .unwrap();
        assert!(!user_skills.join("pr-review.toml").exists());
        assert!(!marker.exists());
        let (_, detail) = app.mcp_detail_for_test().expect("skill uninstall detail");
        assert!(detail.contains("Skill `pr-review` uninstalled."));
        assert!(detail.contains("Removed trust marker:"));

        handle_tui_action(
            &store,
            Some(&config),
            &mut app,
            TuiAction::Skills {
                command: TuiSkillsCommand::Uninstall {
                    name: "pr-review".to_string(),
                },
            },
        )
        .unwrap();
        let (_, detail) = app.mcp_detail_for_test().expect("bundled uninstall detail");
        assert!(detail.contains("Cannot uninstall bundled skill `pr-review`"));

        handle_tui_action(
            &store,
            Some(&config),
            &mut app,
            TuiAction::Skills {
                command: TuiSkillsCommand::Trust {
                    name: "pr-review".to_string(),
                },
            },
        )
        .unwrap();
        let (_, detail) = app.mcp_detail_for_test().expect("bundled trust detail");
        assert!(detail.contains("Cannot mark bundled skill `pr-review` trusted"));

        handle_tui_action(
            &store,
            Some(&config),
            &mut app,
            TuiAction::Skills {
                command: TuiSkillsCommand::Trust {
                    name: "missing-skill".to_string(),
                },
            },
        )
        .unwrap();
        let (_, detail) = app.mcp_detail_for_test().expect("missing trust detail");
        assert!(detail.contains("Skill `missing-skill` not found"));

        handle_tui_action(
            &store,
            Some(&config),
            &mut app,
            TuiAction::Skills {
                command: TuiSkillsCommand::Uninstall {
                    name: "missing-skill".to_string(),
                },
            },
        )
        .unwrap();
        let (_, detail) = app.mcp_detail_for_test().expect("missing uninstall detail");
        assert!(detail.contains("Skill `missing-skill` not found in user skills."));

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn parse_remote_skill_registry_sorts_entries() {
        let entries = parse_remote_skill_entries(
            r#"{"skills":{"zeta":{"description":"Z skill","source":"github:owner/zeta"},"alpha":{"description":"A skill","source":"https://example.com/a.tar.gz"}}}"#,
        )
        .unwrap();

        assert_eq!(
            entries,
            vec![
                RemoteSkillEntry {
                    name: "alpha".to_string(),
                    description: "A skill".to_string(),
                    source: "https://example.com/a.tar.gz".to_string(),
                },
                RemoteSkillEntry {
                    name: "zeta".to_string(),
                    description: "Z skill".to_string(),
                    source: "github:owner/zeta".to_string(),
                },
            ]
        );
    }

    #[test]
    fn github_skill_source_resolves_main_master_tarball_candidates() {
        let source = resolve_remote_skill_entry_source("github:owner/repo").unwrap();
        assert_eq!(
            source.candidate_urls,
            vec![
                "https://github.com/owner/repo/archive/refs/heads/main.tar.gz".to_string(),
                "https://github.com/owner/repo/archive/refs/heads/master.tar.gz".to_string(),
            ]
        );

        let source = resolve_remote_skill_entry_source("https://github.com/owner/repo.git")
            .expect("bare GitHub URL source");
        assert_eq!(
            source.candidate_urls[0],
            "https://github.com/owner/repo/archive/refs/heads/main.tar.gz"
        );
    }

    #[test]
    fn handle_tui_action_lists_remote_skill_registry() {
        let _guard = env_lock();
        let previous_allow_local = std::env::var("DSCODE_ALLOW_LOCAL_FETCH").ok();
        let previous_network_default = std::env::var("DSCODE_NETWORK_DEFAULT").ok();
        std::env::set_var("DSCODE_ALLOW_LOCAL_FETCH", "1");
        std::env::set_var("DSCODE_NETWORK_DEFAULT", "allow");

        let registry_url = serve_once_text(
            r#"{"skills":{"beta":{"description":"Beta skill","source":"github:team/beta"},"alpha":{"description":"Alpha skill","source":"https://example.com/alpha.tar.gz"}}}"#
                .to_string(),
        );
        let root = temp_root("remote-skills-action");
        let store = RuntimeStore::new(root.join("runtime"));
        let mut config = temp_config(&root);
        config.skills.registry_url = registry_url.clone();
        let mut app = TuiApp::new(Vec::new());

        handle_tui_action(
            &store,
            Some(&config),
            &mut app,
            TuiAction::Skills {
                command: TuiSkillsCommand::Remote,
            },
        )
        .unwrap();

        let (_, detail) = app
            .mcp_detail_for_test()
            .expect("remote skill registry detail");
        assert!(detail.contains("Available remote skills (2)"));
        assert!(detail.contains("- alpha: Alpha skill"));
        assert!(detail.contains("source: https://example.com/alpha.tar.gz"));
        assert!(detail.contains("- beta: Beta skill"));
        assert!(detail.contains("source: github:team/beta"));
        assert!(detail.contains(&format!("Registry: {registry_url}")));
        assert!(detail.contains("Use /skill install <name|url>"));
        assert!(detail.contains("GitHub, tar.gz, or zip skill sources"));

        restore_env_var("DSCODE_ALLOW_LOCAL_FETCH", previous_allow_local);
        restore_env_var("DSCODE_NETWORK_DEFAULT", previous_network_default);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn handle_tui_action_syncs_remote_toml_skill_cache() {
        let _guard = env_lock();
        let previous_allow_local = std::env::var("DSCODE_ALLOW_LOCAL_FETCH").ok();
        let previous_network_default = std::env::var("DSCODE_NETWORK_DEFAULT").ok();
        std::env::set_var("DSCODE_ALLOW_LOCAL_FETCH", "1");
        std::env::set_var("DSCODE_NETWORK_DEFAULT", "allow");

        let skill_url = serve_once_text(test_skill_toml("cache-triage", "Cache triage"));
        let registry_url = serve_once_text(format!(
            r#"{{"skills":{{"cache-triage":{{"description":"Cache triage","source":"{skill_url}"}},"unsupported":{{"description":"Unsupported skill","source":"ftp://example.com/repo.toml"}}}}}}"#
        ));
        let root = temp_root("skill-sync-cache");
        let store = RuntimeStore::new(root.join("runtime"));
        let cache_dir = root.join("skill-cache");
        let mut config = temp_config(&root);
        config.skills.registry_url = registry_url;
        config.skills.cache_dir = cache_dir.display().to_string();
        let mut app = TuiApp::new(Vec::new());

        handle_tui_action(
            &store,
            Some(&config),
            &mut app,
            TuiAction::Skills {
                command: TuiSkillsCommand::Sync,
            },
        )
        .unwrap();

        assert!(cache_dir.join("cache-triage.toml").is_file());
        assert!(cache_dir.join("cache-triage.sync-meta").is_file());
        let (_, detail) = app.mcp_detail_for_test().expect("skill sync detail");
        assert!(detail.contains("Remote skill registry sync complete."));
        assert!(detail.contains("cache-triage downloaded"));
        assert!(detail.contains("unsupported skipped"));
        assert!(detail
            .contains("2 skill(s) processed: 1 downloaded, 0 up-to-date, 1 skipped, 0 failed."));

        restore_env_var("DSCODE_ALLOW_LOCAL_FETCH", previous_allow_local);
        restore_env_var("DSCODE_NETWORK_DEFAULT", previous_network_default);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn handle_tui_action_syncs_remote_tarball_skill_cache() {
        let _guard = env_lock();
        let previous_allow_local = std::env::var("DSCODE_ALLOW_LOCAL_FETCH").ok();
        let previous_network_default = std::env::var("DSCODE_NETWORK_DEFAULT").ok();
        std::env::set_var("DSCODE_ALLOW_LOCAL_FETCH", "1");
        std::env::set_var("DSCODE_NETWORK_DEFAULT", "allow");

        let skill_md = test_skill_md(
            "archive-triage",
            "Archive triage",
            "Use archived workflow instructions.",
        );
        let archive_url = serve_once_bytes(
            test_skill_tarball("repo-main/skills/archive-triage/SKILL.md", &skill_md),
            "application/gzip",
        );
        let registry_url = serve_once_text(format!(
            r#"{{"skills":{{"archive-triage":{{"description":"Archive triage","source":"{archive_url}"}}}}}}"#
        ));
        let root = temp_root("skill-sync-archive");
        let store = RuntimeStore::new(root.join("runtime"));
        let cache_dir = root.join("skill-cache");
        let mut config = temp_config(&root);
        config.skills.registry_url = registry_url;
        config.skills.cache_dir = cache_dir.display().to_string();
        let mut app = TuiApp::new(Vec::new());

        handle_tui_action(
            &store,
            Some(&config),
            &mut app,
            TuiAction::Skills {
                command: TuiSkillsCommand::Sync,
            },
        )
        .unwrap();

        let cached = cache_dir.join("archive-triage.toml");
        assert!(cached.is_file());
        let cached_body = fs::read_to_string(cached).unwrap();
        assert!(cached_body.contains("Imported from a SKILL.md bundle."));
        assert!(cached_body.contains("Use archived workflow instructions."));
        let (_, detail) = app.mcp_detail_for_test().expect("archive sync detail");
        assert!(detail.contains("archive-triage downloaded"));
        assert!(detail
            .contains("1 skill(s) processed: 1 downloaded, 0 up-to-date, 0 skipped, 0 failed."));

        restore_env_var("DSCODE_ALLOW_LOCAL_FETCH", previous_allow_local);
        restore_env_var("DSCODE_NETWORK_DEFAULT", previous_network_default);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn handle_tui_action_syncs_remote_zip_skill_cache() {
        let _guard = env_lock();
        let previous_allow_local = std::env::var("DSCODE_ALLOW_LOCAL_FETCH").ok();
        let previous_network_default = std::env::var("DSCODE_NETWORK_DEFAULT").ok();
        std::env::set_var("DSCODE_ALLOW_LOCAL_FETCH", "1");
        std::env::set_var("DSCODE_NETWORK_DEFAULT", "allow");

        let skill_md = test_skill_md(
            "zip-triage",
            "Zip triage",
            "Use zipped workflow instructions.",
        );
        let archive_url = serve_once_bytes(
            test_skill_zip("repo-main/skills/zip-triage/SKILL.md", &skill_md),
            "application/zip",
        );
        let registry_url = serve_once_text(format!(
            r#"{{"skills":{{"zip-triage":{{"description":"Zip triage","source":"{archive_url}"}}}}}}"#
        ));
        let root = temp_root("skill-sync-zip");
        let store = RuntimeStore::new(root.join("runtime"));
        let cache_dir = root.join("skill-cache");
        let mut config = temp_config(&root);
        config.skills.registry_url = registry_url;
        config.skills.cache_dir = cache_dir.display().to_string();
        let mut app = TuiApp::new(Vec::new());

        handle_tui_action(
            &store,
            Some(&config),
            &mut app,
            TuiAction::Skills {
                command: TuiSkillsCommand::Sync,
            },
        )
        .unwrap();

        let cached = cache_dir.join("zip-triage.toml");
        assert!(cached.is_file());
        let cached_body = fs::read_to_string(cached).unwrap();
        assert!(cached_body.contains("Imported from a SKILL.md bundle."));
        assert!(cached_body.contains("Use zipped workflow instructions."));
        let (_, detail) = app.mcp_detail_for_test().expect("zip sync detail");
        assert!(detail.contains("zip-triage downloaded"));
        assert!(detail
            .contains("1 skill(s) processed: 1 downloaded, 0 up-to-date, 0 skipped, 0 failed."));

        restore_env_var("DSCODE_ALLOW_LOCAL_FETCH", previous_allow_local);
        restore_env_var("DSCODE_NETWORK_DEFAULT", previous_network_default);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn sync_remote_skill_entry_reports_fresh_when_checksum_matches() {
        let _guard = env_lock();
        let previous_allow_local = std::env::var("DSCODE_ALLOW_LOCAL_FETCH").ok();
        let previous_network_default = std::env::var("DSCODE_NETWORK_DEFAULT").ok();
        std::env::set_var("DSCODE_ALLOW_LOCAL_FETCH", "1");
        std::env::set_var("DSCODE_NETWORK_DEFAULT", "allow");

        let root = temp_root("skill-sync-fresh");
        let cache_dir = root.join("skill-cache");
        fs::create_dir_all(&cache_dir).unwrap();
        let body = test_skill_toml("cache-triage", "Cache triage");
        let skill_path = cache_dir.join("cache-triage.toml");
        fs::write(&skill_path, &body).unwrap();
        let source = serve_once_text(body.clone());
        write_skill_sync_marker(
            &cache_dir.join("cache-triage.sync-meta"),
            "cache-triage",
            &source,
            &checksum_hex(&body),
        )
        .unwrap();

        let outcome = sync_remote_skill_entry(
            &RemoteSkillEntry {
                name: "cache-triage".to_string(),
                description: "Cache triage".to_string(),
                source,
            },
            &cache_dir,
        );

        match outcome {
            SkillSyncEntryOutcome::Fresh { name } => assert_eq!(name, "cache-triage"),
            _ => panic!("expected fresh sync outcome"),
        }

        restore_env_var("DSCODE_ALLOW_LOCAL_FETCH", previous_allow_local);
        restore_env_var("DSCODE_NETWORK_DEFAULT", previous_network_default);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn format_remote_skill_registry_reports_network_denial() {
        let _guard = env_lock();
        let previous_allow_local = std::env::var("DSCODE_ALLOW_LOCAL_FETCH").ok();
        let previous_network_default = std::env::var("DSCODE_NETWORK_DEFAULT").ok();
        std::env::set_var("DSCODE_ALLOW_LOCAL_FETCH", "1");
        std::env::set_var("DSCODE_NETWORK_DEFAULT", "deny");

        let root = temp_root("remote-skills-deny");
        let mut config = temp_config(&root);
        config.skills.registry_url = "http://127.0.0.1:9/index.json".to_string();

        let detail = format_remote_skills_summary(&config);
        assert!(detail.contains("Remote skill registry unavailable."));
        assert!(detail.contains("network policy denied host"));
        assert!(detail.contains("network allow <host>"));

        restore_env_var("DSCODE_ALLOW_LOCAL_FETCH", previous_allow_local);
        restore_env_var("DSCODE_NETWORK_DEFAULT", previous_network_default);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn format_remote_skill_registry_reports_parse_failure() {
        let _guard = env_lock();
        let previous_allow_local = std::env::var("DSCODE_ALLOW_LOCAL_FETCH").ok();
        let previous_network_default = std::env::var("DSCODE_NETWORK_DEFAULT").ok();
        std::env::set_var("DSCODE_ALLOW_LOCAL_FETCH", "1");
        std::env::set_var("DSCODE_NETWORK_DEFAULT", "allow");

        let registry_url = serve_once_text(r#"{"unexpected":true}"#.to_string());
        let root = temp_root("remote-skills-parse");
        let mut config = temp_config(&root);
        config.skills.registry_url = registry_url;

        let detail = format_remote_skills_summary(&config);
        assert!(detail.contains("Remote skill registry could not be parsed."));
        assert!(detail.contains("remote skill registry missing `skills`"));
        assert!(detail.contains("Expected JSON with a top-level `skills` object."));

        restore_env_var("DSCODE_ALLOW_LOCAL_FETCH", previous_allow_local);
        restore_env_var("DSCODE_NETWORK_DEFAULT", previous_network_default);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn handle_tui_action_installs_direct_toml_skill() {
        let _guard = env_lock();
        let previous_allow_local = std::env::var("DSCODE_ALLOW_LOCAL_FETCH").ok();
        let previous_network_default = std::env::var("DSCODE_NETWORK_DEFAULT").ok();
        std::env::set_var("DSCODE_ALLOW_LOCAL_FETCH", "1");
        std::env::set_var("DSCODE_NETWORK_DEFAULT", "allow");

        let skill_url = serve_once_text(test_skill_toml("triage", "Triage issues"));
        let root = temp_root("skill-install-direct");
        let store = RuntimeStore::new(root.join("runtime"));
        let user_skills = root.join("user-skills");
        let mut config = temp_config(&root);
        config.workspace.user_skills_dir = user_skills.display().to_string();
        let mut app = TuiApp::new(Vec::new());

        handle_tui_action(
            &store,
            Some(&config),
            &mut app,
            TuiAction::Skills {
                command: TuiSkillsCommand::Install {
                    source: skill_url.clone(),
                },
            },
        )
        .unwrap();

        let skill_path = user_skills.join("triage.toml");
        assert!(skill_path.is_file());
        assert!(user_skills.join("triage.installed-from").is_file());
        let (_, detail) = app.mcp_detail_for_test().expect("install detail");
        assert!(detail.contains("Skill `triage` installed."));
        assert!(detail.contains(&format!("URL: {skill_url}")));

        restore_env_var("DSCODE_ALLOW_LOCAL_FETCH", previous_allow_local);
        restore_env_var("DSCODE_NETWORK_DEFAULT", previous_network_default);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn handle_tui_action_installs_direct_skill_md_import() {
        let _guard = env_lock();
        let previous_allow_local = std::env::var("DSCODE_ALLOW_LOCAL_FETCH").ok();
        let previous_network_default = std::env::var("DSCODE_NETWORK_DEFAULT").ok();
        std::env::set_var("DSCODE_ALLOW_LOCAL_FETCH", "1");
        std::env::set_var("DSCODE_NETWORK_DEFAULT", "allow");

        let skill_url = serve_once_text(test_skill_md(
            "md-triage",
            "Markdown triage",
            "Use Markdown skill instructions.",
        ));
        let root = temp_root("skill-install-skill-md");
        let store = RuntimeStore::new(root.join("runtime"));
        let user_skills = root.join("user-skills");
        let mut config = temp_config(&root);
        config.workspace.user_skills_dir = user_skills.display().to_string();
        let mut app = TuiApp::new(Vec::new());

        handle_tui_action(
            &store,
            Some(&config),
            &mut app,
            TuiAction::Skills {
                command: TuiSkillsCommand::Install {
                    source: skill_url.clone(),
                },
            },
        )
        .unwrap();

        let skill_path = user_skills.join("md-triage.toml");
        assert!(skill_path.is_file());
        let body = fs::read_to_string(&skill_path).unwrap();
        assert!(body.contains("Imported from a SKILL.md bundle."));
        assert!(body.contains("Use Markdown skill instructions."));
        let (_, detail) = app.mcp_detail_for_test().expect("skill md install detail");
        assert!(detail.contains("Skill `md-triage` installed."));
        assert!(detail.contains(&format!("URL: {skill_url}")));

        restore_env_var("DSCODE_ALLOW_LOCAL_FETCH", previous_allow_local);
        restore_env_var("DSCODE_NETWORK_DEFAULT", previous_network_default);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn handle_tui_action_installs_registry_toml_skill() {
        let _guard = env_lock();
        let previous_allow_local = std::env::var("DSCODE_ALLOW_LOCAL_FETCH").ok();
        let previous_network_default = std::env::var("DSCODE_NETWORK_DEFAULT").ok();
        std::env::set_var("DSCODE_ALLOW_LOCAL_FETCH", "1");
        std::env::set_var("DSCODE_NETWORK_DEFAULT", "allow");

        let skill_url = serve_once_text(test_skill_toml("registry-triage", "Registry triage"));
        let registry_url = serve_once_text(format!(
            r#"{{"skills":{{"remote-triage":{{"description":"Remote triage","source":"{skill_url}"}}}}}}"#
        ));
        let root = temp_root("skill-install-registry");
        let store = RuntimeStore::new(root.join("runtime"));
        let user_skills = root.join("user-skills");
        let mut config = temp_config(&root);
        config.workspace.user_skills_dir = user_skills.display().to_string();
        config.skills.registry_url = registry_url;
        let mut app = TuiApp::new(Vec::new());

        handle_tui_action(
            &store,
            Some(&config),
            &mut app,
            TuiAction::Skills {
                command: TuiSkillsCommand::Install {
                    source: "remote-triage".to_string(),
                },
            },
        )
        .unwrap();

        assert!(user_skills.join("registry-triage.toml").is_file());
        let (_, detail) = app.mcp_detail_for_test().expect("registry install detail");
        assert!(detail.contains("Skill `registry-triage` installed."));
        assert!(detail.contains("Source: remote-triage"));

        restore_env_var("DSCODE_ALLOW_LOCAL_FETCH", previous_allow_local);
        restore_env_var("DSCODE_NETWORK_DEFAULT", previous_network_default);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn handle_tui_action_installs_registry_tarball_skill() {
        let _guard = env_lock();
        let previous_allow_local = std::env::var("DSCODE_ALLOW_LOCAL_FETCH").ok();
        let previous_network_default = std::env::var("DSCODE_NETWORK_DEFAULT").ok();
        std::env::set_var("DSCODE_ALLOW_LOCAL_FETCH", "1");
        std::env::set_var("DSCODE_NETWORK_DEFAULT", "allow");

        let skill_md = test_skill_md(
            "archive-triage",
            "Archive triage",
            "Use archive install instructions.",
        );
        let archive_url = serve_once_bytes(
            test_skill_tarball("repo-main/SKILL.md", &skill_md),
            "application/gzip",
        );
        let registry_url = serve_once_text(format!(
            r#"{{"skills":{{"remote-archive":{{"description":"Remote archive","source":"{archive_url}"}}}}}}"#
        ));
        let root = temp_root("skill-install-registry-archive");
        let store = RuntimeStore::new(root.join("runtime"));
        let user_skills = root.join("user-skills");
        let mut config = temp_config(&root);
        config.workspace.user_skills_dir = user_skills.display().to_string();
        config.skills.registry_url = registry_url;
        let mut app = TuiApp::new(Vec::new());

        handle_tui_action(
            &store,
            Some(&config),
            &mut app,
            TuiAction::Skills {
                command: TuiSkillsCommand::Install {
                    source: "remote-archive".to_string(),
                },
            },
        )
        .unwrap();

        let skill_path = user_skills.join("archive-triage.toml");
        assert!(skill_path.is_file());
        let body = fs::read_to_string(skill_path).unwrap();
        assert!(body.contains("Use archive install instructions."));
        let (_, detail) = app
            .mcp_detail_for_test()
            .expect("registry archive install detail");
        assert!(detail.contains("Skill `archive-triage` installed."));
        assert!(detail.contains("Source: remote-archive"));

        restore_env_var("DSCODE_ALLOW_LOCAL_FETCH", previous_allow_local);
        restore_env_var("DSCODE_NETWORK_DEFAULT", previous_network_default);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn handle_tui_action_updates_installed_toml_skill() {
        let _guard = env_lock();
        let previous_allow_local = std::env::var("DSCODE_ALLOW_LOCAL_FETCH").ok();
        let previous_network_default = std::env::var("DSCODE_NETWORK_DEFAULT").ok();
        std::env::set_var("DSCODE_ALLOW_LOCAL_FETCH", "1");
        std::env::set_var("DSCODE_NETWORK_DEFAULT", "allow");

        let root = temp_root("skill-update");
        let store = RuntimeStore::new(root.join("runtime"));
        let user_skills = root.join("user-skills");
        fs::create_dir_all(&user_skills).unwrap();
        let old_body = test_skill_toml("triage", "Old triage");
        let new_body = test_skill_toml("triage", "Updated triage");
        let skill_path = user_skills.join("triage.toml");
        fs::write(&skill_path, old_body).unwrap();
        let update_url = serve_once_text(new_body.clone());
        write_installed_from_marker(
            &user_skills.join("triage.installed-from"),
            &update_url,
            &update_url,
            "old-checksum",
        )
        .unwrap();
        let mut config = temp_config(&root);
        config.workspace.user_skills_dir = user_skills.display().to_string();
        let mut app = TuiApp::new(Vec::new());

        handle_tui_action(
            &store,
            Some(&config),
            &mut app,
            TuiAction::Skills {
                command: TuiSkillsCommand::Update {
                    name: "triage".to_string(),
                },
            },
        )
        .unwrap();

        assert_eq!(fs::read_to_string(&skill_path).unwrap(), new_body);
        let marker = fs::read_to_string(user_skills.join("triage.installed-from")).unwrap();
        assert!(marker.contains(&checksum_hex(&new_body)));
        let (_, detail) = app.mcp_detail_for_test().expect("update detail");
        assert!(detail.contains("Skill `triage` updated."));

        restore_env_var("DSCODE_ALLOW_LOCAL_FETCH", previous_allow_local);
        restore_env_var("DSCODE_NETWORK_DEFAULT", previous_network_default);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn handle_tui_action_updates_installed_skill_md_import() {
        let _guard = env_lock();
        let previous_allow_local = std::env::var("DSCODE_ALLOW_LOCAL_FETCH").ok();
        let previous_network_default = std::env::var("DSCODE_NETWORK_DEFAULT").ok();
        std::env::set_var("DSCODE_ALLOW_LOCAL_FETCH", "1");
        std::env::set_var("DSCODE_NETWORK_DEFAULT", "allow");

        let root = temp_root("skill-update-skill-md");
        let store = RuntimeStore::new(root.join("runtime"));
        let user_skills = root.join("user-skills");
        fs::create_dir_all(&user_skills).unwrap();
        let old_toml =
            skill_md_to_toml(&test_skill_md("md-triage", "Old triage", "Old body.")).unwrap();
        let new_md = test_skill_md("md-triage", "Updated triage", "Updated body.");
        let new_toml = skill_md_to_toml(&new_md).unwrap();
        let skill_path = user_skills.join("md-triage.toml");
        fs::write(&skill_path, old_toml).unwrap();
        let update_url = serve_once_text(new_md);
        write_installed_from_marker(
            &user_skills.join("md-triage.installed-from"),
            &update_url,
            &update_url,
            "old-checksum",
        )
        .unwrap();
        let mut config = temp_config(&root);
        config.workspace.user_skills_dir = user_skills.display().to_string();
        let mut app = TuiApp::new(Vec::new());

        handle_tui_action(
            &store,
            Some(&config),
            &mut app,
            TuiAction::Skills {
                command: TuiSkillsCommand::Update {
                    name: "md-triage".to_string(),
                },
            },
        )
        .unwrap();

        assert_eq!(fs::read_to_string(&skill_path).unwrap(), new_toml);
        let marker = fs::read_to_string(user_skills.join("md-triage.installed-from")).unwrap();
        assert!(marker.contains(&checksum_hex(&new_toml)));
        let (_, detail) = app.mcp_detail_for_test().expect("skill md update detail");
        assert!(detail.contains("Skill `md-triage` updated."));

        restore_env_var("DSCODE_ALLOW_LOCAL_FETCH", previous_allow_local);
        restore_env_var("DSCODE_NETWORK_DEFAULT", previous_network_default);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn handle_tui_action_installs_direct_zip_skill() {
        let _guard = env_lock();
        let previous_allow_local = std::env::var("DSCODE_ALLOW_LOCAL_FETCH").ok();
        let previous_network_default = std::env::var("DSCODE_NETWORK_DEFAULT").ok();
        std::env::set_var("DSCODE_ALLOW_LOCAL_FETCH", "1");
        std::env::set_var("DSCODE_NETWORK_DEFAULT", "allow");

        let skill_md = test_skill_md("zip-triage", "Zip triage", "Use zip install instructions.");
        let archive_url = serve_once_bytes(
            test_skill_zip("repo-main/skills/zip-triage/SKILL.md", &skill_md),
            "application/zip",
        );
        let root = temp_root("skill-install-zip");
        let store = RuntimeStore::new(root.join("runtime"));
        let user_skills = root.join("user-skills");
        let mut config = temp_config(&root);
        config.workspace.user_skills_dir = user_skills.display().to_string();
        let mut app = TuiApp::new(Vec::new());

        handle_tui_action(
            &store,
            Some(&config),
            &mut app,
            TuiAction::Skills {
                command: TuiSkillsCommand::Install {
                    source: archive_url.clone(),
                },
            },
        )
        .unwrap();

        let skill_path = user_skills.join("zip-triage.toml");
        assert!(skill_path.is_file());
        assert!(user_skills.join("zip-triage.installed-from").is_file());
        let body = fs::read_to_string(skill_path).unwrap();
        assert!(body.contains("Use zip install instructions."));
        let (_, detail) = app.mcp_detail_for_test().expect("zip install detail");
        assert!(detail.contains("Skill `zip-triage` installed."));
        assert!(detail.contains(&format!("URL: {archive_url}")));

        restore_env_var("DSCODE_ALLOW_LOCAL_FETCH", previous_allow_local);
        restore_env_var("DSCODE_NETWORK_DEFAULT", previous_network_default);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn skill_zip_archive_rejects_unsafe_path() {
        let skill_md = test_skill_md("zip-triage", "Zip triage", "Unsafe.");
        for path in ["../SKILL.md", "/SKILL.md"] {
            let error = skill_source_bytes_to_toml(
                "https://example.com/repo.zip",
                &test_skill_zip(path, &skill_md),
            )
            .unwrap_err();

            assert!(
                error
                    .to_string()
                    .contains("skill zip archive entry escapes destination"),
                "{path} should be rejected"
            );
        }
    }

    #[test]
    fn skill_zip_archive_reports_missing_skill_md() {
        let error = skill_source_bytes_to_toml(
            "https://example.com/repo.zip",
            &test_skill_zip("repo-main/README.md", "no skill here"),
        )
        .unwrap_err();

        assert!(error
            .to_string()
            .contains("missing SKILL.md in skill zip archive"));
    }

    #[test]
    fn skill_zip_archive_prefers_root_skill_md() {
        let first = test_skill_md("nested-zip", "Nested zip", "Nested body.");
        let second = test_skill_md("root-zip", "Root zip", "Root body.");
        let cursor = std::io::Cursor::new(Vec::new());
        let mut writer = zip::ZipWriter::new(cursor);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored)
            .unix_permissions(0o644);
        writer
            .start_file("repo-main/skills/nested-zip/SKILL.md", options)
            .unwrap();
        writer.write_all(first.as_bytes()).unwrap();
        writer.start_file("repo-main/SKILL.md", options).unwrap();
        writer.write_all(second.as_bytes()).unwrap();
        let archive = writer.finish().unwrap().into_inner();

        let toml = skill_source_bytes_to_toml("https://example.com/repo.zip", &archive).unwrap();
        assert!(toml.contains("name = \"root-zip\""));
        assert!(toml.contains("Root body."));
        assert!(!toml.contains("Nested body."));
    }

    #[test]
    fn handle_tui_action_records_approval_response() {
        let store = temp_store("approval-response");
        let session = store
            .create_session("Daily work".to_string(), ".".to_string())
            .unwrap();
        let thread = store
            .create_thread_for_session(
                &session.id,
                "Runtime permissions".to_string(),
                ".".to_string(),
                "deepseek-coder".to_string(),
                "agent".to_string(),
            )
            .unwrap();
        let request = store
            .append_permission_request(
                &thread.id,
                None,
                "run_shell".to_string(),
                "shell".to_string(),
                "cargo test".to_string(),
                BTreeMap::new(),
            )
            .unwrap();
        let mut app = app_from_store(&store).unwrap();

        handle_tui_action(
            &store,
            None,
            &mut app,
            TuiAction::RespondApproval {
                thread_id: thread.id.clone(),
                turn_id: None,
                request_id: request.id.clone(),
                decision: "approved".to_string(),
                scope: None,
            },
        )
        .unwrap();
        refresh_app_from_store(&store, &mut app).unwrap();

        let events = store.read_events(&thread.id, 0).unwrap();
        assert_eq!(events.last().unwrap().kind, "permission_response");
        let output = render_once(&app, 120, 36).unwrap();
        assert!(!output.contains("Approval Modal"));
        assert!(!output.contains("cargo test"));
    }

    #[test]
    fn handle_tui_action_records_user_input_response() {
        let store = temp_store("user-input-response");
        let session = store
            .create_session("Daily work".to_string(), ".".to_string())
            .unwrap();
        let thread = store
            .create_thread_for_session(
                &session.id,
                "Runtime input".to_string(),
                ".".to_string(),
                "deepseek-coder".to_string(),
                "agent".to_string(),
            )
            .unwrap();
        let request = store
            .append_user_input_request(
                &thread.id,
                None,
                JsonValue::Array(vec![json_object([
                    ("header", JsonValue::String("Mode".to_string())),
                    ("id", JsonValue::String("mode".to_string())),
                    (
                        "question",
                        JsonValue::String("Which mode should be used?".to_string()),
                    ),
                    (
                        "options",
                        JsonValue::Array(vec![
                            json_object([
                                ("label", JsonValue::String("Plan".to_string())),
                                ("description", JsonValue::String("Plan first.".to_string())),
                            ]),
                            json_object([
                                ("label", JsonValue::String("Apply".to_string())),
                                (
                                    "description",
                                    JsonValue::String("Implement directly.".to_string()),
                                ),
                            ]),
                        ]),
                    ),
                ])]),
            )
            .unwrap();
        let mut app = app_from_store(&store).unwrap();

        handle_tui_action(
            &store,
            None,
            &mut app,
            TuiAction::RespondUserInput {
                thread_id: thread.id.clone(),
                turn_id: None,
                request_id: request.id.clone(),
                answers: BTreeMap::from([("mode".to_string(), "Plan".to_string())]),
            },
        )
        .unwrap();
        refresh_app_from_store(&store, &mut app).unwrap();

        let events = store.read_events(&thread.id, 0).unwrap();
        assert_eq!(events.last().unwrap().kind, "user_input_response");
        let output = render_once(&app, 120, 36).unwrap();
        assert!(!output.contains("User Input Modal"));
        assert!(!output.contains("Which mode should be used?"));
    }

    #[test]
    fn handle_tui_action_records_cancel_request() {
        let store = temp_store("cancel-action");
        let session = store
            .create_session("Daily work".to_string(), ".".to_string())
            .unwrap();
        let thread = store
            .create_thread_for_session(
                &session.id,
                "Runtime cancel".to_string(),
                ".".to_string(),
                "deepseek-coder".to_string(),
                "agent".to_string(),
            )
            .unwrap();
        let turn = store
            .append_turn(&thread.id, "assistant".to_string(), "running".to_string())
            .unwrap();
        let mut app = app_from_store(&store).unwrap();

        handle_tui_action(
            &store,
            None,
            &mut app,
            TuiAction::CancelRun {
                thread_id: thread.id.clone(),
                turn_id: Some(turn.id.clone()),
            },
        )
        .unwrap();

        let events = store.read_events(&thread.id, 0).unwrap();
        assert_eq!(events.last().unwrap().kind, "cancel_requested");
        assert_eq!(
            events.last().unwrap().turn_id.as_deref(),
            Some(turn.id.as_str())
        );
        assert!(runtime_cancel_requested(&store, &thread.id, &turn.id, 0).unwrap());
    }

    #[test]
    fn handle_tui_action_creates_pending_runtime_task() {
        let store = temp_store("create-task-action");
        let session = store
            .create_session("Daily work".to_string(), ".".to_string())
            .unwrap();
        let thread = store
            .create_thread_for_session(
                &session.id,
                "Runtime tasks".to_string(),
                ".".to_string(),
                "deepseek-coder".to_string(),
                "agent".to_string(),
            )
            .unwrap();
        let mut app = app_from_store(&store).unwrap();

        handle_tui_action(
            &store,
            None,
            &mut app,
            TuiAction::CreateTask {
                thread_id: thread.id.clone(),
                summary: "inspect flaky test".to_string(),
            },
        )
        .unwrap();

        let tasks = store
            .list_tasks(Some(&session.id), Some(&thread.id), 10)
            .unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].kind, "agent");
        assert_eq!(tasks[0].status, "pending");
        assert_eq!(tasks[0].summary, "inspect flaky test");
        assert!(store
            .read_events(&thread.id, 0)
            .unwrap()
            .iter()
            .any(|event| event.kind == "task_recorded"));
        let output = render_once(&app, 160, 48).unwrap();
        assert!(output.contains("created pending task"));
    }

    #[test]
    fn handle_tui_action_creates_pending_subagent_runtime_task() {
        let store = temp_store("create-subagent-task-action");
        let session = store
            .create_session("Daily work".to_string(), ".".to_string())
            .unwrap();
        let thread = store
            .create_thread_for_session(
                &session.id,
                "Runtime subagents".to_string(),
                ".".to_string(),
                "deepseek-coder".to_string(),
                "agent".to_string(),
            )
            .unwrap();
        let mut app = app_from_store(&store).unwrap();

        handle_tui_action(
            &store,
            None,
            &mut app,
            TuiAction::CreateSubagentTask {
                thread_id: thread.id.clone(),
                task: "inspect parity gap".to_string(),
                max_depth: 2,
            },
        )
        .unwrap();

        let tasks = store
            .list_tasks(Some(&session.id), Some(&thread.id), 10)
            .unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].kind, "subagent");
        assert_eq!(tasks[0].status, "pending");
        assert_eq!(tasks[0].summary, "max_depth=2: inspect parity gap");
        assert!(store
            .read_events(&thread.id, 0)
            .unwrap()
            .iter()
            .any(|event| event.kind == "task_recorded"));
        let output = render_once(&app, 160, 48).unwrap();
        assert!(output.contains("created pending subagent task"));
    }

    #[test]
    fn handle_tui_action_pauses_and_resumes_runtime_task() {
        let store = temp_store("pause-resume-task-action");
        let session = store
            .create_session("Daily work".to_string(), ".".to_string())
            .unwrap();
        let thread = store
            .create_thread_for_session(
                &session.id,
                "Runtime tasks".to_string(),
                ".".to_string(),
                "deepseek-coder".to_string(),
                "agent".to_string(),
            )
            .unwrap();
        let task = store
            .create_task(
                Some(&session.id),
                Some(&thread.id),
                None,
                "agent".to_string(),
                "pending".to_string(),
                "inspect flaky test".to_string(),
            )
            .unwrap();
        let mut app = app_from_store(&store).unwrap();

        handle_tui_action(
            &store,
            None,
            &mut app,
            TuiAction::PauseTask {
                task_id: task.id.clone(),
            },
        )
        .unwrap();
        assert_eq!(store.load_task(&task.id).unwrap().status, "paused");
        assert!(render_once(&app, 160, 48).unwrap().contains("paused task"));

        handle_tui_action(
            &store,
            None,
            &mut app,
            TuiAction::ResumeTask {
                task_id: task.id.clone(),
            },
        )
        .unwrap();
        assert_eq!(store.load_task(&task.id).unwrap().status, "pending");
        assert!(render_once(&app, 160, 48).unwrap().contains("resumed task"));
    }

    #[test]
    fn handle_tui_action_cancels_runtime_task() {
        let store = temp_store("cancel-task-action");
        let session = store
            .create_session("Daily work".to_string(), ".".to_string())
            .unwrap();
        let thread = store
            .create_thread_for_session(
                &session.id,
                "Runtime tasks".to_string(),
                ".".to_string(),
                "deepseek-coder".to_string(),
                "agent".to_string(),
            )
            .unwrap();
        let task = store
            .create_task(
                Some(&session.id),
                Some(&thread.id),
                None,
                "agent".to_string(),
                "running".to_string(),
                "inspect flaky test".to_string(),
            )
            .unwrap();
        let mut app = app_from_store(&store).unwrap();

        handle_tui_action(
            &store,
            None,
            &mut app,
            TuiAction::CancelTask {
                task_id: task.id.clone(),
            },
        )
        .unwrap();

        assert_eq!(store.load_task(&task.id).unwrap().status, "cancelled");
        let events = store.read_events(&thread.id, 0).unwrap();
        assert!(events.iter().any(|event| event.kind == "cancel_requested"
            && json_as_object(&event.payload)
                .and_then(|payload| payload.get("task_id"))
                .and_then(json_as_string)
                .is_some_and(|task_id| task_id == task.id)));
        assert!(render_once(&app, 160, 48)
            .unwrap()
            .contains("cancelled task"));
    }

    #[test]
    fn handle_tui_action_manages_project_mcp_config() {
        let root = temp_root("mcp-manager-action");
        fs::create_dir_all(&root).unwrap();

        let mut config = temp_config(&root);
        let store = RuntimeStore::new(root.join(".dscode/runtime"));
        let mut app = TuiApp::new(Vec::new());
        let mcp_path = root.join(".dscode/mcp.json");
        let user_mcp_path = root.join("user-mcp.json");
        config.mcp.project_file = mcp_path.display().to_string();
        config.mcp.user_file = user_mcp_path.display().to_string();

        handle_tui_action(
            &store,
            Some(&config),
            &mut app,
            TuiAction::McpInit { force: false },
        )
        .unwrap();
        assert!(mcp_path.exists());
        assert!(render_once(&app, 160, 48)
            .unwrap()
            .contains("mcp project config initialized"));

        handle_tui_action(&store, Some(&config), &mut app, TuiAction::McpList).unwrap();
        assert!(render_once(&app, 160, 48).unwrap().contains("mcp servers="));

        handle_tui_action(&store, Some(&config), &mut app, TuiAction::McpManager).unwrap();
        let output = render_once(&app, 160, 48).unwrap();
        assert!(output.contains("MCP Manager"));
        assert!(output.contains("example-filesystem"));
        assert!(output.contains("Discovery refresh"));
        assert!(output.contains("mcp validate: ok"));
        assert!(output.contains("Available actions"));
        assert!(!output.contains("Transcript"));

        handle_tui_action(
            &store,
            Some(&config),
            &mut app,
            TuiAction::McpDetails {
                kind: TuiMcpDetailKind::Tools,
                server: None,
            },
        )
        .unwrap();
        let output = render_once(&app, 160, 48).unwrap();
        assert!(output.contains("MCP Tools"));
        assert!(output.contains("No enabled MCP servers configured"));

        handle_tui_action(
            &store,
            Some(&config),
            &mut app,
            TuiAction::McpManagerDetails {
                kind: TuiMcpDetailKind::Tools,
                server: None,
            },
        )
        .unwrap();
        let output = render_once(&app, 160, 48).unwrap();
        assert!(output.contains("MCP Tools"));
        assert!(output.contains("No enabled MCP servers configured"));
        assert!(!output.contains("Transcript"));

        handle_tui_action(&store, Some(&config), &mut app, TuiAction::McpValidate).unwrap();
        let output = render_once(&app, 160, 48).unwrap();
        assert!(output.contains("MCP Health"));
        assert!(output.contains("mcp validate: ok"));

        handle_tui_action(
            &store,
            Some(&config),
            &mut app,
            TuiAction::McpAddRemote {
                scope: TuiMcpConfigScope::Project,
                name: "remote".to_string(),
                transport: "http".to_string(),
                url: "http://127.0.0.1:3999/mcp".to_string(),
            },
        )
        .unwrap();
        let content = fs::read_to_string(&mcp_path).unwrap();
        assert!(content.contains("\"remote\""));
        assert!(content.contains("http://127.0.0.1:3999/mcp"));

        handle_tui_action(
            &store,
            Some(&config),
            &mut app,
            TuiAction::McpSetEnabled {
                scope: TuiMcpConfigScope::Project,
                name: "remote".to_string(),
                enabled: false,
            },
        )
        .unwrap();
        assert!(fs::read_to_string(&mcp_path)
            .unwrap()
            .contains("\"enabled\":false"));

        handle_tui_action(
            &store,
            Some(&config),
            &mut app,
            TuiAction::McpSetEnabled {
                scope: TuiMcpConfigScope::Project,
                name: "remote".to_string(),
                enabled: true,
            },
        )
        .unwrap();
        assert!(fs::read_to_string(&mcp_path)
            .unwrap()
            .contains("\"enabled\":true"));

        handle_tui_action(
            &store,
            Some(&config),
            &mut app,
            TuiAction::McpRemove {
                scope: TuiMcpConfigScope::Project,
                name: "remote".to_string(),
            },
        )
        .unwrap();
        assert!(!fs::read_to_string(&mcp_path)
            .unwrap()
            .contains("\"remote\""));

        handle_tui_action(
            &store,
            Some(&config),
            &mut app,
            TuiAction::McpAddRemote {
                scope: TuiMcpConfigScope::User,
                name: "shared".to_string(),
                transport: "http".to_string(),
                url: "http://127.0.0.1:4000/mcp".to_string(),
            },
        )
        .unwrap();
        assert!(fs::read_to_string(&user_mcp_path)
            .unwrap()
            .contains("\"shared\""));
        assert!(!fs::read_to_string(&mcp_path)
            .unwrap()
            .contains("\"shared\""));
    }

    #[test]
    fn handle_tui_action_lists_shows_hunks_and_restores_rollback_snapshot() {
        let repo = temp_root("rollback-action");
        fs::create_dir_all(&repo).unwrap();
        run_git(&repo, &["init"]);
        run_git(&repo, &["config", "user.email", "test@example.com"]);
        run_git(&repo, &["config", "user.name", "Deepseek Test"]);
        fs::write(repo.join("src.txt"), "base\n").unwrap();
        run_git(&repo, &["add", "src.txt"]);
        run_git(&repo, &["commit", "-m", "initial"]);

        fs::write(repo.join("src.txt"), "snapshot\n").unwrap();
        let config = temp_config(&repo);
        let rollback_store =
            RollbackStore::new(PathBuf::from(&config.workspace.config_dir).join("rollback"));
        let snapshot = rollback_store
            .create_snapshot(&repo, "before TUI turn".to_string())
            .unwrap();
        rollback_store
            .bind_snapshot_runtime(&snapshot.id, Some("thread-one"), Some("turn-one"))
            .unwrap();
        fs::write(repo.join("src.txt"), "after\n").unwrap();

        let store = RuntimeStore::new(repo.join(".dscode/runtime"));
        let mut app = TuiApp::new(Vec::new());

        handle_tui_action(
            &store,
            Some(&config),
            &mut app,
            TuiAction::ListRollbackSnapshots { limit: 20 },
        )
        .unwrap();
        assert!(render_once(&app, 160, 48)
            .unwrap()
            .contains("rollback snapshots=1"));
        assert!(render_once(&app, 160, 48)
            .unwrap()
            .contains("Rollback snapshots"));

        handle_tui_action(
            &store,
            Some(&config),
            &mut app,
            TuiAction::ShowRollbackSnapshot {
                id: "turn-one".to_string(),
            },
        )
        .unwrap();
        assert!(render_once(&app, 160, 48)
            .unwrap()
            .contains(snapshot.id.as_str()));
        let rendered = render_once(&app, 160, 48).unwrap();
        assert!(rendered.contains("Patch preview"));
        assert!(rendered.contains("src.txt"));

        handle_tui_action(
            &store,
            Some(&config),
            &mut app,
            TuiAction::ShowRollbackHunk {
                id: "turn-one".to_string(),
                hunk: None,
            },
        )
        .unwrap();
        let rendered = render_once(&app, 160, 48).unwrap();
        assert!(rendered.contains("Rollback patch hunks"));
        assert!(rendered.contains("#1 src.txt"));

        handle_tui_action(
            &store,
            Some(&config),
            &mut app,
            TuiAction::ShowRollbackHunk {
                id: "turn-one".to_string(),
                hunk: Some(1),
            },
        )
        .unwrap();
        let rendered = render_once(&app, 160, 48).unwrap();
        assert!(rendered.contains("Hunk 1/"));
        assert!(rendered.contains("-base"));
        assert!(rendered.contains("+snapshot"));

        handle_tui_action(
            &store,
            Some(&config),
            &mut app,
            TuiAction::RevertTurn {
                id: "turn-one".to_string(),
                apply: false,
            },
        )
        .unwrap();
        assert_eq!(fs::read_to_string(repo.join("src.txt")).unwrap(), "after\n");
        assert!(render_once(&app, 160, 48)
            .unwrap()
            .contains("dry-run rollback"));
        assert!(render_once(&app, 160, 48).unwrap().contains("Dry-run only"));

        handle_tui_action(
            &store,
            Some(&config),
            &mut app,
            TuiAction::RevertTurn {
                id: "turn-one".to_string(),
                apply: true,
            },
        )
        .unwrap();
        assert_eq!(
            fs::read_to_string(repo.join("src.txt")).unwrap(),
            "snapshot\n"
        );
        assert!(render_once(&app, 160, 48)
            .unwrap()
            .contains("restored rollback"));
        assert!(render_once(&app, 160, 48)
            .unwrap()
            .contains("changed files: 1"));
    }

    #[test]
    fn handle_tui_action_restores_single_rollback_hunk() {
        let repo = temp_root("rollback-hunk-action");
        fs::create_dir_all(&repo).unwrap();
        run_git(&repo, &["init"]);
        run_git(&repo, &["config", "user.email", "test@example.com"]);
        run_git(&repo, &["config", "user.name", "Deepseek Test"]);
        let base = (1..=24)
            .map(|line| format!("line {line}"))
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
        fs::write(repo.join("src.txt"), &base).unwrap();
        run_git(&repo, &["add", "src.txt"]);
        run_git(&repo, &["commit", "-m", "initial"]);

        let mut snapshot_lines = (1..=24)
            .map(|line| format!("line {line}"))
            .collect::<Vec<_>>();
        snapshot_lines[1] = "line 2 snapshot".to_string();
        snapshot_lines[21] = "line 22 snapshot".to_string();
        fs::write(repo.join("src.txt"), snapshot_lines.join("\n") + "\n").unwrap();
        let config = temp_config(&repo);
        let rollback_store =
            RollbackStore::new(PathBuf::from(&config.workspace.config_dir).join("rollback"));
        let snapshot = rollback_store
            .create_snapshot(&repo, "two hunk snapshot".to_string())
            .unwrap();
        run_git(&repo, &["checkout", "--", "src.txt"]);

        let store = RuntimeStore::new(repo.join(".dscode/runtime"));
        let mut app = TuiApp::new(Vec::new());

        handle_tui_action(
            &store,
            Some(&config),
            &mut app,
            TuiAction::RestoreRollbackHunk {
                id: snapshot.id.clone(),
                hunk: 1,
                apply: false,
            },
        )
        .unwrap();
        assert_eq!(fs::read_to_string(repo.join("src.txt")).unwrap(), base);
        let rendered = render_once(&app, 160, 48).unwrap();
        assert!(rendered.contains("Rollback hunk restore plan"));
        assert!(rendered.contains("Dry-run only"));

        handle_tui_action(
            &store,
            Some(&config),
            &mut app,
            TuiAction::RestoreRollbackHunk {
                id: snapshot.id.clone(),
                hunk: 1,
                apply: true,
            },
        )
        .unwrap();

        let restored = fs::read_to_string(repo.join("src.txt")).unwrap();
        assert!(restored.contains("line 2 snapshot"));
        assert!(restored.contains("line 22\n"));
        assert!(!restored.contains("line 22 snapshot"));
        let rendered = render_once(&app, 160, 48).unwrap();
        assert!(rendered.contains("restored rollback hunk"));
        assert!(rendered.contains("changed files: 1"));
    }

    #[test]
    fn run_tui_diagnostics_reports_status() {
        let root = temp_root("diagnostics-action");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("README.md"), "# docs\n").unwrap();
        let mut app = TuiApp::new(Vec::new());

        run_tui_diagnostics_in(&mut app, &root, false, vec!["README.md".to_string()]);

        assert!(render_once(&app, 160, 48).unwrap().contains("diagnostics"));
    }

    #[test]
    fn handle_tui_action_manages_memory_file() {
        let root = temp_root("memory-action");
        fs::create_dir_all(&root).unwrap();
        let store = RuntimeStore::new(root.join(".dscode/runtime"));
        let mut config = temp_config(&root);
        config.memory.enabled = true;
        config.memory.memory_path = root.join("memory.md").display().to_string();
        let memory_path = config.memory.memory_path();
        let mut app = TuiApp::new(Vec::new());

        handle_tui_action(
            &store,
            Some(&config),
            &mut app,
            TuiAction::AppendMemory {
                note: "# prefer cargo fmt".to_string(),
            },
        )
        .unwrap();
        assert!(fs::read_to_string(&memory_path)
            .unwrap()
            .contains("prefer cargo fmt"));
        assert!(render_once(&app, 160, 48).unwrap().contains("remembered"));

        handle_tui_action(
            &store,
            Some(&config),
            &mut app,
            TuiAction::Memory {
                command: TuiMemoryCommand::Show,
            },
        )
        .unwrap();
        let output = render_once(&app, 160, 48).unwrap();
        assert!(output.contains("Memory"));
        assert!(output.contains("memory enabled"));

        handle_tui_action(
            &store,
            Some(&config),
            &mut app,
            TuiAction::Memory {
                command: TuiMemoryCommand::Clear,
            },
        )
        .unwrap();
        assert_eq!(fs::read_to_string(&memory_path).unwrap(), "");
        assert!(render_once(&app, 160, 48)
            .unwrap()
            .contains("memory cleared"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn handle_tui_action_manages_note_file() {
        let root = temp_root("note-action");
        fs::create_dir_all(&root).unwrap();
        let store = RuntimeStore::new(root.join(".dscode/runtime"));
        let mut config = temp_config(&root);
        config.memory.notes_path = root.join("notes.md").display().to_string();
        let notes_path = config.memory.notes_path();
        let mut app = TuiApp::new(Vec::new());

        handle_tui_action(
            &store,
            Some(&config),
            &mut app,
            TuiAction::Note {
                command: TuiNoteCommand::Add {
                    content: "keep release notes short".to_string(),
                },
            },
        )
        .unwrap();
        assert!(fs::read_to_string(&notes_path)
            .unwrap()
            .contains("keep release notes short"));

        handle_tui_action(
            &store,
            Some(&config),
            &mut app,
            TuiAction::Note {
                command: TuiNoteCommand::List,
            },
        )
        .unwrap();
        let output = render_once(&app, 160, 48).unwrap();
        assert!(output.contains("Notes"));
        assert!(output.contains("keep release notes short"));

        handle_tui_action(
            &store,
            Some(&config),
            &mut app,
            TuiAction::Note {
                command: TuiNoteCommand::Edit {
                    index: 1,
                    content: "updated note".to_string(),
                },
            },
        )
        .unwrap();
        assert!(fs::read_to_string(&notes_path)
            .unwrap()
            .contains("updated note"));

        handle_tui_action(
            &store,
            Some(&config),
            &mut app,
            TuiAction::Note {
                command: TuiNoteCommand::Remove { index: 1 },
            },
        )
        .unwrap();
        assert_eq!(fs::read_to_string(&notes_path).unwrap().trim(), "");

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn handle_tui_action_manages_anchor_file() {
        let root = temp_root("anchor-action");
        fs::create_dir_all(&root).unwrap();
        let store = RuntimeStore::new(root.join(".dscode/runtime"));
        let mut app = TuiApp::new(Vec::new());
        let anchors_path = root.join(".dscode/anchors.md");

        handle_tui_action(
            &store,
            None,
            &mut app,
            TuiAction::Anchor {
                workspace: root.display().to_string(),
                command: TuiAnchorCommand::Add {
                    content: "Never touch .ssh".to_string(),
                },
            },
        )
        .unwrap();
        assert!(fs::read_to_string(&anchors_path)
            .unwrap()
            .contains("Never touch .ssh"));

        handle_tui_action(
            &store,
            None,
            &mut app,
            TuiAction::Anchor {
                workspace: root.display().to_string(),
                command: TuiAnchorCommand::List,
            },
        )
        .unwrap();
        let output = render_once(&app, 160, 48).unwrap();
        assert!(output.contains("Anchors"));
        assert!(output.contains("Never touch .ssh"));

        handle_tui_action(
            &store,
            None,
            &mut app,
            TuiAction::Anchor {
                workspace: root.display().to_string(),
                command: TuiAnchorCommand::Remove { index: 1 },
            },
        )
        .unwrap();
        assert_eq!(fs::read_to_string(&anchors_path).unwrap().trim(), "");

        handle_tui_action(
            &store,
            None,
            &mut app,
            TuiAction::Anchor {
                workspace: root.display().to_string(),
                command: TuiAnchorCommand::Path,
            },
        )
        .unwrap();
        let output = render_once(&app, 160, 48).unwrap();
        assert!(output.contains(".dscode/anchors.md"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn handle_tui_action_skips_empty_share_without_upload() {
        let root = temp_root("share-empty-action");
        fs::create_dir_all(&root).unwrap();
        let store = RuntimeStore::new(root.join(".dscode/runtime"));
        let thread = store
            .create_thread(
                "Share me".to_string(),
                root.display().to_string(),
                "deepseek-v4-pro".to_string(),
                "agent".to_string(),
            )
            .unwrap();
        let mut app = TuiApp::new(Vec::new());

        handle_tui_action(
            &store,
            None,
            &mut app,
            TuiAction::ShareSession {
                thread_id: thread.id,
            },
        )
        .unwrap();

        let output = render_once(&app, 160, 48).unwrap();
        assert!(output.contains("Share export skipped"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn render_tui_share_html_escapes_transcript_content() {
        let thread = ThreadRecord {
            id: "thread-one".to_string(),
            session_id: Some("session-one".to_string()),
            created_at: "0".to_string(),
            updated_at: "1".to_string(),
            title: "Share <thread>".to_string(),
            workspace: "/tmp/share".to_string(),
            model: "deepseek-v4-pro".to_string(),
            mode: "agent".to_string(),
            status: "active".to_string(),
            latest_turn_id: Some("turn-one".to_string()),
            event_seq: 1,
        };
        let session = SessionRecord {
            id: "session-one".to_string(),
            created_at: "0".to_string(),
            updated_at: "1".to_string(),
            title: "Session & Co".to_string(),
            workspace: "/tmp/share".to_string(),
            status: "active".to_string(),
            active_thread_id: Some("thread-one".to_string()),
            thread_count: 1,
        };
        let items = vec![ItemRecord {
            id: "item-one".to_string(),
            thread_id: "thread-one".to_string(),
            turn_id: Some("turn-one".to_string()),
            index: 1,
            item_type: "message".to_string(),
            role: Some("user".to_string()),
            content: "<script>alert('x')</script>".to_string(),
            status: "completed".to_string(),
            created_at: "1".to_string(),
        }];

        let html = render_tui_share_html(Some(&session), &thread, &items);

        assert!(html.contains("DeepSeekCode TUI Session"));
        assert!(html.contains("Session &amp; Co"));
        assert!(html.contains("Share &lt;thread&gt;"));
        assert!(html.contains("&lt;script&gt;alert(&#39;x&#39;)&lt;/script&gt;"));
        assert!(!html.contains("<script>alert"));
    }

    #[test]
    fn handle_tui_action_exports_thread_markdown() {
        let root = temp_root("export-action");
        fs::create_dir_all(&root).unwrap();
        let store = RuntimeStore::new(root.join(".dscode/runtime"));
        let session = store
            .create_session("Daily work".to_string(), root.display().to_string())
            .unwrap();
        let thread = store
            .create_thread_for_session(
                &session.id,
                "Export me".to_string(),
                root.display().to_string(),
                "deepseek-v4-pro".to_string(),
                "agent".to_string(),
            )
            .unwrap();
        let user_turn = store
            .append_turn(&thread.id, "user".to_string(), "hello".to_string())
            .unwrap();
        store
            .append_item(
                &thread.id,
                Some(&user_turn.id),
                "message".to_string(),
                Some("user".to_string()),
                "hello from user".to_string(),
                "completed".to_string(),
            )
            .unwrap();
        store
            .append_item(
                &thread.id,
                None,
                "message".to_string(),
                Some("assistant".to_string()),
                "hello from assistant".to_string(),
                "completed".to_string(),
            )
            .unwrap();
        let mut app = app_from_store(&store).unwrap();

        handle_tui_action(
            &store,
            None,
            &mut app,
            TuiAction::ExportThread {
                thread_id: thread.id,
                path: Some("exports/chat.md".to_string()),
            },
        )
        .unwrap();

        let export_path = root.join("exports/chat.md");
        let markdown = fs::read_to_string(&export_path).unwrap();
        assert!(markdown.contains("# Chat Export"));
        assert!(markdown.contains("**Session:** Daily work"));
        assert!(markdown.contains("**Model:** deepseek-v4-pro"));
        assert!(markdown.contains("**You:**"));
        assert!(markdown.contains("hello from user"));
        assert!(markdown.contains("**Assistant:**"));
        assert!(markdown.contains("hello from assistant"));
        let (_, detail) = app.mcp_detail_for_test().expect("export detail");
        assert!(detail.contains("Export complete"));
        assert!(detail.contains("exports/chat.md"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn handle_tui_action_saves_and_loads_session_snapshot() {
        let root = temp_root("save-load-action");
        fs::create_dir_all(&root).unwrap();
        let store = RuntimeStore::new(root.join(".dscode/runtime"));
        let session = store
            .create_session("Daily work".to_string(), root.display().to_string())
            .unwrap();
        let thread = store
            .create_thread_for_session(
                &session.id,
                "Snapshot me".to_string(),
                root.display().to_string(),
                "deepseek-v4-pro".to_string(),
                "agent".to_string(),
            )
            .unwrap();
        let user_turn = store
            .append_turn(&thread.id, "user".to_string(), "hello".to_string())
            .unwrap();
        store
            .append_item(
                &thread.id,
                Some(&user_turn.id),
                "message".to_string(),
                Some("user".to_string()),
                "hello from user".to_string(),
                "completed".to_string(),
            )
            .unwrap();
        store
            .append_item(
                &thread.id,
                None,
                "message".to_string(),
                Some("assistant".to_string()),
                "hello from assistant".to_string(),
                "completed".to_string(),
            )
            .unwrap();
        let mut app = app_from_store(&store).unwrap();

        handle_tui_action(
            &store,
            None,
            &mut app,
            TuiAction::SaveSession {
                session_id: session.id.clone(),
                thread_id: thread.id.clone(),
                path: Some("snapshots/session.json".to_string()),
            },
        )
        .unwrap();

        let snapshot_path = root.join("snapshots/session.json");
        let snapshot = fs::read_to_string(&snapshot_path).unwrap();
        assert!(snapshot.contains(TUI_SESSION_SNAPSHOT_KIND));
        assert!(snapshot.contains("Daily work"));
        assert!(snapshot.contains("hello from assistant"));
        assert!(render_once(&app, 160, 48)
            .unwrap()
            .contains("Save complete"));

        handle_tui_action(
            &store,
            None,
            &mut app,
            TuiAction::LoadSession {
                workspace: root.display().to_string(),
                path: "snapshots/session.json".to_string(),
            },
        )
        .unwrap();

        let sessions = store.list_sessions(20).unwrap();
        assert!(sessions
            .iter()
            .any(|record| record.title == "Imported: Daily work"));
        let imported_thread = store
            .list_threads(20)
            .unwrap()
            .into_iter()
            .find(|record| record.title == "Imported: Snapshot me")
            .expect("imported thread");
        let imported_items = store.list_items(&imported_thread.id, None).unwrap();
        assert_eq!(imported_items.len(), 2);
        assert!(imported_items
            .iter()
            .any(|item| item.content == "hello from assistant"));
        assert!(render_once(&app, 160, 48)
            .unwrap()
            .contains("Load complete"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn handle_tui_action_prunes_old_sessions() {
        let root = temp_root("prune-sessions");
        fs::create_dir_all(&root).unwrap();
        let store = RuntimeStore::new(root.join(".dscode/runtime"));
        let old_session = store
            .create_session("Old work".to_string(), root.display().to_string())
            .unwrap();
        store
            .create_thread_for_session(
                &old_session.id,
                "Old thread".to_string(),
                root.display().to_string(),
                "deepseek-v4-pro".to_string(),
                "agent".to_string(),
            )
            .unwrap();
        let fresh_session = store
            .create_session("Fresh work".to_string(), root.display().to_string())
            .unwrap();
        let mut old_session_record = store.load_session(&old_session.id).unwrap();
        old_session_record.updated_at = "epoch+1".to_string();
        fs::write(
            store
                .root()
                .join("sessions")
                .join(format!("{}.json", old_session_record.id)),
            json_value_to_string(&session_to_json(&old_session_record)),
        )
        .unwrap();
        let mut app = app_from_store(&store).unwrap();

        handle_tui_action(&store, None, &mut app, TuiAction::PruneSessions { days: 1 }).unwrap();

        assert!(render_once(&app, 160, 48)
            .unwrap()
            .contains("pruned 1 session older than 1d"));
        let sessions = store.list_sessions(20).unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].id, fresh_session.id);

        handle_tui_action(&store, None, &mut app, TuiAction::PruneSessions { days: 1 }).unwrap();

        assert!(render_once(&app, 160, 48)
            .unwrap()
            .contains("no sessions older than 1d to prune"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn render_tui_export_markdown_includes_empty_transcript_note() {
        let thread = ThreadRecord {
            id: "thread-one".to_string(),
            session_id: None,
            created_at: "0".to_string(),
            updated_at: "1".to_string(),
            title: "Empty export".to_string(),
            workspace: "/tmp/export".to_string(),
            model: "deepseek-v4-pro".to_string(),
            mode: "agent".to_string(),
            status: "active".to_string(),
            latest_turn_id: None,
            event_seq: 1,
        };

        let markdown = render_tui_export_markdown(None, &thread, &[]);

        assert!(markdown.contains("# Chat Export"));
        assert!(markdown.contains("**Thread:** Empty export"));
        assert!(markdown.contains("No transcript items."));
    }

    #[test]
    fn handle_tui_action_clears_conversation_by_switching_to_new_thread() {
        let root = temp_root("clear-action");
        fs::create_dir_all(&root).unwrap();
        let store = RuntimeStore::new(root.join(".dscode/runtime"));
        let session = store
            .create_session("Daily work".to_string(), root.display().to_string())
            .unwrap();
        let thread = store
            .create_thread_for_session(
                &session.id,
                "Old conversation".to_string(),
                root.display().to_string(),
                "deepseek-v4-pro".to_string(),
                "agent".to_string(),
            )
            .unwrap();
        store
            .append_item(
                &thread.id,
                None,
                "message".to_string(),
                Some("user".to_string()),
                "old transcript".to_string(),
                "completed".to_string(),
            )
            .unwrap();
        let mut app = app_from_store(&store).unwrap();

        handle_tui_action(
            &store,
            None,
            &mut app,
            TuiAction::ClearConversation {
                session_id: session.id.clone(),
                previous_thread_id: Some(thread.id.clone()),
            },
        )
        .unwrap();

        let loaded_session = store.load_session(&session.id).unwrap();
        let new_thread_id = loaded_session.active_thread_id.expect("active thread");
        assert_ne!(new_thread_id, thread.id);
        let new_thread = store.load_thread(&new_thread_id).unwrap();
        assert_eq!(new_thread.title, "New conversation");
        assert_eq!(new_thread.model, "deepseek-v4-pro");
        assert!(store.list_items(&new_thread_id, None).unwrap().is_empty());
        let output = render_once(&app, 160, 48).unwrap();
        assert!(output.contains("New conversation"));
        assert!(!output.contains("old transcript"));
        assert!(output.contains("cleared conversation; new active thread"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn handle_tui_action_undo_forks_before_latest_user_turn() {
        let root = temp_root("undo-action");
        fs::create_dir_all(&root).unwrap();
        let store = RuntimeStore::new(root.join(".dscode/runtime"));
        let session = store
            .create_session("Daily work".to_string(), root.display().to_string())
            .unwrap();
        let thread = store
            .create_thread_for_session(
                &session.id,
                "Active conversation".to_string(),
                root.display().to_string(),
                "deepseek-v4-pro".to_string(),
                "agent".to_string(),
            )
            .unwrap();
        let first_user = store
            .append_turn(&thread.id, "user".to_string(), "first request".to_string())
            .unwrap();
        store
            .append_item(
                &thread.id,
                Some(&first_user.id),
                "message".to_string(),
                Some("user".to_string()),
                "first request".to_string(),
                "completed".to_string(),
            )
            .unwrap();
        let first_assistant = store
            .append_turn(
                &thread.id,
                "assistant".to_string(),
                "first answer".to_string(),
            )
            .unwrap();
        store
            .append_item(
                &thread.id,
                Some(&first_assistant.id),
                "message".to_string(),
                Some("assistant".to_string()),
                "first answer".to_string(),
                "completed".to_string(),
            )
            .unwrap();
        let second_user = store
            .append_turn(&thread.id, "user".to_string(), "second request".to_string())
            .unwrap();
        store
            .append_item(
                &thread.id,
                Some(&second_user.id),
                "message".to_string(),
                Some("user".to_string()),
                "second request".to_string(),
                "completed".to_string(),
            )
            .unwrap();
        let second_assistant = store
            .append_turn(
                &thread.id,
                "assistant".to_string(),
                "second answer".to_string(),
            )
            .unwrap();
        store
            .append_item(
                &thread.id,
                Some(&second_assistant.id),
                "message".to_string(),
                Some("assistant".to_string()),
                "second answer".to_string(),
                "completed".to_string(),
            )
            .unwrap();
        let mut app = app_from_store(&store).unwrap();

        handle_tui_action(
            &store,
            None,
            &mut app,
            TuiAction::UndoConversation {
                thread_id: thread.id.clone(),
            },
        )
        .unwrap();

        let active_thread_id = store
            .load_session(&session.id)
            .unwrap()
            .active_thread_id
            .expect("active thread");
        assert_ne!(active_thread_id, thread.id);
        let active_turns = store.list_turns(&active_thread_id).unwrap();
        assert_eq!(active_turns.len(), 2);
        assert_eq!(active_turns[0].content, "first request");
        assert_eq!(active_turns[1].content, "first answer");
        let active_items = store.list_items(&active_thread_id, None).unwrap();
        assert!(active_items
            .iter()
            .any(|item| item.content == "first answer"));
        assert!(!active_items
            .iter()
            .any(|item| item.content == "second request"));
        assert_eq!(store.list_turns(&thread.id).unwrap().len(), 4);
        assert!(render_once(&app, 160, 48)
            .unwrap()
            .contains("undid latest exchange"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn handle_tui_action_retry_and_edit_resubmit_on_rollback_fork() {
        let root = temp_root("retry-edit-action");
        fs::create_dir_all(&root).unwrap();
        let store = RuntimeStore::new(root.join(".dscode/runtime"));
        let session = store
            .create_session("Daily work".to_string(), root.display().to_string())
            .unwrap();
        let thread = store
            .create_thread_for_session(
                &session.id,
                "Active conversation".to_string(),
                root.display().to_string(),
                "deepseek-v4-pro".to_string(),
                "agent".to_string(),
            )
            .unwrap();
        let first_user = store
            .append_turn(&thread.id, "user".to_string(), "first request".to_string())
            .unwrap();
        store
            .append_item(
                &thread.id,
                Some(&first_user.id),
                "message".to_string(),
                Some("user".to_string()),
                "first request".to_string(),
                "completed".to_string(),
            )
            .unwrap();
        let first_assistant = store
            .append_turn(
                &thread.id,
                "assistant".to_string(),
                "first answer".to_string(),
            )
            .unwrap();
        store
            .append_item(
                &thread.id,
                Some(&first_assistant.id),
                "message".to_string(),
                Some("assistant".to_string()),
                "first answer".to_string(),
                "completed".to_string(),
            )
            .unwrap();
        let second_user = store
            .append_turn(&thread.id, "user".to_string(), "second request".to_string())
            .unwrap();
        store
            .append_item(
                &thread.id,
                Some(&second_user.id),
                "message".to_string(),
                Some("user".to_string()),
                "second request".to_string(),
                "completed".to_string(),
            )
            .unwrap();
        let second_assistant = store
            .append_turn(
                &thread.id,
                "assistant".to_string(),
                "second answer".to_string(),
            )
            .unwrap();
        store
            .append_item(
                &thread.id,
                Some(&second_assistant.id),
                "message".to_string(),
                Some("assistant".to_string()),
                "second answer".to_string(),
                "completed".to_string(),
            )
            .unwrap();
        let mut app = app_from_store(&store).unwrap();

        handle_tui_action(
            &store,
            None,
            &mut app,
            TuiAction::RetryUserMessage {
                thread_id: thread.id.clone(),
            },
        )
        .unwrap();

        let retry_thread_id = store
            .load_session(&session.id)
            .unwrap()
            .active_thread_id
            .expect("retry active thread");
        let retry_turns = store.list_turns(&retry_thread_id).unwrap();
        assert_eq!(retry_turns.len(), 3);
        assert_eq!(retry_turns[2].role, "user");
        assert_eq!(retry_turns[2].content, "second request");
        let retry_items = store.list_items(&retry_thread_id, None).unwrap();
        assert!(retry_items
            .iter()
            .any(|item| item.content == "second request"));
        assert!(!retry_items
            .iter()
            .any(|item| item.content == "second answer"));
        assert!(render_once(&app, 160, 48).unwrap().contains("retrying on"));

        handle_tui_action(
            &store,
            None,
            &mut app,
            TuiAction::SubmitEditedUserMessage {
                thread_id: thread.id.clone(),
                content: "revised second request".to_string(),
            },
        )
        .unwrap();

        let edit_thread_id = store
            .load_session(&session.id)
            .unwrap()
            .active_thread_id
            .expect("edit active thread");
        assert_ne!(edit_thread_id, retry_thread_id);
        let edit_turns = store.list_turns(&edit_thread_id).unwrap();
        assert_eq!(edit_turns.len(), 3);
        assert_eq!(edit_turns[2].content, "revised second request");
        assert!(render_once(&app, 160, 48)
            .unwrap()
            .contains("submitted edited message"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn handle_tui_action_runs_recall_archive() {
        let root = temp_root("recall-action");
        fs::create_dir_all(&root).unwrap();
        let store = RuntimeStore::new(root.join(".dscode/runtime"));
        let thread = store
            .create_thread(
                "Recall thread".to_string(),
                root.display().to_string(),
                "deepseek-v4-pro".to_string(),
                "agent".to_string(),
            )
            .unwrap();
        let turn = store
            .append_turn(
                &thread.id,
                "assistant".to_string(),
                "The invoice reconciliation issue is in sync.".to_string(),
            )
            .unwrap();
        store
            .append_item(
                &thread.id,
                Some(&turn.id),
                "summary".to_string(),
                Some("assistant".to_string()),
                "Carry forward invoice reconciliation details.".to_string(),
                "completed".to_string(),
            )
            .unwrap();
        let mut app = TuiApp::new(Vec::new());

        handle_tui_action(
            &store,
            None,
            &mut app,
            TuiAction::RecallArchive {
                workspace: root.display().to_string(),
                thread_id: Some(thread.id.clone()),
                query: "invoice reconciliation".to_string(),
            },
        )
        .unwrap();

        let output = render_once(&app, 160, 48).unwrap();
        assert!(output.contains("Recall Archive"));
        assert!(output.contains("invoice reconciliation"));
        assert!(output.contains("\"hits\""));
        assert!(output.contains("recall complete"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn handle_tui_action_runs_review_target() {
        let root = temp_root("review-action");
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(
            root.join("src/lib.rs"),
            "fn demo(value: Option<u8>) { println!(\"{:?}\", value.unwrap()); }\n",
        )
        .unwrap();
        let store = RuntimeStore::new(root.join(".dscode/runtime"));
        let mut app = TuiApp::new(Vec::new());

        handle_tui_action(
            &store,
            None,
            &mut app,
            TuiAction::ReviewTarget {
                workspace: root.display().to_string(),
                target: "src/lib.rs".to_string(),
            },
        )
        .unwrap();

        let (kind, detail) = app.mcp_detail_for_test().expect("review detail");
        assert_eq!(kind, TuiMcpDetailKind::Review);
        assert!(detail.contains("src/lib.rs"));
        assert!(detail.contains("\"panic-prone error handling\""));

        let output = render_once(&app, 160, 48).unwrap();
        assert!(output.contains("Review"));
        assert!(output.contains("review complete"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn handle_tui_action_renders_workspace_diff() {
        let root = temp_root("diff-action");
        fs::create_dir_all(&root).unwrap();
        run_git(&root, &["init"]);
        run_git(&root, &["config", "user.email", "test@example.com"]);
        run_git(&root, &["config", "user.name", "Deepseek Test"]);
        fs::write(root.join("src.txt"), "before\n").unwrap();
        run_git(&root, &["add", "src.txt"]);
        run_git(&root, &["commit", "-m", "initial"]);
        fs::write(root.join("src.txt"), "before\nafter\n").unwrap();

        let store = RuntimeStore::new(root.join(".dscode/runtime"));
        let mut app = TuiApp::new(Vec::new());

        handle_tui_action(
            &store,
            None,
            &mut app,
            TuiAction::ShowDiff {
                workspace: root.display().to_string(),
            },
        )
        .unwrap();

        let output = render_once(&app, 160, 48).unwrap();
        assert!(output.contains("DeepSeekCode Diff"));
        assert!(output.contains("Changed files (1):"));
        assert!(output.contains("src.txt"));
        assert!(output.contains("Stat"));
        assert!(output.contains("diff shown"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn handle_tui_action_renders_hooks_inventory() {
        let root = temp_root("hooks-action");
        fs::create_dir_all(&root).unwrap();
        let store = RuntimeStore::new(root.join(".dscode/runtime"));
        let mut config = temp_config(&root);
        let project_hooks = root.join(".dscode/hooks");
        let user_hooks = root.join("user-hooks");
        fs::create_dir_all(project_hooks.join("pre_tool_use")).unwrap();
        fs::create_dir_all(user_hooks.join("shell_env")).unwrap();
        let project_script = project_hooks.join("pre_tool_use/10-block");
        let user_script = user_hooks.join("shell_env/10-env");
        fs::write(&project_script, "#!/bin/sh\nexit 0\n").unwrap();
        fs::write(&user_script, "#!/bin/sh\nprintf 'FOO=bar\\n'\n").unwrap();
        make_executable(&project_script);
        make_executable(&user_script);
        config.hooks.enabled = true;
        config.hooks.timeout_ms = 1234;
        config.hooks.project_dir = project_hooks.display().to_string();
        config.hooks.user_dir = user_hooks.display().to_string();
        let mut app = TuiApp::new(Vec::new());

        handle_tui_action(
            &store,
            Some(&config),
            &mut app,
            TuiAction::Hooks {
                command: TuiHooksCommand::List,
            },
        )
        .unwrap();
        let (kind, detail) = app.mcp_detail_for_test().expect("hooks detail");
        assert_eq!(kind, TuiMcpDetailKind::Hooks);
        assert!(detail.contains("Hooks"));
        assert!(detail.contains("Enabled: true"));
        assert!(detail.contains("Timeout: 1234 ms"));
        assert!(detail.contains("pre_tool_use"));
        assert!(detail.contains("10-block"));
        assert!(detail.contains("shell_env"));
        assert!(detail.contains("10-env"));

        handle_tui_action(
            &store,
            Some(&config),
            &mut app,
            TuiAction::Hooks {
                command: TuiHooksCommand::Events,
            },
        )
        .unwrap();
        let (kind, detail) = app.mcp_detail_for_test().expect("hook events detail");
        assert_eq!(kind, TuiMcpDetailKind::Hooks);
        assert!(detail.contains("Hook Events"));
        assert!(detail.contains("user_prompt_submit"));
        assert!(detail.contains("permission_request"));
        assert!(detail.contains("shell_env"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn handle_tui_action_runs_and_polls_shell_job() {
        let _guard = shell_tool_lock();
        let store = temp_store("shell-action");
        let mut app = TuiApp::new(Vec::new());

        handle_tui_action(
            &store,
            None,
            &mut app,
            TuiAction::RunShell {
                command: "echo shell-start".to_string(),
            },
        )
        .unwrap();
        let output = render_once(&app, 160, 48).unwrap();
        assert!(output.contains("Shell Jobs"));
        assert!(output.contains("task_id"), "{output}");
        assert!(output.contains("echo shell-start"), "{output}");

        handle_tui_action(&store, None, &mut app, TuiAction::ListShell).unwrap();
        let output = render_once(&app, 160, 48).unwrap();
        assert!(output.contains("Shell jobs"), "{output}");
        assert!(output.contains("echo shell-start"), "{output}");

        let started = ExecShellTool
            .execute(
                ToolInput::new()
                    .with_arg("command", "echo shell-wait")
                    .with_arg("background", "true"),
            )
            .unwrap();
        let task_id = shell_task_id_from_summary(&started.summary)
            .expect("started shell task id")
            .to_string();

        handle_tui_action(
            &store,
            None,
            &mut app,
            TuiAction::WaitShell {
                task_id: task_id.clone(),
                wait: true,
                timeout_ms: 1_000,
            },
        )
        .unwrap();
        let output = render_once(&app, 160, 48).unwrap();
        assert!(output.contains("Shell Jobs"));
        assert!(output.contains("stdout_delta:"));
        assert!(output.contains("shell-wait"));

        handle_tui_action(
            &store,
            None,
            &mut app,
            TuiAction::ShowShell {
                task_id: task_id.clone(),
            },
        )
        .unwrap();
        let output = render_once(&app, 160, 48).unwrap();
        assert!(output.contains("stdout:"));
        assert!(output.contains("shell-wait"));

        let cat = ExecShellTool
            .execute(
                ToolInput::new()
                    .with_arg("command", "cat -")
                    .with_arg("background", "true"),
            )
            .unwrap();
        let cat_id = shell_task_id_from_summary(&cat.summary)
            .expect("cat shell task id")
            .to_string();
        handle_tui_action(
            &store,
            None,
            &mut app,
            TuiAction::SendShellStdin {
                task_id: cat_id,
                input: "shell-stdin\n".to_string(),
                close: true,
            },
        )
        .unwrap();
        let output = render_once(&app, 160, 48).unwrap();
        assert!(output.contains("shell stdin closed"));
        assert!(output.contains("shell-stdin"));

        let cancellable = ExecShellTool
            .execute(
                ToolInput::new()
                    .with_arg("command", "tail -f /dev/null")
                    .with_arg("background", "true"),
            )
            .unwrap();
        let cancel_id = shell_task_id_from_summary(&cancellable.summary)
            .expect("cancellable shell task id")
            .to_string();
        handle_tui_action(
            &store,
            None,
            &mut app,
            TuiAction::CancelShell {
                task_id: Some(cancel_id),
                all: false,
            },
        )
        .unwrap();
        assert!(render_once(&app, 160, 48)
            .unwrap()
            .contains("background shell job"));
    }

    #[test]
    fn handle_tui_action_shell_supervisor_shows_job_inventory() {
        let _guard = shell_tool_lock();
        let store = temp_store("shell-supervisor-inventory");
        let mut app = TuiApp::new(Vec::new());
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let task_id = format!("tui-supervisor-inventory-{suffix}");
        let job_dir = Path::new(".").join(".dscode/shell-jobs").join(&task_id);
        fs::create_dir_all(&job_dir).unwrap();
        fs::write(job_dir.join("stdout.log"), "supervisor-inventory\n").unwrap();
        let manifest = JsonValue::Object(BTreeMap::from([
            (
                "kind".to_string(),
                JsonValue::String("deepseek.exec_shell.job.v1".to_string()),
            ),
            ("id".to_string(), JsonValue::String(task_id.clone())),
            (
                "command".to_string(),
                JsonValue::String("echo supervisor-inventory".to_string()),
            ),
            ("cwd".to_string(), JsonValue::String(".".to_string())),
            ("tty".to_string(), JsonValue::Bool(false)),
            ("pty_backend".to_string(), JsonValue::Null),
            ("attachable".to_string(), JsonValue::Bool(false)),
            ("resizable".to_string(), JsonValue::Bool(false)),
            ("supervisor_pid".to_string(), JsonValue::Null),
            ("supervisor_socket".to_string(), JsonValue::Null),
            ("supervisor_epoch".to_string(), JsonValue::Null),
            ("terminal_event_log".to_string(), JsonValue::Null),
            ("terminal_event_seq".to_string(), JsonValue::Null),
            ("control_token_hash".to_string(), JsonValue::Null),
            ("tty_rows".to_string(), JsonValue::Null),
            ("tty_cols".to_string(), JsonValue::Null),
            (
                "status".to_string(),
                JsonValue::String("exited".to_string()),
            ),
            ("exit_code".to_string(), JsonValue::Number("0".to_string())),
            ("pid".to_string(), JsonValue::Number("0".to_string())),
            ("owner_pid".to_string(), JsonValue::Null),
            ("process_group".to_string(), JsonValue::Null),
            ("stdin_path".to_string(), JsonValue::Null),
            ("stdin_keeper_pid".to_string(), JsonValue::Null),
            ("stdin_closed".to_string(), JsonValue::Bool(true)),
            (
                "started_at".to_string(),
                JsonValue::String("epoch+1".to_string()),
            ),
            (
                "updated_at".to_string(),
                JsonValue::String("epoch+2".to_string()),
            ),
            (
                "stdout_total_bytes".to_string(),
                JsonValue::Number("21".to_string()),
            ),
            (
                "stderr_total_bytes".to_string(),
                JsonValue::Number("0".to_string()),
            ),
        ]));
        fs::write(
            job_dir.join("manifest.json"),
            json_value_to_string(&manifest),
        )
        .unwrap();

        handle_tui_action(&store, None, &mut app, TuiAction::ShellSupervisorStatus).unwrap();

        let (kind, detail) = app.mcp_detail_for_test().expect("shell detail");
        assert_eq!(kind, TuiMcpDetailKind::Shell);
        assert!(detail.contains("Shell supervisor status"), "{detail}");
        assert!(detail.contains("Shell job inventory"), "{detail}");
        assert!(detail.contains(&task_id), "{detail}");
        assert!(detail.contains("echo supervisor-inventory"), "{detail}");
        let _ = fs::remove_dir_all(job_dir);
    }

    #[test]
    fn handle_tui_action_runs_approved_shell_job() {
        let _guard = shell_tool_lock();
        let store = temp_store("approved-shell-action");
        let mut app = TuiApp::new(Vec::new());

        handle_tui_action(
            &store,
            None,
            &mut app,
            TuiAction::RunApprovedShell {
                command: "printf approved-shell".to_string(),
            },
        )
        .unwrap();

        let output = render_once(&app, 160, 48).unwrap();
        assert!(output.contains("Approved shell job started"), "{output}");
        assert!(output.contains("trusted_foreground_approval"), "{output}");
        assert!(output.contains("true"), "{output}");
        assert!(output.contains("printf approved-shell"), "{output}");
    }

    #[test]
    fn remote_diagnostics_status_summarizes_runtime_report() {
        let value = json_object([
            (
                "schema",
                JsonValue::String("deepseek.runtime.diagnostics.v1".to_string()),
            ),
            ("skipped", JsonValue::Bool(false)),
            (
                "report",
                json_object([
                    ("status", JsonValue::String("passed".to_string())),
                    (
                        "engine",
                        JsonValue::String("lsp publishDiagnostics".to_string()),
                    ),
                    (
                        "checked_files",
                        JsonValue::Array(vec![JsonValue::String("src/lib.rs".to_string())]),
                    ),
                    ("note", JsonValue::Null),
                ]),
            ),
        ]);

        assert_eq!(
            remote_diagnostics_status(&value),
            "remote diagnostics passed via lsp publishDiagnostics (1 checked files)"
        );
    }

    #[test]
    fn handle_tui_action_triggers_automation_into_pending_task() {
        let store = temp_store("automation-trigger-action");
        let session = store
            .create_session("Daily work".to_string(), ".".to_string())
            .unwrap();
        let thread = store
            .create_thread_for_session(
                &session.id,
                "Runtime automation".to_string(),
                ".".to_string(),
                "deepseek-coder".to_string(),
                "agent".to_string(),
            )
            .unwrap();
        let automation = store
            .create_automation(
                Some(&session.id),
                Some(&thread.id),
                "Nightly diagnostics".to_string(),
                "active".to_string(),
                "daily".to_string(),
                "run diagnostics".to_string(),
                None,
                Some("epoch+100".to_string()),
            )
            .unwrap();
        let mut app = app_from_store(&store).unwrap();

        handle_tui_action(
            &store,
            None,
            &mut app,
            TuiAction::TriggerAutomation {
                automation_id: automation.id.clone(),
                prompt_override: Some("manual run now".to_string()),
            },
        )
        .unwrap();

        let tasks = store
            .list_tasks(Some(&session.id), Some(&thread.id), 10)
            .unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].kind, "automation");
        assert_eq!(tasks[0].status, "pending");
        assert_eq!(tasks[0].summary, "manual run now");
        let events = store.read_events(&thread.id, 0).unwrap();
        assert!(events
            .iter()
            .any(|event| event.kind == "automation_triggered"));
        let output = render_once(&app, 160, 48).unwrap();
        assert!(output.contains("triggered automation"));
    }

    #[test]
    fn handle_tui_action_compacts_active_thread() {
        let store = temp_store("compact-action");
        let session = store
            .create_session("Daily work".to_string(), ".".to_string())
            .unwrap();
        let thread = store
            .create_thread_for_session(
                &session.id,
                "Runtime compaction".to_string(),
                ".".to_string(),
                "deepseek-coder".to_string(),
                "agent".to_string(),
            )
            .unwrap();
        for content in ["first", "second", "third"] {
            let turn = store
                .append_turn(&thread.id, "user".to_string(), content.to_string())
                .unwrap();
            store
                .append_item(
                    &thread.id,
                    Some(&turn.id),
                    "message".to_string(),
                    Some("user".to_string()),
                    content.to_string(),
                    "completed".to_string(),
                )
                .unwrap();
        }
        let mut app = app_from_store(&store).unwrap();

        handle_tui_action(
            &store,
            None,
            &mut app,
            TuiAction::CompactThread {
                thread_id: thread.id.clone(),
                keep_tail_turns: 1,
            },
        )
        .unwrap();

        let refreshed = app_from_store(&store).unwrap();
        let output = render_once(&refreshed, 160, 48).unwrap();
        assert!(output.contains("Compacted runtime thread summary"));
        let events = store.read_events(&thread.id, 0).unwrap();
        assert_eq!(events.last().unwrap().kind, "thread_compacted");
        let action_output = render_once(&app, 160, 48).unwrap();
        assert!(action_output.contains("compacted"));
        assert!(action_output.contains("summarized=2"));
    }

    #[test]
    fn record_tui_agent_result_appends_assistant_tools_usage_and_task() {
        let store = temp_store("agent-result");
        let session = store
            .create_session("Daily work".to_string(), ".".to_string())
            .unwrap();
        let thread = store
            .create_thread_for_session(
                &session.id,
                "Runtime agent".to_string(),
                ".".to_string(),
                "deepseek-coder".to_string(),
                "agent".to_string(),
            )
            .unwrap();
        let mut usage = crate::model::protocol::TokenUsage::new(3, 4);
        usage.model = Some("deepseek-v4-flash".to_string());
        let result = RunResult {
            final_message: "done from agent".to_string(),
            tool_events: vec![ToolEvent {
                tool_name: "run_shell".to_string(),
                input: BTreeMap::from([("command".to_string(), "pwd".to_string())]),
                output: "exit_code: 0".to_string(),
                status: crate::model::protocol::ObservationStatus::Ok,
            }],
            usage,
        };

        record_tui_agent_result(&store, &thread.id, "deepseek-coder", &result).unwrap();

        let items = store.list_items(&thread.id, None).unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].role.as_deref(), Some("assistant"));
        assert_eq!(items[0].content, "done from agent");
        assert_eq!(items[1].item_type, "tool_result");
        assert!(items[1].content.contains("tool: run_shell"));
        assert!(items[1].content.contains("command=pwd"));
        let usage = store.list_usage(Some(&thread.id), 10).unwrap();
        assert_eq!(usage.len(), 1);
        assert_eq!(usage[0].source, "tui");
        assert_eq!(usage[0].model, "deepseek-v4-flash");
        assert_eq!(usage[0].total_tokens, 7);
        let tasks = store.list_tasks(None, Some(&thread.id), 10).unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].status, "completed");
    }

    #[test]
    fn record_tui_agent_cancelled_surfaces_cancelled_item_and_task() {
        let store = temp_store("agent-cancelled");
        let session = store
            .create_session("Daily work".to_string(), ".".to_string())
            .unwrap();
        let thread = store
            .create_thread_for_session(
                &session.id,
                "Runtime agent".to_string(),
                ".".to_string(),
                "deepseek-coder".to_string(),
                "agent".to_string(),
            )
            .unwrap();
        let turn = store
            .append_turn(&thread.id, "assistant".to_string(), "running".to_string())
            .unwrap();
        let item = store
            .append_item(
                &thread.id,
                Some(&turn.id),
                "message".to_string(),
                Some("assistant".to_string()),
                "partial".to_string(),
                "running".to_string(),
            )
            .unwrap();
        let task = store
            .create_task(
                Some(&session.id),
                Some(&thread.id),
                None,
                "agent".to_string(),
                "running".to_string(),
                "agent run: test".to_string(),
            )
            .unwrap();

        record_tui_agent_cancelled_into(&store, &thread.id, &turn.id, &item.id, Some(&task.id))
            .unwrap();

        let item = store.load_item(&thread.id, &item.id).unwrap();
        assert_eq!(item.status, "cancelled");
        assert!(item.content.contains("partial"));
        assert!(item.content.contains("agent run cancelled"));
        assert_eq!(store.load_task(&task.id).unwrap().status, "cancelled");
    }

    #[test]
    fn runtime_item_stream_updates_running_assistant_item() {
        let store = temp_store("stream");
        let session = store
            .create_session("Daily work".to_string(), ".".to_string())
            .unwrap();
        let thread = store
            .create_thread_for_session(
                &session.id,
                "Runtime stream".to_string(),
                ".".to_string(),
                "deepseek-coder".to_string(),
                "agent".to_string(),
            )
            .unwrap();
        let turn = store
            .append_turn(
                &thread.id,
                "assistant".to_string(),
                "(assistant response running)".to_string(),
            )
            .unwrap();
        let item = store
            .append_item(
                &thread.id,
                Some(&turn.id),
                "message".to_string(),
                Some("assistant".to_string()),
                "".to_string(),
                "running".to_string(),
            )
            .unwrap();
        let mut stream = RuntimeItemStream::new(
            store.clone(),
            thread.id.clone(),
            turn.id.clone(),
            item.id.clone(),
            None,
        );

        stream.on_reasoning_delta("thinking");
        stream.on_text_delta("hello");
        stream.on_text_delta(" world");
        stream.on_assistant_done("hello world");

        let running = store.load_item(&thread.id, &item.id).unwrap();
        assert_eq!(running.content, "hello world");
        assert_eq!(running.status, "running");
        let items = store.list_items(&thread.id, Some(&turn.id)).unwrap();
        let reasoning = items
            .iter()
            .find(|item| item.item_type == "reasoning")
            .expect("reasoning item");
        assert_eq!(reasoning.content, "thinking");
        assert_eq!(reasoning.status, "completed");
        assert!(store
            .read_events(&thread.id, 0)
            .unwrap()
            .iter()
            .any(|event| event.kind == "item_updated"));
    }

    #[test]
    fn runtime_item_stream_emits_live_item_updates() {
        let store = temp_store("stream-live");
        let session = store
            .create_session("Daily work".to_string(), ".".to_string())
            .unwrap();
        let thread = store
            .create_thread_for_session(
                &session.id,
                "Runtime stream".to_string(),
                ".".to_string(),
                "deepseek-coder".to_string(),
                "agent".to_string(),
            )
            .unwrap();
        let turn = store
            .append_turn(
                &thread.id,
                "assistant".to_string(),
                "(assistant response running)".to_string(),
            )
            .unwrap();
        let item = store
            .append_item(
                &thread.id,
                Some(&turn.id),
                "message".to_string(),
                Some("assistant".to_string()),
                "".to_string(),
                "running".to_string(),
            )
            .unwrap();
        let (tx, rx) = mpsc::channel();
        let mut stream = RuntimeItemStream::new(
            store.clone(),
            thread.id.clone(),
            turn.id.clone(),
            item.id.clone(),
            Some(tx),
        );

        stream.on_text_delta("hello");
        stream.on_text_delta(" live");
        stream.on_reasoning_delta("thinking");

        let events = rx.try_iter().collect::<Vec<_>>();
        assert!(events.iter().any(|event| matches!(
            event,
            TuiLiveEvent::UpsertItem(item)
                if item.item_type == "message" && item.content == "hello live"
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            TuiLiveEvent::UpsertItem(item)
                if item.item_type == "reasoning" && item.content == "thinking"
        )));
    }

    #[test]
    fn runtime_tool_run_events_persist_and_emit_live_tool_updates() {
        let store = temp_store("tool-run-live");
        let session = store
            .create_session("Daily work".to_string(), ".".to_string())
            .unwrap();
        let thread = store
            .create_thread_for_session(
                &session.id,
                "Runtime tools".to_string(),
                ".".to_string(),
                "deepseek-coder".to_string(),
                "agent".to_string(),
            )
            .unwrap();
        let turn = store
            .append_turn(
                &thread.id,
                "assistant".to_string(),
                "(assistant response running)".to_string(),
            )
            .unwrap();
        let (tx, rx) = mpsc::channel();
        let persisted = Rc::new(RefCell::new(0_usize));
        let mut events = RuntimeToolRunEvents::new(
            store.clone(),
            thread.id.clone(),
            turn.id.clone(),
            Some(tx),
            Rc::clone(&persisted),
        );
        let input = BTreeMap::from([(
            "command".to_string(),
            "cd /tmp/repo && sleep 15 && gh pr checks 1611 --repo Hmbown/DeepSeek-TUI".to_string(),
        )]);

        events.on_tool_call("run_shell", &input);
        events.on_permission_request("run_shell", &input, "shell", "gh pr checks 1611");
        events.on_tool_result(&ToolEvent {
            tool_name: "run_shell".to_string(),
            input: input.clone(),
            output: "2 checks pending".to_string(),
            status: crate::model::protocol::ObservationStatus::Ok,
        });

        assert_eq!(*persisted.borrow(), 1);
        let items = store.list_items(&thread.id, Some(&turn.id)).unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].item_type, "tool_call");
        assert_eq!(items[0].status, "completed");
        assert!(items[0].content.contains("target: gh pr checks 1611"));
        assert!(!items[0].content.contains("target: cd /tmp"));
        assert_eq!(items[1].item_type, "tool_result");
        assert_eq!(items[1].status, "completed");
        assert!(items[1].content.contains("tool: run_shell"));
        assert!(items[1].content.contains("2 checks pending"));

        let live_items = rx
            .try_iter()
            .filter_map(|event| match event {
                TuiLiveEvent::UpsertItem(item) => Some(item),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert!(live_items.iter().any(|item| {
            item.item_type == "tool_call"
                && item.status == "running"
                && item.content.contains("gh pr checks 1611")
        }));
        assert!(live_items.iter().any(|item| {
            item.item_type == "tool_call"
                && item.status == "pending"
                && item.content.contains("approval: shell gh pr checks 1611")
        }));
        assert!(live_items
            .iter()
            .any(|item| item.item_type == "tool_result" && item.status == "completed"));
    }

    #[test]
    fn record_tui_agent_result_skips_live_persisted_tool_results() {
        let store = temp_store("agent-result-live-skip");
        let session = store
            .create_session("Daily work".to_string(), ".".to_string())
            .unwrap();
        let thread = store
            .create_thread_for_session(
                &session.id,
                "Runtime agent".to_string(),
                ".".to_string(),
                "deepseek-coder".to_string(),
                "agent".to_string(),
            )
            .unwrap();
        let assistant = store
            .append_turn(&thread.id, "assistant".to_string(), "running".to_string())
            .unwrap();
        let assistant_item = store
            .append_item(
                &thread.id,
                Some(&assistant.id),
                "message".to_string(),
                Some("assistant".to_string()),
                "".to_string(),
                "running".to_string(),
            )
            .unwrap();
        let mut usage = crate::model::protocol::TokenUsage::new(3, 4);
        usage.model = Some("deepseek-v4-flash".to_string());
        let result = RunResult {
            final_message: "done from agent".to_string(),
            tool_events: vec![
                ToolEvent {
                    tool_name: "run_shell".to_string(),
                    input: BTreeMap::from([("command".to_string(), "pwd".to_string())]),
                    output: "exit_code: 0".to_string(),
                    status: crate::model::protocol::ObservationStatus::Ok,
                },
                ToolEvent {
                    tool_name: "posthoc_translate".to_string(),
                    input: BTreeMap::from([("target_language".to_string(), "Chinese".to_string())]),
                    output: "translated".to_string(),
                    status: crate::model::protocol::ObservationStatus::Ok,
                },
            ],
            usage,
        };

        record_tui_agent_result_into(
            &store,
            &thread.id,
            &assistant.id,
            &assistant_item.id,
            None,
            "deepseek-coder",
            &result,
            1,
        )
        .unwrap();

        let items = store.list_items(&thread.id, Some(&assistant.id)).unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].item_type, "message");
        assert_eq!(items[1].item_type, "tool_result");
        assert!(!items[1].content.contains("tool: run_shell"));
        assert!(items[1].content.contains("tool: posthoc_translate"));
    }

    #[test]
    fn record_tui_agent_failure_surfaces_failed_assistant_item_and_task() {
        let store = temp_store("agent-failure");
        let session = store
            .create_session("Daily work".to_string(), ".".to_string())
            .unwrap();
        let thread = store
            .create_thread_for_session(
                &session.id,
                "Runtime agent".to_string(),
                ".".to_string(),
                "deepseek-coder".to_string(),
                "agent".to_string(),
            )
            .unwrap();

        record_tui_agent_failure(&store, &thread.id, "missing api key").unwrap();

        let items = store.list_items(&thread.id, None).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].status, "failed");
        assert!(items[0]
            .content
            .contains("agent run failed: missing api key"));
        let tasks = store.list_tasks(None, Some(&thread.id), 10).unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].status, "failed");
    }

    #[test]
    fn approval_response_decision_matches_request_id() {
        let store = temp_store("approval-decision");
        let session = store
            .create_session("Daily work".to_string(), ".".to_string())
            .unwrap();
        let thread = store
            .create_thread_for_session(
                &session.id,
                "Runtime permissions".to_string(),
                ".".to_string(),
                "deepseek-coder".to_string(),
                "agent".to_string(),
            )
            .unwrap();
        let request = store
            .append_permission_request(
                &thread.id,
                None,
                "run_shell".to_string(),
                "shell".to_string(),
                "pwd".to_string(),
                BTreeMap::new(),
            )
            .unwrap();
        store
            .append_permission_response(
                &thread.id,
                None,
                request.id.clone(),
                "approved".to_string(),
            )
            .unwrap();

        let events = store.read_events(&thread.id, request.seq).unwrap();
        assert_eq!(
            events
                .iter()
                .find_map(|event| approval_response_decision(event, &request.id)),
            Some(AgentApprovalDecision::Approved)
        );
        assert_eq!(
            events
                .iter()
                .find_map(|event| approval_response_decision(event, "event-other")),
            None
        );
    }

    #[test]
    fn cached_runtime_permission_decision_reuses_session_grouping() {
        let store = temp_store("approval-session-group");
        let session = store
            .create_session("Daily work".to_string(), ".".to_string())
            .unwrap();
        let thread = store
            .create_thread_for_session(
                &session.id,
                "Runtime permissions".to_string(),
                ".".to_string(),
                "deepseek-coder".to_string(),
                "agent".to_string(),
            )
            .unwrap();

        let first = store
            .append_permission_request(
                &thread.id,
                None,
                "run_shell".to_string(),
                "shell".to_string(),
                "cargo build".to_string(),
                BTreeMap::from([("command".to_string(), "cargo build".to_string())]),
            )
            .unwrap();
        store
            .append_permission_response_with_scope(
                &thread.id,
                None,
                first.id,
                "approved".to_string(),
                Some("session".to_string()),
            )
            .unwrap();
        let second = store
            .append_permission_request(
                &thread.id,
                None,
                "run_shell".to_string(),
                "shell".to_string(),
                "cargo build --release".to_string(),
                BTreeMap::from([("command".to_string(), "cargo build --release".to_string())]),
            )
            .unwrap();

        assert_eq!(
            cached_runtime_permission_decision(&store, &thread.id, &second).unwrap(),
            Some(AgentApprovalDecision::Approved)
        );
    }

    #[test]
    fn cached_runtime_permission_decision_keeps_denials_exact() {
        let store = temp_store("approval-denial-exact");
        let session = store
            .create_session("Daily work".to_string(), ".".to_string())
            .unwrap();
        let thread = store
            .create_thread_for_session(
                &session.id,
                "Runtime permissions".to_string(),
                ".".to_string(),
                "deepseek-coder".to_string(),
                "agent".to_string(),
            )
            .unwrap();

        let first = store
            .append_permission_request(
                &thread.id,
                None,
                "run_shell".to_string(),
                "shell".to_string(),
                "cargo build".to_string(),
                BTreeMap::from([("command".to_string(), "cargo build".to_string())]),
            )
            .unwrap();
        store
            .append_permission_response(&thread.id, None, first.id, "denied".to_string())
            .unwrap();
        let variant = store
            .append_permission_request(
                &thread.id,
                None,
                "run_shell".to_string(),
                "shell".to_string(),
                "cargo build --release".to_string(),
                BTreeMap::from([("command".to_string(), "cargo build --release".to_string())]),
            )
            .unwrap();
        assert_eq!(
            cached_runtime_permission_decision(&store, &thread.id, &variant).unwrap(),
            None
        );

        let repeat = store
            .append_permission_request(
                &thread.id,
                None,
                "run_shell".to_string(),
                "shell".to_string(),
                "cargo build".to_string(),
                BTreeMap::from([("command".to_string(), "cargo build".to_string())]),
            )
            .unwrap();
        assert_eq!(
            cached_runtime_permission_decision(&store, &thread.id, &repeat).unwrap(),
            Some(AgentApprovalDecision::Denied)
        );
    }

    #[test]
    fn runtime_user_input_resolver_waits_for_response_event() {
        let store = temp_store("user-input-resolver");
        let session = store
            .create_session("Daily work".to_string(), ".".to_string())
            .unwrap();
        let runtime_thread = store
            .create_thread_for_session(
                &session.id,
                "Runtime user input".to_string(),
                ".".to_string(),
                "deepseek-coder".to_string(),
                "agent".to_string(),
            )
            .unwrap();
        let turn = store
            .append_turn(
                &runtime_thread.id,
                "assistant".to_string(),
                "(assistant response running)".to_string(),
            )
            .unwrap();
        let responder_store = store.clone();
        let responder_thread_id = runtime_thread.id.clone();
        let responder_turn_id = turn.id.clone();
        let responder = thread::spawn(move || {
            for _ in 0..500 {
                let events = responder_store
                    .read_events(&responder_thread_id, 0)
                    .unwrap();
                if let Some(request) = events
                    .iter()
                    .find(|event| event.kind == "user_input_request")
                {
                    responder_store
                        .append_user_input_response(
                            &responder_thread_id,
                            Some(&responder_turn_id),
                            request.id.clone(),
                            BTreeMap::from([("mode".to_string(), "Plan".to_string())]),
                        )
                        .unwrap();
                    return;
                }
                thread::sleep(Duration::from_millis(1));
            }
            panic!("timed out waiting for user_input_request");
        });
        let mut resolver = RuntimeUserInputResolver {
            store: store.clone(),
            thread_id: runtime_thread.id.clone(),
            turn_id: Some(turn.id.clone()),
            cancel_since_seq: Some(0),
            poll_interval: Duration::from_millis(1),
            max_polls: Some(500),
        };
        let questions = r#"[{"header":"Mode","id":"mode","question":"Which mode?","options":[{"label":"Plan","description":"Plan first."},{"label":"Apply","description":"Implement directly."}]}]"#;
        let response = resolver
            .resolve(&AgentUserInputRequest {
                input: BTreeMap::from([("questions".to_string(), questions.to_string())]),
            })
            .unwrap();
        responder.join().unwrap();

        assert_eq!(
            response.answers.get("mode").map(String::as_str),
            Some("Plan")
        );
        let events = store.read_events(&runtime_thread.id, 0).unwrap();
        assert!(events.iter().any(|event| event.kind == "user_input_request"
            && event.turn_id.as_deref() == Some(turn.id.as_str())));
        assert!(events
            .iter()
            .any(|event| event.kind == "user_input_response"
                && event.turn_id.as_deref() == Some(turn.id.as_str())));
    }
}
