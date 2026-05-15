use std::cell::RefCell;
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::rc::Rc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::cli::app::{
    AgentsAction, AgentsRlmCancelArgs, AgentsRlmDrainArgs, AgentsRlmEventsArgs,
    AgentsRlmRecoverArgs, AgentsRlmRunNextArgs, AgentsRlmStatusArgs, AgentsRlmStopArgs,
    AgentsRlmWaitArgs, AgentsServiceArgs, AgentsServiceDoctorArgs, AgentsServiceKind,
    AgentsServiceSmokeArgs, AgentsShellAction, AgentsShellArgs, AgentsShellSupervisorArgs,
};
use crate::config::load::load_or_default;
use crate::config::types::AppConfig;
use crate::core::agents::{load_agent_file, load_default_agents, AgentLoadResult, AgentSource};
use crate::core::context::TaskContext;
use crate::core::loop_runtime::{
    AgentApprovalDecision, AgentApprovalRequest, AgentApprovalResolver, AgentLoop,
    AgentLoopOptions, AgentUserInputRequest, AgentUserInputResolver, AgentUserInputResponse,
    RunResult, SharedAgentApprovalResolver, SharedAgentUserInputResolver, ToolEvent,
};
use crate::core::rollback::RollbackStore;
use crate::core::runtime::{
    AutomationRecord, RuntimeStore, TaskRecord, ThreadCompactionRecord, ThreadRecord, TurnRecord,
};
use crate::error::{app_error, AppResult};
use crate::model::client::ModelClient;
use crate::model::deepseek::DeepSeekClient;
use crate::model::protocol::{ModelAction, ModelRequest, ObservationStatus};
use crate::tools::dispatch_subagent::{
    active_agent_thread_path, agent_threads_dir, thread_file_path, validate_thread_id,
};
use crate::tools::exec_shell::{
    count_active_durable_shell_jobs, native_supervisor_pty_supported, ExecShellAttachTool,
    ExecShellCancelTool, ExecShellInteractTool, ExecShellListTool, ExecShellReplayTool,
    ExecShellResizeTool, ExecShellSupervisorStatusTool, ExecShellWaitTool, TaskShellStartTool,
    SHELL_SUPERVISOR_SUPPORTED_METHODS, SHELL_SUPERVISOR_UNSUPPORTED_PTY_METHODS,
};
use crate::tools::rlm::{
    rlm_live_session_ids_by_runtime_thread, RlmLiveCancelTool, RlmLiveDrainTool, RlmLiveEventsTool,
    RlmLiveRecoverTool, RlmLiveRunNextTool, RlmLiveStatusTool, RlmLiveStopTool, RlmLiveWaitTool,
};
use crate::tools::types::{Tool, ToolInput};
use crate::util::json::{
    json_as_array, json_as_object, json_as_string, json_as_u64, parse_json_value,
};
use crate::util::json::{json_escape, json_value_to_string, JsonValue};

pub fn run(action: AgentsAction) -> AppResult<()> {
    let config = load_or_default()?;
    match action {
        AgentsAction::List => list_agents(&config.workspace.config_dir),
        AgentsAction::Show { name } => show_agent(&config.workspace.config_dir, &name),
        AgentsAction::Validate { path } => validate_agents(&config.workspace.config_dir, path),
        AgentsAction::RunTask { id, budget, json } => run_runtime_task(config, &id, budget, json),
        AgentsAction::Daemon {
            budget,
            interval_ms,
            once,
            json,
        } => run_runtime_daemon(config, budget, interval_ms, once, json),
        AgentsAction::RlmStatus(args) => run_rlm_status(config, args),
        AgentsAction::RlmEvents(args) => run_rlm_events(config, args),
        AgentsAction::RlmWait(args) => run_rlm_wait(config, args),
        AgentsAction::RlmCancel(args) => run_rlm_cancel(config, args),
        AgentsAction::RlmRecover(args) => run_rlm_recover(config, args),
        AgentsAction::RlmStop(args) => run_rlm_stop(config, args),
        AgentsAction::RlmRunNext(args) => run_rlm_run_next(config, args),
        AgentsAction::RlmDrain(args) => run_rlm_drain(config, args),
        AgentsAction::Shell(args) => run_shell_control(args),
        AgentsAction::ShellSupervisor(args) => run_shell_supervisor(args),
        AgentsAction::Service(args) => render_agent_services(args),
        AgentsAction::ServiceDoctor(args) => run_service_doctor(args),
        AgentsAction::ServiceSmoke(args) => run_service_smoke(args),
        AgentsAction::Threads => list_threads(&config.workspace.config_dir),
        AgentsAction::ShowThread { id } => show_thread(&config.workspace.config_dir, &id),
        AgentsAction::SwitchThread { id } => switch_thread(&config.workspace.config_dir, &id),
        AgentsAction::CurrentThread => current_thread(&config.workspace.config_dir),
        AgentsAction::ClearThread => clear_thread(&config.workspace.config_dir),
    }
}

fn run_shell_control(args: AgentsShellArgs) -> AppResult<()> {
    let cwd = std::env::current_dir()?;
    let request = agents_shell_request_json(&args);
    let response = send_shell_supervisor_cli_request(&cwd, &request)?;
    print_shell_control_response(&args, response)
}

fn agents_shell_request_json(args: &AgentsShellArgs) -> JsonValue {
    let mut object = BTreeMap::new();
    match &args.action {
        AgentsShellAction::Status => {
            object.insert(
                "method".to_string(),
                JsonValue::String("status".to_string()),
            );
        }
        AgentsShellAction::Show => {
            object.insert("method".to_string(), JsonValue::String("show".to_string()));
        }
        AgentsShellAction::Start {
            command,
            cwd,
            tty,
            tty_rows,
            tty_cols,
        } => {
            object.insert("method".to_string(), JsonValue::String("start".to_string()));
            object.insert("command".to_string(), JsonValue::String(command.clone()));
            if let Some(cwd) = cwd {
                object.insert("cwd".to_string(), JsonValue::String(cwd.clone()));
            }
            if *tty {
                object.insert("tty".to_string(), JsonValue::Bool(true));
            }
            insert_optional_number(&mut object, "tty_rows", *tty_rows);
            insert_optional_number(&mut object, "tty_cols", *tty_cols);
        }
        AgentsShellAction::Wait {
            task_id,
            timeout_ms,
        } => {
            object.insert("method".to_string(), JsonValue::String("wait".to_string()));
            object.insert("task_id".to_string(), JsonValue::String(task_id.clone()));
            insert_optional_number(&mut object, "timeout_ms", *timeout_ms);
        }
        AgentsShellAction::Replay {
            task_id,
            stream,
            cursor,
            offset,
            limit_bytes,
            tail,
        } => {
            object.insert(
                "method".to_string(),
                JsonValue::String("replay".to_string()),
            );
            object.insert("task_id".to_string(), JsonValue::String(task_id.clone()));
            if let Some(stream) = stream {
                object.insert("stream".to_string(), JsonValue::String(stream.clone()));
            }
            insert_optional_number(&mut object, "cursor", *cursor);
            insert_optional_number(&mut object, "offset", *offset);
            insert_optional_number(&mut object, "limit_bytes", *limit_bytes);
            if *tail {
                object.insert("tail".to_string(), JsonValue::Bool(true));
            }
        }
        AgentsShellAction::Attach {
            task_id,
            cursor,
            wait_ms,
            limit_bytes,
            tail,
        } => {
            object.insert(
                "method".to_string(),
                JsonValue::String("attach".to_string()),
            );
            object.insert("task_id".to_string(), JsonValue::String(task_id.clone()));
            insert_optional_number(&mut object, "cursor", *cursor);
            insert_optional_number(&mut object, "wait_ms", *wait_ms);
            insert_optional_number(&mut object, "limit_bytes", *limit_bytes);
            if *tail {
                object.insert("tail".to_string(), JsonValue::Bool(true));
            }
        }
        AgentsShellAction::Stdin {
            task_id,
            input,
            close_stdin,
            timeout_ms,
        } => {
            object.insert("method".to_string(), JsonValue::String("stdin".to_string()));
            object.insert("task_id".to_string(), JsonValue::String(task_id.clone()));
            if let Some(input) = input {
                object.insert("input".to_string(), JsonValue::String(input.clone()));
            }
            if *close_stdin {
                object.insert("close_stdin".to_string(), JsonValue::Bool(true));
            }
            insert_optional_number(&mut object, "timeout_ms", *timeout_ms);
        }
        AgentsShellAction::Resize {
            task_id,
            tty_rows,
            tty_cols,
        } => {
            object.insert(
                "method".to_string(),
                JsonValue::String("resize".to_string()),
            );
            object.insert("task_id".to_string(), JsonValue::String(task_id.clone()));
            object.insert(
                "tty_rows".to_string(),
                JsonValue::Number(tty_rows.to_string()),
            );
            object.insert(
                "tty_cols".to_string(),
                JsonValue::Number(tty_cols.to_string()),
            );
        }
        AgentsShellAction::Cancel { task_id, all } => {
            object.insert(
                "method".to_string(),
                JsonValue::String("cancel".to_string()),
            );
            if *all {
                object.insert("all".to_string(), JsonValue::Bool(true));
            }
            if let Some(task_id) = task_id {
                object.insert("task_id".to_string(), JsonValue::String(task_id.clone()));
            }
        }
        AgentsShellAction::Shutdown => {
            object.insert(
                "method".to_string(),
                JsonValue::String("shutdown".to_string()),
            );
        }
    }
    JsonValue::Object(object)
}

fn insert_optional_number(object: &mut BTreeMap<String, JsonValue>, key: &str, value: Option<u64>) {
    if let Some(value) = value {
        object.insert(key.to_string(), JsonValue::Number(value.to_string()));
    }
}

#[cfg(unix)]
fn send_shell_supervisor_cli_request(cwd: &Path, request: &JsonValue) -> AppResult<JsonValue> {
    use std::os::unix::net::UnixStream;

    let socket = cwd.join(".dscode/shell-supervisor/supervisor.sock");
    let mut stream = UnixStream::connect(&socket).map_err(|error| {
        app_error(format!(
            "shell supervisor socket is not active at {}: {error}. Start it with `deepseek agents shell-supervisor --json`.",
            socket.display()
        ))
    })?;
    stream.write_all(json_value_to_string(request).as_bytes())?;
    stream.write_all(b"\n")?;
    stream.flush()?;
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line)?;
    parse_json_value(line.trim()).map_err(|error| {
        app_error(format!(
            "shell supervisor returned invalid response JSON: {error}"
        ))
    })
}

#[cfg(not(unix))]
fn send_shell_supervisor_cli_request(_cwd: &Path, _request: &JsonValue) -> AppResult<JsonValue> {
    Err(app_error(
        "agents shell control currently requires the Unix shell supervisor socket",
    ))
}

fn print_shell_control_response(args: &AgentsShellArgs, response: JsonValue) -> AppResult<()> {
    if args.json {
        println!("{}", json_value_to_string(&response));
        return Ok(());
    }
    let Some(object) = json_as_object(&response) else {
        return Err(app_error("shell supervisor response must be a JSON object"));
    };
    let status = object
        .get("status")
        .and_then(json_as_string)
        .unwrap_or("unknown");
    if status == "error" || status == "unsupported" {
        let error = object
            .get("error")
            .and_then(json_as_string)
            .unwrap_or("shell supervisor request failed");
        return Err(app_error(error.to_string()));
    }
    let summary_key = match args.action {
        AgentsShellAction::Show => Some("job_inventory"),
        AgentsShellAction::Start { .. } => Some("start_summary"),
        AgentsShellAction::Wait { .. } => Some("wait_summary"),
        AgentsShellAction::Replay { .. } => Some("replay_summary"),
        AgentsShellAction::Attach { .. } => Some("attach_summary"),
        AgentsShellAction::Stdin { .. } => Some("stdin_summary"),
        AgentsShellAction::Resize { .. } => Some("resize_summary"),
        AgentsShellAction::Cancel { .. } => Some("cancel_summary"),
        AgentsShellAction::Status | AgentsShellAction::Shutdown => None,
    };
    if let Some(key) = summary_key {
        if let Some(summary) = object.get(key).and_then(json_as_string) {
            println!("{summary}");
            return Ok(());
        }
    }
    print_shell_control_status(object);
    Ok(())
}

fn print_shell_control_status(object: &BTreeMap<String, JsonValue>) {
    let method = object
        .get("method")
        .and_then(json_as_string)
        .unwrap_or("status");
    let status = object
        .get("status")
        .and_then(json_as_string)
        .unwrap_or("unknown");
    println!("shell supervisor {method}: {status}");
    for key in [
        "cwd",
        "supervisor_pid",
        "supervisor_socket",
        "supervisor_epoch",
        "protocol",
        "native_pty",
        "active_jobs",
    ] {
        if let Some(value) = object.get(key) {
            println!("{key}: {}", shell_control_scalar(value));
        }
    }
}

fn shell_control_scalar(value: &JsonValue) -> String {
    match value {
        JsonValue::String(value) | JsonValue::Number(value) => value.clone(),
        JsonValue::Bool(value) => value.to_string(),
        JsonValue::Null => "null".to_string(),
        JsonValue::Array(_) | JsonValue::Object(_) => json_value_to_string(value),
    }
}

fn list_agents(config_dir: &str) -> AppResult<()> {
    let results = load_default_agents(config_dir);
    if results.is_empty() {
        println!("No subagents configured.");
        println!("Add project agents under .dscode/agents/*.md");
        println!("Add user agents under ~/.config/dscode/agents/*.md");
        return Ok(());
    }

    println!("Subagents:");
    for result in results {
        match result {
            Ok(agent) => println!(
                "- {} {}: {} ({})",
                agent.source.label(),
                agent.name,
                agent.description,
                agent.path.display()
            ),
            Err(error) => println!("- error {}: {}", error.path.display(), error.message),
        }
    }
    Ok(())
}

fn show_agent(config_dir: &str, name: &str) -> AppResult<()> {
    let agent = crate::core::agents::find_agent(config_dir, name)
        .map_err(|error| app_error(format!("{}: {}", error.path.display(), error.message)))?;

    println!("Name: {}", agent.name);
    println!("Source: {}", agent.source.label());
    println!("Path: {}", agent.path.display());
    println!("Description: {}", agent.description);
    println!(
        "Tools: {}",
        if agent.tools.is_empty() {
            "all".to_string()
        } else {
            agent.tools.join(", ")
        }
    );
    println!(
        "Model: {}",
        agent.model.as_deref().unwrap_or("default configured model")
    );
    println!();
    println!("{}", agent.prompt);
    Ok(())
}

fn validate_agents(config_dir: &str, path: Option<String>) -> AppResult<()> {
    let results = if let Some(path) = path {
        vec![load_agent_file(Path::new(&path), AgentSource::File)]
    } else {
        load_default_agents(config_dir)
    };

    if results.is_empty() {
        println!("No agent files found.");
        return Ok(());
    }

    let mut failed = 0usize;
    for result in &results {
        match result {
            Ok(agent) => println!("OK {} name={}", agent.path.display(), agent.name),
            Err(error) => {
                failed += 1;
                println!("ERR {} {}", error.path.display(), error.message);
            }
        }
    }

    if failed > 0 {
        return Err(app_error(format!(
            "agent validation failed for {failed} file{}",
            if failed == 1 { "" } else { "s" }
        )));
    }
    Ok(())
}

fn run_runtime_task(
    config: AppConfig,
    task_id: &str,
    budget: Option<usize>,
    json: bool,
) -> AppResult<()> {
    let store = RuntimeStore::new(PathBuf::from(&config.workspace.config_dir).join("runtime"));
    let rollback_store =
        RollbackStore::new(PathBuf::from(&config.workspace.config_dir).join("rollback"));
    let task = store.load_task(task_id)?;
    let thread_id = task
        .thread_id
        .clone()
        .ok_or_else(|| app_error("agents run-task requires a task linked to a runtime thread"))?;
    let thread = store.load_thread(&thread_id)?;
    let runner_id = format!("local-runner-{}", std::process::id());
    let task = store.claim_task(task_id, runner_id.clone())?;
    if json {
        println!(
            "{}",
            json_value_to_string(&runner_event(
                "task_claimed",
                &task.id,
                &thread.id,
                Some(&runner_id),
                None,
            ))
        );
    } else {
        println!("claimed runtime task: {}", task.id);
        println!("thread: {}", thread.id);
    }

    let workspace = PathBuf::from(&thread.workspace);
    let rollback_snapshot_id = rollback_store
        .create_snapshot(&workspace, format!("runtime task rollback: {}", task.id))
        .ok()
        .map(|snapshot| snapshot.id);
    let cwd_guard = match crate::util::cwd::CwdGuard::enter(&workspace) {
        Ok(guard) => guard,
        Err(error) => {
            record_runtime_task_failure(&store, &task, &thread, &error.to_string())?;
            if json {
                println!(
                    "{}",
                    json_value_to_string(&runner_event(
                        "task_failed",
                        &task.id,
                        &thread.id,
                        None,
                        Some(&error.to_string()),
                    ))
                );
            }
            return Err(error);
        }
    };
    let run_result = run_runtime_task_loop(&config, &store, &task, &thread, budget, json);
    cwd_guard.restore()?;

    match run_result {
        Ok(result) => {
            let assistant_turn_id = record_runtime_task_result(&store, &task, &thread, &result)?;
            if let Some(snapshot_id) = rollback_snapshot_id {
                let _ = rollback_store.bind_snapshot_runtime(
                    &snapshot_id,
                    Some(&thread.id),
                    Some(&assistant_turn_id),
                );
            }
            if json {
                println!(
                    "{}",
                    json_value_to_string(&runner_event(
                        "task_completed",
                        &task.id,
                        &thread.id,
                        None,
                        Some(&result.final_message),
                    ))
                );
            }
            Ok(())
        }
        Err(error) => {
            record_runtime_task_failure(&store, &task, &thread, &error.to_string())?;
            if json {
                println!(
                    "{}",
                    json_value_to_string(&runner_event(
                        "task_failed",
                        &task.id,
                        &thread.id,
                        None,
                        Some(&error.to_string()),
                    ))
                );
            }
            Err(error)
        }
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct RuntimeDaemonTick {
    triggered_automations: usize,
    executed_tasks: usize,
    executed_rlm_turns: usize,
    recovered_rlm_turns: usize,
    compacted_threads: usize,
    failed_automations: usize,
    failed_tasks: usize,
    failed_rlm_turns: usize,
    failed_rlm_recoveries: usize,
    failed_compactions: usize,
}

const DAEMON_COMPACTION_THRESHOLD_TOKENS: u64 = 800_000;
const DAEMON_COMPACTION_KEEP_TAIL_TURNS: usize = 8;

fn run_runtime_daemon(
    config: AppConfig,
    budget: Option<usize>,
    interval_ms: u64,
    once: bool,
    json: bool,
) -> AppResult<()> {
    let runtime_root = PathBuf::from(&config.workspace.config_dir).join("runtime");
    let store = RuntimeStore::new(runtime_root.clone());
    let interval = Duration::from_millis(interval_ms.max(100));
    if !json {
        println!("runtime daemon watching {}", runtime_root.display());
        println!("poll interval: {}ms", interval.as_millis());
    }

    loop {
        let tick = run_runtime_daemon_tick(&config, &store, budget, json)?;
        if json {
            println!("{}", json_value_to_string(&daemon_tick_event(&tick)));
        } else if tick.triggered_automations > 0
            || tick.executed_tasks > 0
            || tick.executed_rlm_turns > 0
            || tick.recovered_rlm_turns > 0
            || tick.compacted_threads > 0
            || tick.failed_automations > 0
            || tick.failed_tasks > 0
            || tick.failed_rlm_turns > 0
            || tick.failed_rlm_recoveries > 0
            || tick.failed_compactions > 0
        {
            println!(
                "daemon tick: triggered={} executed={} rlm_executed={} rlm_recovered={} compacted={} automation_errors={} task_errors={} rlm_errors={} rlm_recovery_errors={} compaction_errors={}",
                tick.triggered_automations,
                tick.executed_tasks,
                tick.executed_rlm_turns,
                tick.recovered_rlm_turns,
                tick.compacted_threads,
                tick.failed_automations,
                tick.failed_tasks,
                tick.failed_rlm_turns,
                tick.failed_rlm_recoveries,
                tick.failed_compactions
            );
        }

        if once {
            return Ok(());
        }
        std::thread::sleep(interval);
    }
}

fn run_rlm_status(config: AppConfig, args: AgentsRlmStatusArgs) -> AppResult<()> {
    let output = RlmLiveStatusTool { config }.execute(rlm_status_tool_input(&args))?;
    print_rlm_cli_output(&output.summary, args.json);
    Ok(())
}

fn run_rlm_events(config: AppConfig, args: AgentsRlmEventsArgs) -> AppResult<()> {
    let output = RlmLiveEventsTool { config }.execute(rlm_events_tool_input(&args))?;
    print_rlm_cli_output(&output.summary, args.json);
    Ok(())
}

fn run_rlm_wait(config: AppConfig, args: AgentsRlmWaitArgs) -> AppResult<()> {
    let output = RlmLiveWaitTool { config }.execute(rlm_wait_tool_input(&args))?;
    print_rlm_cli_output(&output.summary, args.json);
    Ok(())
}

fn run_rlm_cancel(config: AppConfig, args: AgentsRlmCancelArgs) -> AppResult<()> {
    let output = RlmLiveCancelTool { config }.execute(rlm_cancel_tool_input(&args))?;
    print_rlm_cli_output(&output.summary, args.json);
    Ok(())
}

fn run_rlm_recover(config: AppConfig, args: AgentsRlmRecoverArgs) -> AppResult<()> {
    let output = RlmLiveRecoverTool { config }.execute(rlm_recover_tool_input(&args))?;
    print_rlm_cli_output(&output.summary, args.json);
    Ok(())
}

fn run_rlm_stop(config: AppConfig, args: AgentsRlmStopArgs) -> AppResult<()> {
    let output = RlmLiveStopTool { config }.execute(rlm_stop_tool_input(&args))?;
    print_rlm_cli_output(&output.summary, args.json);
    Ok(())
}

fn run_rlm_run_next(config: AppConfig, args: AgentsRlmRunNextArgs) -> AppResult<()> {
    let output = RlmLiveRunNextTool {
        config,
        parent_depth: 0,
    }
    .execute(rlm_run_next_tool_input(&args))?;
    print_rlm_cli_output(&output.summary, args.json);
    Ok(())
}

fn run_rlm_drain(config: AppConfig, args: AgentsRlmDrainArgs) -> AppResult<()> {
    let output = RlmLiveDrainTool {
        config,
        parent_depth: 0,
    }
    .execute(rlm_drain_tool_input(&args))?;
    print_rlm_cli_output(&output.summary, args.json);
    Ok(())
}

fn rlm_status_tool_input(args: &AgentsRlmStatusArgs) -> ToolInput {
    let mut input = ToolInput::new();
    if let Some(session_id) = &args.session_id {
        input = input.with_arg("session_id", session_id.clone());
    }
    if let Some(limit) = args.limit {
        input = input.with_arg("limit", limit.to_string());
    }
    input
}

fn rlm_events_tool_input(args: &AgentsRlmEventsArgs) -> ToolInput {
    let mut input = ToolInput::new().with_arg("session_id", args.session_id.clone());
    if let Some(cursor) = args.cursor {
        input = input.with_arg("cursor", cursor.to_string());
    }
    if let Some(limit) = args.limit {
        input = input.with_arg("limit", limit.to_string());
    }
    input
}

fn rlm_wait_tool_input(args: &AgentsRlmWaitArgs) -> ToolInput {
    let mut input = ToolInput::new().with_arg("session_id", args.session_id.clone());
    if let Some(cursor) = args.cursor {
        input = input.with_arg("cursor", cursor.to_string());
    }
    if let Some(limit) = args.limit {
        input = input.with_arg("limit", limit.to_string());
    }
    if let Some(timeout_ms) = args.timeout_ms {
        input = input.with_arg("timeout_ms", timeout_ms.to_string());
    }
    if let Some(poll_interval_ms) = args.poll_interval_ms {
        input = input.with_arg("poll_interval_ms", poll_interval_ms.to_string());
    }
    input
}

fn rlm_cancel_tool_input(args: &AgentsRlmCancelArgs) -> ToolInput {
    let mut input = ToolInput::new().with_arg("session_id", args.session_id.clone());
    if let Some(task_id) = &args.task_id {
        input = input.with_arg("task_id", task_id.clone());
    }
    if args.all {
        input = input.with_arg("all", "true");
    }
    if args.force {
        input = input.with_arg("force", "true");
    }
    if let Some(reason) = &args.reason {
        input = input.with_arg("reason", reason.clone());
    }
    input
}

fn rlm_recover_tool_input(args: &AgentsRlmRecoverArgs) -> ToolInput {
    let mut input = ToolInput::new();
    if let Some(session_id) = &args.session_id {
        input = input.with_arg("session_id", session_id.clone());
    }
    if args.all {
        input = input.with_arg("all", "true");
    }
    if let Some(mode) = &args.mode {
        input = input.with_arg("mode", mode.clone());
    }
    if args.dry_run {
        input = input.with_arg("dry_run", "true");
    }
    if args.force {
        input = input.with_arg("force", "true");
    }
    if let Some(limit) = args.limit {
        input = input.with_arg("limit", limit.to_string());
    }
    if let Some(reason) = &args.reason {
        input = input.with_arg("reason", reason.clone());
    }
    input
}

fn rlm_stop_tool_input(args: &AgentsRlmStopArgs) -> ToolInput {
    let mut input = ToolInput::new().with_arg("session_id", args.session_id.clone());
    if let Some(reason) = &args.reason {
        input = input.with_arg("reason", reason.clone());
    }
    input
}

fn rlm_run_next_tool_input(args: &AgentsRlmRunNextArgs) -> ToolInput {
    let mut input = ToolInput::new().with_arg("session_id", args.session_id.clone());
    if let Some(task_id) = &args.task_id {
        input = input.with_arg("task_id", task_id.clone());
    }
    if args.dry_run {
        input = input.with_arg("dry_run", "true");
    }
    input
}

fn rlm_drain_tool_input(args: &AgentsRlmDrainArgs) -> ToolInput {
    let mut input = ToolInput::new().with_arg("session_id", args.session_id.clone());
    if let Some(max_turns) = args.max_turns {
        input = input.with_arg("max_turns", max_turns.to_string());
    }
    if args.dry_run {
        input = input.with_arg("dry_run", "true");
    }
    input
}

fn print_rlm_cli_output(summary: &str, json: bool) {
    if json {
        println!("{summary}");
        return;
    }
    let Ok(value) = parse_json_value(summary) else {
        println!("{summary}");
        return;
    };
    let Some(root) = json_as_object(&value) else {
        println!("{summary}");
        return;
    };
    if root.get("totals").is_some() {
        let sessions = root
            .get("sessions")
            .and_then(json_as_array)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        println!("RLM live sessions: {}", sessions.len());
        for session in sessions.iter().take(20) {
            print_rlm_status_line(session);
        }
        if let Some(errors) = root.get("errors").and_then(json_as_array) {
            if !errors.is_empty() {
                println!("errors: {}", errors.len());
            }
        }
        return;
    }
    if let Some(events) = root.get("events").and_then(json_as_array) {
        let session_id = root
            .get("session_id")
            .and_then(json_as_string)
            .unwrap_or("-");
        let cursor = rlm_cli_scalar(root.get("cursor"));
        let next_cursor = rlm_cli_scalar(root.get("next_cursor"));
        println!(
            "RLM events for {session_id}: {} event(s), cursor={cursor}, next_cursor={next_cursor}",
            events.len()
        );
        for event in events.iter().take(20) {
            if let Some(event) = json_as_object(event) {
                let seq = rlm_cli_scalar(event.get("seq"));
                let kind = event.get("kind").and_then(json_as_string).unwrap_or("-");
                let task_id = event.get("task_id").and_then(json_as_string).unwrap_or("-");
                println!("- seq={seq} kind={kind} task={task_id}");
            }
        }
        return;
    }
    if root.get("cancelled_count").is_some()
        || root.get("recovered_count").is_some()
        || root.get("selected_count").is_some()
        || root.get("ran_count").is_some()
        || root.get("task_id").is_some()
    {
        print_rlm_action_summary(root);
        return;
    }
    print_rlm_status_line(&value);
}

fn print_rlm_action_summary(root: &std::collections::BTreeMap<String, JsonValue>) {
    let session_id = root
        .get("session_id")
        .and_then(json_as_string)
        .unwrap_or("-");
    let dry_run = rlm_cli_scalar(root.get("dry_run"));
    if root.get("status").and_then(json_as_string) == Some("stopped") {
        println!(
            "RLM stop {session_id}: cancelled={} queued={} reason={}",
            rlm_cli_scalar(root.get("cancelled_count")),
            rlm_cli_scalar(root.get("queued_turns")),
            rlm_cli_scalar(root.get("reason"))
        );
    } else if root.get("task_id").is_some() && root.get("status").is_some() {
        println!(
            "RLM run-next {session_id}: task={} status={} queued={}",
            rlm_cli_scalar(root.get("task_id")),
            rlm_cli_scalar(root.get("status")),
            rlm_cli_scalar(root.get("queued_turns"))
        );
    } else if root.get("cancelled_count").is_some() {
        println!(
            "RLM cancel {session_id}: cancelled={} active_owner_cancelled={} interrupted={} queued={}",
            rlm_cli_scalar(root.get("cancelled_count")),
            rlm_cli_scalar(root.get("active_owner_cancelled")),
            rlm_cli_scalar(root.get("interrupted")),
            rlm_cli_scalar(root.get("queued_turns"))
        );
    } else if root.get("recovered_count").is_some() {
        println!(
            "RLM recover {session_id}: recovered={} mode={} dry_run={} force={} queued={}",
            rlm_cli_scalar(root.get("recovered_count")),
            rlm_cli_scalar(root.get("mode")),
            dry_run,
            rlm_cli_scalar(root.get("force")),
            rlm_cli_scalar(root.get("queued_turns"))
        );
    } else if root.get("selected_count").is_some() {
        println!(
            "RLM drain {session_id}: selected={} max_turns={} dry_run={}",
            rlm_cli_scalar(root.get("selected_count")),
            rlm_cli_scalar(root.get("max_turns")),
            dry_run
        );
    } else if root.get("ran_count").is_some() {
        println!(
            "RLM drain {session_id}: ran={} queued={} dry_run={}",
            rlm_cli_scalar(root.get("ran_count")),
            rlm_cli_scalar(root.get("queued_turns")),
            dry_run
        );
    }
    if let Some(actions) = root.get("actions").and_then(json_as_array) {
        for action in actions.iter().take(5) {
            if let Some(action) = json_as_object(action) {
                println!(
                    "- task={} action={} reason={}",
                    rlm_cli_scalar(action.get("task_id")),
                    rlm_cli_scalar(action.get("action")),
                    rlm_cli_scalar(action.get("reason"))
                );
            }
        }
    }
}

fn print_rlm_status_line(value: &JsonValue) {
    let Some(root) = json_as_object(value) else {
        println!("{}", json_value_to_string(value));
        return;
    };
    let session_id = root
        .get("session_id")
        .and_then(json_as_string)
        .unwrap_or("-");
    if matches!(root.get("exists"), Some(JsonValue::Bool(false))) {
        println!("RLM live session {session_id}: not found");
        return;
    }
    let status = root.get("status").and_then(json_as_string).unwrap_or("-");
    let queued = rlm_cli_scalar(root.get("queued_turns_runtime"));
    let active = rlm_cli_scalar(root.get("active_turn_id"));
    let owner = root
        .get("daemon_owner")
        .and_then(json_as_string)
        .unwrap_or("-");
    let alive = rlm_cli_scalar(root.get("daemon_alive"));
    println!(
        "RLM live session {session_id}: status={status} queued={queued} active={active} owner={owner} alive={alive}"
    );
    if let Some(actions) = root.get("recommended_actions").and_then(json_as_array) {
        let actions = actions
            .iter()
            .filter_map(json_as_string)
            .take(3)
            .collect::<Vec<_>>();
        if !actions.is_empty() {
            println!("  next: {}", actions.join("; "));
        }
    }
}

fn rlm_cli_scalar(value: Option<&JsonValue>) -> String {
    match value {
        Some(JsonValue::String(value)) => value.clone(),
        Some(JsonValue::Number(value)) => value.clone(),
        Some(JsonValue::Bool(value)) => value.to_string(),
        Some(JsonValue::Null) | None => "-".to_string(),
        Some(value) => json_value_to_string(value),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ServiceTemplate {
    path: &'static str,
    body: String,
}

#[derive(Debug, Clone)]
struct ServiceTemplateConfig {
    kind: AgentsServiceKind,
    out: Option<PathBuf>,
    bin: String,
    workdir: String,
    addr: String,
    interval_ms: u64,
    budget: Option<usize>,
}

fn run_shell_supervisor(args: AgentsShellSupervisorArgs) -> AppResult<()> {
    let cwd = std::env::current_dir()?;
    if args.once {
        let output = ExecShellSupervisorStatusTool
            .execute(ToolInput::new().with_arg("cwd", cwd.display().to_string()))?;
        if args.json {
            let mut object = BTreeMap::new();
            object.insert(
                "kind".to_string(),
                JsonValue::String("deepseek.exec_shell.supervisor_once.v1".to_string()),
            );
            object.insert(
                "cwd".to_string(),
                JsonValue::String(cwd.display().to_string()),
            );
            object.insert("status".to_string(), JsonValue::String(output.summary));
            println!("{}", json_value_to_string(&JsonValue::Object(object)));
        } else {
            println!("{}", output.summary);
        }
        return Ok(());
    }
    run_shell_supervisor_daemon(&cwd, args.json)
}

#[cfg(unix)]
fn run_shell_supervisor_daemon(cwd: &Path, json: bool) -> AppResult<()> {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::net::{UnixListener, UnixStream};

    let state_dir = cwd.join(".dscode/shell-supervisor");
    fs::create_dir_all(&state_dir)?;
    fs::set_permissions(&state_dir, fs::Permissions::from_mode(0o700))?;
    let socket = state_dir.join("supervisor.sock");
    if let Some(path_bytes) = unix_socket_path_too_long(&socket) {
        return Err(app_error(format!(
            "shell supervisor socket path is too long for Unix domain sockets: {} ({} bytes; limit is < {} bytes). Use a shorter workspace path.",
            socket.display(),
            path_bytes,
            SHELL_SUPERVISOR_UNIX_SOCKET_MAX_BYTES
        )));
    }
    if socket.exists() {
        if UnixStream::connect(&socket).is_ok() {
            return Err(app_error(format!(
                "shell supervisor socket is already active: {}",
                socket.display()
            )));
        }
        fs::remove_file(&socket)?;
    }
    let listener = UnixListener::bind(&socket)?;
    fs::set_permissions(&socket, fs::Permissions::from_mode(0o600))?;
    let epoch = format_epoch_seconds(current_epoch_seconds());
    write_shell_supervisor_manifest(cwd, &socket, &epoch)?;
    if json {
        println!(
            "{}",
            json_value_to_string(&shell_supervisor_event_json(
                "started", cwd, &socket, &epoch, None,
            ))
        );
    } else {
        println!(
            "shell supervisor protocol bridge listening: {}",
            socket.display()
        );
    }

    for stream in listener.incoming() {
        let stream = stream?;
        let shutdown = handle_shell_supervisor_stream(stream, cwd, &socket, &epoch)?;
        if shutdown {
            break;
        }
    }
    let _ = fs::remove_file(&socket);
    if json {
        println!(
            "{}",
            json_value_to_string(&shell_supervisor_event_json(
                "stopped", cwd, &socket, &epoch, None,
            ))
        );
    }
    Ok(())
}

#[cfg(not(unix))]
fn run_shell_supervisor_daemon(_cwd: &Path, _json: bool) -> AppResult<()> {
    Err(app_error(
        "shell supervisor protocol bridge is currently supported only on Unix",
    ))
}

#[cfg(unix)]
fn handle_shell_supervisor_stream(
    mut stream: std::os::unix::net::UnixStream,
    cwd: &Path,
    socket: &Path,
    epoch: &str,
) -> AppResult<bool> {
    let mut line = String::new();
    {
        let mut reader = BufReader::new(&mut stream);
        reader.read_line(&mut line)?;
    }
    let (response, shutdown) = match parse_shell_supervisor_request(&line) {
        Ok(request) => (
            shell_supervisor_protocol_response_for_request(&request, cwd, socket, epoch),
            request.method == "shutdown",
        ),
        Err(error) => (
            shell_supervisor_protocol_error_response(cwd, socket, epoch, &error.to_string()),
            false,
        ),
    };
    stream.write_all(json_value_to_string(&response).as_bytes())?;
    stream.write_all(b"\n")?;
    stream.flush()?;
    Ok(shutdown)
}

#[derive(Debug, Clone)]
struct ShellSupervisorRequest {
    method: String,
    args: BTreeMap<String, JsonValue>,
}

impl ShellSupervisorRequest {
    #[cfg(test)]
    fn method_only(method: &str) -> Self {
        Self {
            method: method.to_string(),
            args: BTreeMap::new(),
        }
    }
}

fn parse_shell_supervisor_request(line: &str) -> AppResult<ShellSupervisorRequest> {
    let value = if line.trim().is_empty() {
        JsonValue::Object(BTreeMap::new())
    } else {
        parse_json_value(line.trim())?
    };
    let Some(object) = json_as_object(&value) else {
        return Err(app_error("shell supervisor request must be a JSON object"));
    };
    Ok(ShellSupervisorRequest {
        method: object
            .get("method")
            .and_then(json_as_string)
            .unwrap_or("health")
            .to_string(),
        args: object.clone(),
    })
}

#[cfg(test)]
fn parse_shell_supervisor_method(line: &str) -> AppResult<String> {
    Ok(parse_shell_supervisor_request(line)?.method)
}

#[cfg(test)]
fn shell_supervisor_protocol_response(
    method: &str,
    cwd: &Path,
    socket: &Path,
    epoch: &str,
) -> JsonValue {
    shell_supervisor_protocol_response_for_request(
        &ShellSupervisorRequest::method_only(method),
        cwd,
        socket,
        epoch,
    )
}

fn shell_supervisor_protocol_response_for_request(
    request: &ShellSupervisorRequest,
    cwd: &Path,
    socket: &Path,
    epoch: &str,
) -> JsonValue {
    let method = request.method.as_str();
    let supported = SHELL_SUPERVISOR_SUPPORTED_METHODS.contains(&method);
    let (mut active_jobs, mut active_jobs_error) = match count_active_durable_shell_jobs(cwd) {
        Ok(count) => (count, None),
        Err(error) => (0, Some(error.to_string())),
    };
    let mut response = BTreeMap::from([
        (
            "kind".to_string(),
            JsonValue::String("deepseek.exec_shell.supervisor.response.v1".to_string()),
        ),
        ("method".to_string(), JsonValue::String(method.to_string())),
        (
            "status".to_string(),
            JsonValue::String(if supported { "ok" } else { "unsupported" }.to_string()),
        ),
        (
            "cwd".to_string(),
            JsonValue::String(cwd.display().to_string()),
        ),
        (
            "supervisor_pid".to_string(),
            JsonValue::Number(std::process::id().to_string()),
        ),
        (
            "supervisor_socket".to_string(),
            JsonValue::String(socket.display().to_string()),
        ),
        (
            "supervisor_epoch".to_string(),
            JsonValue::String(epoch.to_string()),
        ),
        (
            "protocol".to_string(),
            JsonValue::String("newline-json-v1".to_string()),
        ),
        (
            "methods".to_string(),
            shell_supervisor_method_json(SHELL_SUPERVISOR_SUPPORTED_METHODS),
        ),
        (
            "unsupported_methods".to_string(),
            shell_supervisor_method_json(SHELL_SUPERVISOR_UNSUPPORTED_PTY_METHODS),
        ),
        (
            "pty_backend".to_string(),
            JsonValue::String("none".to_string()),
        ),
        (
            "native_pty".to_string(),
            JsonValue::Bool(native_supervisor_pty_supported()),
        ),
        (
            "active_jobs".to_string(),
            JsonValue::Number(active_jobs.to_string()),
        ),
    ]);
    if !supported {
        response.insert(
            "error".to_string(),
            JsonValue::String(format!(
                "shell supervisor method `{method}` is not supported by this protocol"
            )),
        );
    } else {
        match method {
            "show" => {
                let inventory = ExecShellListTool
                    .execute(ToolInput::new().with_arg("cwd", cwd.display().to_string()));
                match inventory {
                    Ok(output) => {
                        response.insert(
                            "job_inventory".to_string(),
                            JsonValue::String(output.summary),
                        );
                    }
                    Err(error) => {
                        response.insert(
                            "job_inventory_error".to_string(),
                            JsonValue::String(error.to_string()),
                        );
                    }
                }
            }
            "start" => match shell_supervisor_start_job(request, cwd, socket, epoch) {
                Ok(start) => {
                    response.insert("task_id".to_string(), JsonValue::String(start.task_id));
                    response.insert(
                        "start_summary".to_string(),
                        JsonValue::String(start.summary),
                    );
                    response.insert("job_tty".to_string(), JsonValue::Bool(start.tty));
                    response.insert(
                        "job_pty_backend".to_string(),
                        JsonValue::String(start.pty_backend),
                    );
                    match count_active_durable_shell_jobs(cwd) {
                        Ok(count) => {
                            active_jobs = count;
                            active_jobs_error = None;
                            response.insert(
                                "active_jobs".to_string(),
                                JsonValue::Number(active_jobs.to_string()),
                            );
                        }
                        Err(error) => {
                            active_jobs = 0;
                            active_jobs_error = Some(error.to_string());
                            response.insert(
                                "active_jobs".to_string(),
                                JsonValue::Number(active_jobs.to_string()),
                            );
                        }
                    }
                }
                Err(error) => {
                    response.insert("status".to_string(), JsonValue::String("error".to_string()));
                    response.insert("error".to_string(), JsonValue::String(error.to_string()));
                }
            },
            "wait" => shell_supervisor_apply_tool_result(
                &mut response,
                "wait_summary",
                shell_supervisor_wait_job(request, cwd),
            ),
            "replay" => shell_supervisor_apply_tool_result(
                &mut response,
                "replay_summary",
                shell_supervisor_replay_job(request, cwd),
            ),
            "attach" => shell_supervisor_apply_tool_result(
                &mut response,
                "attach_summary",
                shell_supervisor_attach_job(request, cwd),
            ),
            "stdin" => shell_supervisor_apply_tool_result(
                &mut response,
                "stdin_summary",
                shell_supervisor_stdin_job(request, cwd),
            ),
            "resize" => shell_supervisor_apply_tool_result(
                &mut response,
                "resize_summary",
                shell_supervisor_resize_job(request, cwd),
            ),
            "cancel" => shell_supervisor_apply_tool_result(
                &mut response,
                "cancel_summary",
                shell_supervisor_cancel_job(request, cwd),
            ),
            _ => {}
        }
    }
    if supported && !matches!(method, "health" | "status" | "show" | "shutdown") {
        match count_active_durable_shell_jobs(cwd) {
            Ok(count) => {
                active_jobs = count;
                active_jobs_error = None;
                response.insert(
                    "active_jobs".to_string(),
                    JsonValue::Number(active_jobs.to_string()),
                );
            }
            Err(error) => {
                active_jobs = 0;
                active_jobs_error = Some(error.to_string());
                response.insert(
                    "active_jobs".to_string(),
                    JsonValue::Number(active_jobs.to_string()),
                );
            }
        }
    }
    if let Some(error) = active_jobs_error {
        response.insert("active_jobs_error".to_string(), JsonValue::String(error));
    } else if let Err(error) =
        refresh_shell_supervisor_manifest_if_present(cwd, socket, epoch, active_jobs)
    {
        response.insert(
            "manifest_refresh_error".to_string(),
            JsonValue::String(error.to_string()),
        );
    }
    JsonValue::Object(response)
}

fn shell_supervisor_apply_tool_result(
    response: &mut BTreeMap<String, JsonValue>,
    summary_key: &str,
    result: AppResult<crate::tools::types::ToolOutput>,
) {
    match result {
        Ok(output) => {
            response.insert(summary_key.to_string(), JsonValue::String(output.summary));
        }
        Err(error) => {
            response.insert("status".to_string(), JsonValue::String("error".to_string()));
            response.insert("error".to_string(), JsonValue::String(error.to_string()));
        }
    }
}

struct ShellSupervisorStart {
    task_id: String,
    summary: String,
    tty: bool,
    pty_backend: String,
}

fn shell_supervisor_start_job(
    request: &ShellSupervisorRequest,
    supervisor_cwd: &Path,
    socket: &Path,
    epoch: &str,
) -> AppResult<ShellSupervisorStart> {
    let command = shell_supervisor_request_string(request, "command")
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| app_error("shell supervisor start requires command"))?;
    let cwd = shell_supervisor_request_cwd(request, supervisor_cwd)?;
    let mut input = ToolInput::new()
        .with_arg("command", command.to_string())
        .with_arg("cwd", cwd.display().to_string());
    for key in [
        "stdin",
        "input",
        "timeout_ms",
        "tty",
        "tty_rows",
        "tty_cols",
    ] {
        if let Some(value) = shell_supervisor_request_scalar(request, key)? {
            input = input.with_arg(key, value);
        }
    }
    if let Some(env) = shell_supervisor_request_object(request, "env") {
        for (key, value) in env {
            let Some(value) = shell_supervisor_json_scalar(value) else {
                return Err(app_error(format!(
                    "shell supervisor start env.{key} must be a string, number, or bool"
                )));
            };
            input = input.with_arg(format!("env.{key}"), value);
        }
    }
    let tty = shell_supervisor_request_bool(request, "tty");
    if tty {
        input = input
            .with_arg("pty_backend", "native-supervisor")
            .with_arg("supervisor_socket", socket.display().to_string())
            .with_arg("supervisor_epoch", epoch.to_string());
    }
    let output = TaskShellStartTool.execute(input)?;
    let task_id = shell_supervisor_summary_value(&output.summary, "task_id")
        .ok_or_else(|| app_error("shell supervisor start did not return a task_id"))?;
    let pty_backend = shell_supervisor_summary_value(&output.summary, "pty_backend")
        .unwrap_or_else(|| "none".to_string());
    Ok(ShellSupervisorStart {
        task_id,
        summary: output.summary,
        tty,
        pty_backend,
    })
}

fn shell_supervisor_wait_job(
    request: &ShellSupervisorRequest,
    supervisor_cwd: &Path,
) -> AppResult<crate::tools::types::ToolOutput> {
    let input = shell_supervisor_task_tool_input(request, supervisor_cwd, &["wait", "timeout_ms"])?;
    ExecShellWaitTool {
        tool_name: "exec_shell_wait",
    }
    .execute(input)
}

fn shell_supervisor_replay_job(
    request: &ShellSupervisorRequest,
    supervisor_cwd: &Path,
) -> AppResult<crate::tools::types::ToolOutput> {
    let input = shell_supervisor_task_tool_input(
        request,
        supervisor_cwd,
        &["stream", "offset", "cursor", "limit_bytes", "tail"],
    )?;
    ExecShellReplayTool.execute(input)
}

fn shell_supervisor_attach_job(
    request: &ShellSupervisorRequest,
    supervisor_cwd: &Path,
) -> AppResult<crate::tools::types::ToolOutput> {
    let input = shell_supervisor_task_tool_input(
        request,
        supervisor_cwd,
        &["offset", "cursor", "limit_bytes", "tail", "wait_ms"],
    )?;
    ExecShellAttachTool.execute(input)
}

fn shell_supervisor_stdin_job(
    request: &ShellSupervisorRequest,
    supervisor_cwd: &Path,
) -> AppResult<crate::tools::types::ToolOutput> {
    let input = shell_supervisor_task_tool_input(
        request,
        supervisor_cwd,
        &["input", "stdin", "data", "close_stdin", "timeout_ms"],
    )?;
    ExecShellInteractTool {
        tool_name: "exec_shell_interact",
    }
    .execute(input)
}

fn shell_supervisor_resize_job(
    request: &ShellSupervisorRequest,
    supervisor_cwd: &Path,
) -> AppResult<crate::tools::types::ToolOutput> {
    let input = shell_supervisor_task_tool_input(
        request,
        supervisor_cwd,
        &["tty_rows", "tty_cols", "rows", "cols"],
    )?;
    ExecShellResizeTool.execute(input)
}

fn shell_supervisor_cancel_job(
    request: &ShellSupervisorRequest,
    supervisor_cwd: &Path,
) -> AppResult<crate::tools::types::ToolOutput> {
    let cwd = shell_supervisor_request_cwd(request, supervisor_cwd)?;
    let mut input = ToolInput::new().with_arg("cwd", cwd.display().to_string());
    if shell_supervisor_request_bool(request, "all") {
        input = input.with_arg("all", "true");
    } else {
        let task_id = shell_supervisor_request_task_id(
            request,
            "shell supervisor cancel requires task_id or all=true",
        )?;
        input = input.with_arg("task_id", task_id);
    }
    ExecShellCancelTool.execute(input)
}

fn shell_supervisor_task_tool_input(
    request: &ShellSupervisorRequest,
    supervisor_cwd: &Path,
    optional_keys: &[&str],
) -> AppResult<ToolInput> {
    let task_id =
        shell_supervisor_request_task_id(request, "shell supervisor method requires task_id")?;
    let cwd = shell_supervisor_request_cwd(request, supervisor_cwd)?;
    let mut input = ToolInput::new()
        .with_arg("task_id", task_id)
        .with_arg("cwd", cwd.display().to_string());
    for key in optional_keys {
        if let Some(value) = shell_supervisor_request_scalar(request, key)? {
            input = input.with_arg((*key).to_string(), value);
        }
    }
    Ok(input)
}

fn shell_supervisor_request_task_id(
    request: &ShellSupervisorRequest,
    missing_message: &str,
) -> AppResult<String> {
    for key in ["task_id", "id"] {
        if let Some(value) = shell_supervisor_request_scalar(request, key)? {
            let value = value.trim().to_string();
            if !value.is_empty() {
                return Ok(value);
            }
        }
    }
    Err(app_error(missing_message))
}

fn shell_supervisor_request_cwd(
    request: &ShellSupervisorRequest,
    supervisor_cwd: &Path,
) -> AppResult<PathBuf> {
    let raw = shell_supervisor_request_string(request, "cwd")
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(".");
    let requested = Path::new(raw);
    let cwd = if requested.is_absolute() {
        requested.to_path_buf()
    } else {
        supervisor_cwd.join(requested)
    };
    let normalized = cwd
        .canonicalize()
        .unwrap_or_else(|_| normalize_child_path(supervisor_cwd, requested));
    let supervisor_root = supervisor_cwd
        .canonicalize()
        .unwrap_or_else(|_| supervisor_cwd.to_path_buf());
    if !normalized.starts_with(&supervisor_root) {
        return Err(app_error(format!(
            "shell supervisor cwd must stay inside {}",
            supervisor_root.display()
        )));
    }
    Ok(normalized)
}

fn normalize_child_path(root: &Path, child: &Path) -> PathBuf {
    let path = if child.is_absolute() {
        child.to_path_buf()
    } else {
        root.join(child)
    };
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    normalized
}

fn shell_supervisor_request_string<'a>(
    request: &'a ShellSupervisorRequest,
    key: &str,
) -> Option<&'a str> {
    shell_supervisor_request_value(request, key).and_then(json_as_string)
}

fn shell_supervisor_request_scalar(
    request: &ShellSupervisorRequest,
    key: &str,
) -> AppResult<Option<String>> {
    let Some(value) = shell_supervisor_request_value(request, key) else {
        return Ok(None);
    };
    shell_supervisor_json_scalar(value)
        .map(Some)
        .ok_or_else(|| app_error(format!("shell supervisor request {key} must be scalar")))
}

fn shell_supervisor_request_bool(request: &ShellSupervisorRequest, key: &str) -> bool {
    match shell_supervisor_request_value(request, key) {
        Some(JsonValue::Bool(value)) => *value,
        Some(JsonValue::String(value)) => {
            matches!(value.trim(), "1" | "true" | "TRUE" | "yes" | "on")
        }
        Some(JsonValue::Number(value)) => value == "1",
        _ => false,
    }
}

fn shell_supervisor_request_object<'a>(
    request: &'a ShellSupervisorRequest,
    key: &str,
) -> Option<&'a BTreeMap<String, JsonValue>> {
    shell_supervisor_request_value(request, key).and_then(json_as_object)
}

fn shell_supervisor_request_value<'a>(
    request: &'a ShellSupervisorRequest,
    key: &str,
) -> Option<&'a JsonValue> {
    request.args.get(key).or_else(|| {
        ["params", "arguments"]
            .iter()
            .find_map(|container| request.args.get(*container).and_then(json_as_object))
            .and_then(|object| object.get(key))
    })
}

fn shell_supervisor_json_scalar(value: &JsonValue) -> Option<String> {
    match value {
        JsonValue::String(value) => Some(value.to_string()),
        JsonValue::Number(value) => Some(value.to_string()),
        JsonValue::Bool(value) => Some(value.to_string()),
        JsonValue::Null | JsonValue::Array(_) | JsonValue::Object(_) => None,
    }
}

fn shell_supervisor_summary_value(summary: &str, key: &str) -> Option<String> {
    summary.lines().find_map(|line| {
        line.strip_prefix(&format!("{key}: "))
            .or_else(|| line.strip_prefix(&format!("{key}=")))
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    })
}

fn shell_supervisor_protocol_error_response(
    cwd: &Path,
    socket: &Path,
    epoch: &str,
    error: &str,
) -> JsonValue {
    JsonValue::Object(BTreeMap::from([
        (
            "kind".to_string(),
            JsonValue::String("deepseek.exec_shell.supervisor.response.v1".to_string()),
        ),
        (
            "method".to_string(),
            JsonValue::String("invalid_request".to_string()),
        ),
        ("status".to_string(), JsonValue::String("error".to_string())),
        (
            "cwd".to_string(),
            JsonValue::String(cwd.display().to_string()),
        ),
        (
            "supervisor_pid".to_string(),
            JsonValue::Number(std::process::id().to_string()),
        ),
        (
            "supervisor_socket".to_string(),
            JsonValue::String(socket.display().to_string()),
        ),
        (
            "supervisor_epoch".to_string(),
            JsonValue::String(epoch.to_string()),
        ),
        (
            "protocol".to_string(),
            JsonValue::String("newline-json-v1".to_string()),
        ),
        (
            "methods".to_string(),
            shell_supervisor_method_json(SHELL_SUPERVISOR_SUPPORTED_METHODS),
        ),
        (
            "unsupported_methods".to_string(),
            shell_supervisor_method_json(SHELL_SUPERVISOR_UNSUPPORTED_PTY_METHODS),
        ),
        (
            "pty_backend".to_string(),
            JsonValue::String("none".to_string()),
        ),
        ("native_pty".to_string(), JsonValue::Bool(false)),
        (
            "active_jobs".to_string(),
            JsonValue::Number("0".to_string()),
        ),
        ("error".to_string(), JsonValue::String(error.to_string())),
    ]))
}

#[cfg(unix)]
fn write_shell_supervisor_manifest(cwd: &Path, socket: &Path, epoch: &str) -> AppResult<()> {
    let active_jobs = count_active_durable_shell_jobs(cwd)?;
    write_shell_supervisor_manifest_snapshot(cwd, socket, epoch, epoch, active_jobs)
}

fn refresh_shell_supervisor_manifest_if_present(
    cwd: &Path,
    socket: &Path,
    epoch: &str,
    active_jobs: u64,
) -> AppResult<()> {
    if !cwd.join(".dscode/shell-supervisor").is_dir() {
        return Ok(());
    }
    let updated_at = format_epoch_seconds(current_epoch_seconds());
    write_shell_supervisor_manifest_snapshot(cwd, socket, epoch, &updated_at, active_jobs)
}

fn write_shell_supervisor_manifest_snapshot(
    cwd: &Path,
    socket: &Path,
    epoch: &str,
    updated_at: &str,
    active_jobs: u64,
) -> AppResult<()> {
    let manifest = JsonValue::Object(BTreeMap::from([
        (
            "kind".to_string(),
            JsonValue::String("deepseek.exec_shell.supervisor.v1".to_string()),
        ),
        (
            "supervisor_pid".to_string(),
            JsonValue::Number(std::process::id().to_string()),
        ),
        (
            "supervisor_socket".to_string(),
            JsonValue::String(socket.display().to_string()),
        ),
        (
            "supervisor_epoch".to_string(),
            JsonValue::String(epoch.to_string()),
        ),
        (
            "protocol".to_string(),
            JsonValue::String("newline-json-v1".to_string()),
        ),
        (
            "methods".to_string(),
            shell_supervisor_method_json(SHELL_SUPERVISOR_SUPPORTED_METHODS),
        ),
        (
            "unsupported_methods".to_string(),
            shell_supervisor_method_json(SHELL_SUPERVISOR_UNSUPPORTED_PTY_METHODS),
        ),
        (
            "active_jobs".to_string(),
            JsonValue::Number(active_jobs.to_string()),
        ),
        (
            "started_at".to_string(),
            JsonValue::String(epoch.to_string()),
        ),
        (
            "updated_at".to_string(),
            JsonValue::String(updated_at.to_string()),
        ),
        ("control_token_hash".to_string(), JsonValue::Null),
    ]));
    std::fs::write(
        cwd.join(".dscode/shell-supervisor/manifest.json"),
        json_value_to_string(&manifest),
    )?;
    Ok(())
}

fn shell_supervisor_method_json(methods: &[&str]) -> JsonValue {
    JsonValue::Array(
        methods
            .iter()
            .map(|method| JsonValue::String((*method).to_string()))
            .collect(),
    )
}

fn shell_supervisor_event_json(
    event: &str,
    cwd: &Path,
    socket: &Path,
    epoch: &str,
    message: Option<&str>,
) -> JsonValue {
    let mut object = BTreeMap::from([
        (
            "kind".to_string(),
            JsonValue::String("deepseek.exec_shell.supervisor_daemon.v1".to_string()),
        ),
        ("event".to_string(), JsonValue::String(event.to_string())),
        (
            "cwd".to_string(),
            JsonValue::String(cwd.display().to_string()),
        ),
        (
            "socket".to_string(),
            JsonValue::String(socket.display().to_string()),
        ),
        ("epoch".to_string(), JsonValue::String(epoch.to_string())),
    ]);
    if let Some(message) = message {
        object.insert(
            "message".to_string(),
            JsonValue::String(message.to_string()),
        );
    }
    JsonValue::Object(object)
}

fn render_agent_services(args: AgentsServiceArgs) -> AppResult<()> {
    let config = service_template_config(args)?;
    let templates = service_templates(&config);

    if let Some(out) = &config.out {
        for template in &templates {
            let path = out.join(template.path);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&path, &template.body)?;
            println!("wrote {}", path.display());
        }
        let guide_path = out.join("SERVICES.md");
        std::fs::write(&guide_path, service_lifecycle_guide(&config, &templates))?;
        println!("wrote {}", guide_path.display());
        print_service_next_steps(config.kind, out);
    } else {
        for template in &templates {
            println!("--- {} ---", template.path);
            print!("{}", template.body);
            if !template.body.ends_with('\n') {
                println!();
            }
        }
    }
    Ok(())
}

fn service_template_config(args: AgentsServiceArgs) -> AppResult<ServiceTemplateConfig> {
    let bin = match args.bin {
        Some(bin) => normalize_service_path_or_command(bin),
        None => std::env::current_exe()?.display().to_string(),
    };
    let workdir = match args.workdir {
        Some(workdir) => normalize_service_path_or_command(workdir),
        None => std::env::current_dir()?.display().to_string(),
    };
    Ok(ServiceTemplateConfig {
        kind: args.kind,
        out: args.out.map(PathBuf::from),
        bin,
        workdir,
        addr: args.addr,
        interval_ms: args.interval_ms.max(100),
        budget: args.budget,
    })
}

fn normalize_service_path_or_command(value: String) -> String {
    let path = PathBuf::from(&value);
    if path.components().count() > 1 {
        path.display().to_string()
    } else {
        value
    }
}

fn service_templates(config: &ServiceTemplateConfig) -> Vec<ServiceTemplate> {
    let mut templates = Vec::new();
    if matches!(
        config.kind,
        AgentsServiceKind::Systemd | AgentsServiceKind::All
    ) {
        templates.push(ServiceTemplate {
            path: "systemd/deepseek-runtime.service",
            body: systemd_runtime_service(config),
        });
        templates.push(ServiceTemplate {
            path: "systemd/deepseek-agents.service",
            body: systemd_agents_service(config),
        });
        templates.push(ServiceTemplate {
            path: "systemd/deepseek-diagnostics.service",
            body: systemd_diagnostics_service(config),
        });
        templates.push(ServiceTemplate {
            path: "systemd/deepseek-shell-supervisor.service",
            body: systemd_shell_supervisor_service(config),
        });
    }
    if matches!(
        config.kind,
        AgentsServiceKind::Launchd | AgentsServiceKind::All
    ) {
        templates.push(ServiceTemplate {
            path: "launchd/com.deepseek.runtime.plist",
            body: launchd_runtime_service(config),
        });
        templates.push(ServiceTemplate {
            path: "launchd/com.deepseek.agents.plist",
            body: launchd_agents_service(config),
        });
        templates.push(ServiceTemplate {
            path: "launchd/com.deepseek.diagnostics.plist",
            body: launchd_diagnostics_service(config),
        });
        templates.push(ServiceTemplate {
            path: "launchd/com.deepseek.shell-supervisor.plist",
            body: launchd_shell_supervisor_service(config),
        });
    }
    templates
}

fn service_lifecycle_guide(
    config: &ServiceTemplateConfig,
    templates: &[ServiceTemplate],
) -> String {
    let mut out = String::new();
    out.push_str("# DeepSeekCode Service Lifecycle\n\n");
    out.push_str("Generated by `deepseek agents service` for this workspace.\n\n");
    out.push_str("## Target\n\n");
    out.push_str(&format!("- binary: `{}`\n", config.bin));
    out.push_str(&format!("- workdir: `{}`\n", config.workdir));
    out.push_str(&format!("- runtime address: `{}`\n", config.addr));
    out.push_str(&format!("- worker interval: `{}` ms\n", config.interval_ms));
    if let Some(budget) = config.budget {
        out.push_str(&format!("- daemon budget: `{budget}`\n"));
    }
    out.push_str("\n## Files\n\n");
    for template in templates {
        out.push_str(&format!("- `{}`\n", template.path));
    }
    if matches!(
        config.kind,
        AgentsServiceKind::Systemd | AgentsServiceKind::All
    ) {
        out.push_str("\n## systemd User Lifecycle\n\n");
        out.push_str("```bash\n");
        out.push_str("mkdir -p ~/.config/systemd/user\n");
        out.push_str("cp systemd/*.service ~/.config/systemd/user/\n");
        out.push_str("systemctl --user daemon-reload\n");
        out.push_str("systemctl --user enable --now deepseek-runtime.service deepseek-agents.service deepseek-diagnostics.service deepseek-shell-supervisor.service\n");
        out.push_str("systemctl --user status deepseek-runtime.service deepseek-agents.service deepseek-diagnostics.service deepseek-shell-supervisor.service\n");
        out.push_str("journalctl --user -u deepseek-runtime.service -u deepseek-agents.service -u deepseek-diagnostics.service -u deepseek-shell-supervisor.service -f\n");
        out.push_str("systemctl --user restart deepseek-runtime.service deepseek-agents.service deepseek-diagnostics.service deepseek-shell-supervisor.service\n");
        out.push_str("systemctl --user stop deepseek-runtime.service deepseek-agents.service deepseek-diagnostics.service deepseek-shell-supervisor.service\n");
        out.push_str("systemctl --user disable deepseek-runtime.service deepseek-agents.service deepseek-diagnostics.service deepseek-shell-supervisor.service\n");
        out.push_str("```\n");
    }
    if matches!(
        config.kind,
        AgentsServiceKind::Launchd | AgentsServiceKind::All
    ) {
        out.push_str("\n## launchd User Lifecycle\n\n");
        out.push_str("```bash\n");
        out.push_str("mkdir -p ~/Library/LaunchAgents\n");
        out.push_str("cp launchd/*.plist ~/Library/LaunchAgents/\n");
        out.push_str("launchctl load -w ~/Library/LaunchAgents/com.deepseek.runtime.plist ~/Library/LaunchAgents/com.deepseek.agents.plist ~/Library/LaunchAgents/com.deepseek.diagnostics.plist ~/Library/LaunchAgents/com.deepseek.shell-supervisor.plist\n");
        out.push_str("launchctl list | grep com.deepseek\n");
        out.push_str("tail -f /tmp/deepseek-runtime.out.log /tmp/deepseek-agents.out.log /tmp/deepseek-diagnostics.out.log /tmp/deepseek-shell-supervisor.out.log\n");
        out.push_str("launchctl kickstart -k gui/$(id -u)/com.deepseek.runtime gui/$(id -u)/com.deepseek.agents gui/$(id -u)/com.deepseek.diagnostics gui/$(id -u)/com.deepseek.shell-supervisor\n");
        out.push_str("launchctl unload -w ~/Library/LaunchAgents/com.deepseek.runtime.plist ~/Library/LaunchAgents/com.deepseek.agents.plist ~/Library/LaunchAgents/com.deepseek.diagnostics.plist ~/Library/LaunchAgents/com.deepseek.shell-supervisor.plist\n");
        out.push_str("```\n");
    }
    out.push_str("\n## Runtime Checks\n\n");
    out.push_str("```bash\n");
    out.push_str(&format!("curl -fsS http://{}/v1/health\n", config.addr));
    out.push_str(&format!("{} doctor --json\n", config.bin));
    out.push_str(&format!("{} agents rlm-status --json\n", config.bin));
    out.push_str(&format!("{} agents shell status --json\n", config.bin));
    out.push_str(&format!("{} diagnostics --changed --json\n", config.bin));
    out.push_str("```\n");
    out
}

fn systemd_runtime_service(config: &ServiceTemplateConfig) -> String {
    format!(
        "[Unit]\n\
Description=DeepSeekCode HTTP runtime\n\
After=network.target\n\
\n\
[Service]\n\
Type=simple\n\
WorkingDirectory={workdir}\n\
ExecStart=/usr/bin/env {bin} serve --http --addr {addr}\n\
Restart=on-failure\n\
RestartSec=5\n\
\n\
[Install]\n\
WantedBy=default.target\n",
        workdir = systemd_quote(&config.workdir),
        bin = systemd_quote(&config.bin),
        addr = systemd_quote(&config.addr)
    )
}

fn systemd_agents_service(config: &ServiceTemplateConfig) -> String {
    let budget = config
        .budget
        .map(|budget| format!(" --budget {budget}"))
        .unwrap_or_default();
    format!(
        "[Unit]\n\
Description=DeepSeekCode runtime task daemon\n\
# Runs due automations, pending runtime tasks, stale RLM recovery, and one queued live RLM turn per tick.\n\
After=network.target deepseek-runtime.service\n\
\n\
[Service]\n\
Type=simple\n\
WorkingDirectory={workdir}\n\
ExecStart=/usr/bin/env {bin} agents daemon --interval-ms {interval_ms}{budget} --json\n\
Restart=on-failure\n\
RestartSec=5\n\
\n\
[Install]\n\
WantedBy=default.target\n",
        workdir = systemd_quote(&config.workdir),
        bin = systemd_quote(&config.bin),
        interval_ms = config.interval_ms,
        budget = budget,
    )
}

fn systemd_diagnostics_service(config: &ServiceTemplateConfig) -> String {
    format!(
        "[Unit]\n\
Description=DeepSeekCode diagnostics watch worker\n\
After=network.target\n\
\n\
[Service]\n\
Type=simple\n\
WorkingDirectory={workdir}\n\
ExecStart=/usr/bin/env {bin} diagnostics --watch --changed --interval-ms {interval_ms} --json\n\
Restart=on-failure\n\
RestartSec=5\n\
\n\
[Install]\n\
WantedBy=default.target\n",
        workdir = systemd_quote(&config.workdir),
        bin = systemd_quote(&config.bin),
        interval_ms = config.interval_ms,
    )
}

fn systemd_shell_supervisor_service(config: &ServiceTemplateConfig) -> String {
    format!(
        "[Unit]\n\
Description=DeepSeekCode shell supervisor protocol bridge\n\
# Exposes workspace-local shell status/show/start/control, including native-supervisor PTY jobs where supported.\n\
After=network.target\n\
\n\
[Service]\n\
Type=simple\n\
WorkingDirectory={workdir}\n\
ExecStart=/usr/bin/env {bin} agents shell-supervisor --json\n\
Restart=on-failure\n\
RestartSec=5\n\
\n\
[Install]\n\
WantedBy=default.target\n",
        workdir = systemd_quote(&config.workdir),
        bin = systemd_quote(&config.bin),
    )
}

fn launchd_runtime_service(config: &ServiceTemplateConfig) -> String {
    launchd_plist(
        "com.deepseek.runtime",
        &config.workdir,
        &[
            "/usr/bin/env".to_string(),
            config.bin.clone(),
            "serve".to_string(),
            "--http".to_string(),
            "--addr".to_string(),
            config.addr.clone(),
        ],
        "/tmp/deepseek-runtime.out.log",
        "/tmp/deepseek-runtime.err.log",
        None,
    )
}

fn launchd_agents_service(config: &ServiceTemplateConfig) -> String {
    let mut args = vec![
        "/usr/bin/env".to_string(),
        config.bin.clone(),
        "agents".to_string(),
        "daemon".to_string(),
        "--interval-ms".to_string(),
        config.interval_ms.to_string(),
    ];
    if let Some(budget) = config.budget {
        args.push("--budget".to_string());
        args.push(budget.to_string());
    }
    args.push("--json".to_string());
    launchd_plist(
        "com.deepseek.agents",
        &config.workdir,
        &args,
        "/tmp/deepseek-agents.out.log",
        "/tmp/deepseek-agents.err.log",
        Some(
            "Runs due automations, pending runtime tasks, stale RLM recovery, and one queued live RLM turn per tick.",
        ),
    )
}

fn launchd_diagnostics_service(config: &ServiceTemplateConfig) -> String {
    launchd_plist(
        "com.deepseek.diagnostics",
        &config.workdir,
        &[
            "/usr/bin/env".to_string(),
            config.bin.clone(),
            "diagnostics".to_string(),
            "--watch".to_string(),
            "--changed".to_string(),
            "--interval-ms".to_string(),
            config.interval_ms.to_string(),
            "--json".to_string(),
        ],
        "/tmp/deepseek-diagnostics.out.log",
        "/tmp/deepseek-diagnostics.err.log",
        None,
    )
}

fn launchd_shell_supervisor_service(config: &ServiceTemplateConfig) -> String {
    launchd_plist(
        "com.deepseek.shell-supervisor",
        &config.workdir,
        &[
            "/usr/bin/env".to_string(),
            config.bin.clone(),
            "agents".to_string(),
            "shell-supervisor".to_string(),
            "--json".to_string(),
        ],
        "/tmp/deepseek-shell-supervisor.out.log",
        "/tmp/deepseek-shell-supervisor.err.log",
        Some(
            "Exposes workspace-local shell status/show/start/control, including native-supervisor PTY jobs where supported.",
        ),
    )
}

fn launchd_plist(
    label: &str,
    workdir: &str,
    args: &[String],
    stdout_path: &str,
    stderr_path: &str,
    comment: Option<&str>,
) -> String {
    let mut body = String::new();
    body.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    body.push_str("<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" ");
    body.push_str("\"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n");
    body.push_str("<plist version=\"1.0\">\n<dict>\n");
    if let Some(comment) = comment {
        body.push_str(&format!("  <!-- {} -->\n", xml_escape(comment)));
    }
    body.push_str("  <key>Label</key>\n");
    body.push_str(&format!("  <string>{}</string>\n", xml_escape(label)));
    body.push_str("  <key>WorkingDirectory</key>\n");
    body.push_str(&format!("  <string>{}</string>\n", xml_escape(workdir)));
    body.push_str("  <key>ProgramArguments</key>\n  <array>\n");
    for arg in args {
        body.push_str(&format!("    <string>{}</string>\n", xml_escape(arg)));
    }
    body.push_str("  </array>\n");
    body.push_str("  <key>RunAtLoad</key>\n  <true/>\n");
    body.push_str("  <key>KeepAlive</key>\n  <true/>\n");
    body.push_str("  <key>StandardOutPath</key>\n");
    body.push_str(&format!("  <string>{}</string>\n", xml_escape(stdout_path)));
    body.push_str("  <key>StandardErrorPath</key>\n");
    body.push_str(&format!("  <string>{}</string>\n", xml_escape(stderr_path)));
    body.push_str("</dict>\n</plist>\n");
    body
}

fn systemd_quote(value: &str) -> String {
    if !value.is_empty()
        && value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '/' | '.' | '_' | '-' | ':' | '%'))
    {
        value.to_string()
    } else {
        format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
    }
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn print_service_next_steps(kind: AgentsServiceKind, out: &Path) {
    match kind {
        AgentsServiceKind::Systemd => {
            println!(
                "next: review SERVICES.md, install systemd/*.service into ~/.config/systemd/user, then run `systemctl --user daemon-reload`"
            );
        }
        AgentsServiceKind::Launchd => {
            println!(
                "next: review SERVICES.md, install launchd/*.plist into ~/Library/LaunchAgents, then load with `launchctl load -w <plist>`"
            );
        }
        AgentsServiceKind::All => {
            println!(
                "next: review {}/SERVICES.md, choose the files for your supervisor, and install them with the platform tool",
                out.display()
            );
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ServiceDoctorCheck {
    status: ServiceDoctorStatus,
    name: String,
    message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ServiceDoctorStatus {
    Ok,
    Warn,
    Blocker,
}

#[derive(Debug, Clone)]
struct ServiceDoctorReport {
    config: ServiceTemplateConfig,
    checks: Vec<ServiceDoctorCheck>,
}

fn run_service_doctor(args: AgentsServiceDoctorArgs) -> AppResult<()> {
    let json = args.json;
    let config = service_template_config(AgentsServiceArgs {
        kind: args.kind,
        out: args.out,
        bin: args.bin,
        workdir: args.workdir,
        addr: args.addr,
        interval_ms: args.interval_ms,
        budget: args.budget,
    })?;
    let report = build_service_doctor_report(config);
    if json {
        println!("{}", render_service_doctor_json(&report));
    } else {
        print!("{}", render_service_doctor_text(&report));
    }
    if report.blocker_count() > 0 {
        return Err(app_error(format!(
            "service doctor found {} blocker(s)",
            report.blocker_count()
        )));
    }
    Ok(())
}

impl ServiceDoctorReport {
    fn blocker_count(&self) -> usize {
        self.checks
            .iter()
            .filter(|check| check.status == ServiceDoctorStatus::Blocker)
            .count()
    }

    fn warning_count(&self) -> usize {
        self.checks
            .iter()
            .filter(|check| check.status == ServiceDoctorStatus::Warn)
            .count()
    }
}

fn build_service_doctor_report(config: ServiceTemplateConfig) -> ServiceDoctorReport {
    let templates = service_templates(&config);
    let mut checks = Vec::new();

    service_doctor_check_binary(&config, &mut checks);
    service_doctor_check_workdir(&config, &mut checks);
    service_doctor_check_template_topology(&config, &templates, &mut checks);
    service_doctor_check_platform_tools(config.kind, &mut checks);
    service_doctor_check_output_dir(&config, &templates, &mut checks);

    ServiceDoctorReport { config, checks }
}

fn service_doctor_check_binary(
    config: &ServiceTemplateConfig,
    checks: &mut Vec<ServiceDoctorCheck>,
) {
    let binary = config.bin.trim();
    if binary.is_empty() {
        push_service_doctor_check(
            checks,
            ServiceDoctorStatus::Blocker,
            "binary",
            "service binary is empty",
        );
        return;
    }
    if service_value_is_path(binary) {
        let path = Path::new(binary);
        if path.is_file() {
            push_service_doctor_check(
                checks,
                ServiceDoctorStatus::Ok,
                "binary",
                format!("binary path exists: {binary}"),
            );
        } else {
            push_service_doctor_check(
                checks,
                ServiceDoctorStatus::Blocker,
                "binary",
                format!("binary path is missing or not a file: {binary}"),
            );
        }
    } else if command_on_path(binary) {
        push_service_doctor_check(
            checks,
            ServiceDoctorStatus::Ok,
            "binary",
            format!("binary command is on PATH: {binary}"),
        );
    } else {
        push_service_doctor_check(
            checks,
            ServiceDoctorStatus::Blocker,
            "binary",
            format!("binary command is not on PATH: {binary}"),
        );
    }
}

fn service_doctor_check_workdir(
    config: &ServiceTemplateConfig,
    checks: &mut Vec<ServiceDoctorCheck>,
) {
    let workdir = config.workdir.trim();
    if workdir.is_empty() {
        push_service_doctor_check(
            checks,
            ServiceDoctorStatus::Blocker,
            "workdir",
            "service workdir is empty",
        );
        return;
    }
    let path = Path::new(workdir);
    if path.is_dir() {
        push_service_doctor_check(
            checks,
            ServiceDoctorStatus::Ok,
            "workdir",
            format!("workspace directory exists: {workdir}"),
        );
    } else {
        push_service_doctor_check(
            checks,
            ServiceDoctorStatus::Blocker,
            "workdir",
            format!("workspace directory is missing or not a directory: {workdir}"),
        );
    }
}

fn service_doctor_check_template_topology(
    config: &ServiceTemplateConfig,
    templates: &[ServiceTemplate],
    checks: &mut Vec<ServiceDoctorCheck>,
) {
    let expected = expected_service_template_paths(config.kind);
    for path in &expected {
        if templates.iter().any(|template| template.path == *path) {
            push_service_doctor_check(
                checks,
                ServiceDoctorStatus::Ok,
                "template_set",
                format!("template is rendered: {path}"),
            );
        } else {
            push_service_doctor_check(
                checks,
                ServiceDoctorStatus::Blocker,
                "template_set",
                format!("template is missing from render set: {path}"),
            );
        }
    }

    service_doctor_check_template_command(
        templates,
        "runtime_service",
        "runtime",
        &["serve", "--http"],
        checks,
    );
    service_doctor_check_template_command(
        templates,
        "agents_service",
        "agents",
        &["agents", "daemon"],
        checks,
    );
    service_doctor_check_template_command(
        templates,
        "diagnostics_service",
        "diagnostics",
        &["diagnostics", "--watch"],
        checks,
    );
    service_doctor_check_template_command(
        templates,
        "shell_supervisor_service",
        "shell-supervisor",
        &["agents", "shell-supervisor", "--json"],
        checks,
    );
}

fn service_doctor_check_template_command(
    templates: &[ServiceTemplate],
    name: &str,
    path_marker: &str,
    tokens: &[&str],
    checks: &mut Vec<ServiceDoctorCheck>,
) {
    let matching = templates
        .iter()
        .filter(|template| template.path.contains(path_marker))
        .collect::<Vec<_>>();
    if matching.is_empty() {
        push_service_doctor_check(
            checks,
            ServiceDoctorStatus::Blocker,
            name,
            format!("no template path contains `{path_marker}`"),
        );
        return;
    }
    if matching
        .iter()
        .all(|template| tokens.iter().all(|token| template.body.contains(token)))
    {
        push_service_doctor_check(
            checks,
            ServiceDoctorStatus::Ok,
            name,
            format!("all {path_marker} templates contain {}", tokens.join(" ")),
        );
    } else {
        push_service_doctor_check(
            checks,
            ServiceDoctorStatus::Blocker,
            name,
            format!(
                "one or more {path_marker} templates are missing {}",
                tokens.join(" ")
            ),
        );
    }
}

fn service_doctor_check_platform_tools(
    kind: AgentsServiceKind,
    checks: &mut Vec<ServiceDoctorCheck>,
) {
    if matches!(kind, AgentsServiceKind::Systemd | AgentsServiceKind::All) {
        if command_on_path("systemctl") {
            push_service_doctor_check(
                checks,
                ServiceDoctorStatus::Ok,
                "systemd",
                "systemctl is on PATH",
            );
        } else {
            push_service_doctor_check(
                checks,
                ServiceDoctorStatus::Warn,
                "systemd",
                "systemctl is not on PATH; generated systemd files can still be reviewed",
            );
        }
    }
    if matches!(kind, AgentsServiceKind::Launchd | AgentsServiceKind::All) {
        if command_on_path("launchctl") {
            push_service_doctor_check(
                checks,
                ServiceDoctorStatus::Ok,
                "launchd",
                "launchctl is on PATH",
            );
        } else {
            push_service_doctor_check(
                checks,
                ServiceDoctorStatus::Warn,
                "launchd",
                "launchctl is not on PATH; generated launchd files can still be reviewed",
            );
        }
    }
}

fn service_doctor_check_output_dir(
    config: &ServiceTemplateConfig,
    templates: &[ServiceTemplate],
    checks: &mut Vec<ServiceDoctorCheck>,
) {
    let Some(out) = config.out.as_ref() else {
        push_service_doctor_check(
            checks,
            ServiceDoctorStatus::Ok,
            "output",
            "no --out directory supplied; on-disk template comparison skipped",
        );
        return;
    };
    if !out.is_dir() {
        push_service_doctor_check(
            checks,
            ServiceDoctorStatus::Blocker,
            "output",
            format!("service output directory is missing: {}", out.display()),
        );
        return;
    }

    for template in templates {
        let path = out.join(template.path);
        match std::fs::read_to_string(&path) {
            Ok(content) if content == template.body => push_service_doctor_check(
                checks,
                ServiceDoctorStatus::Ok,
                "output",
                format!(
                    "generated template matches current render: {}",
                    path.display()
                ),
            ),
            Ok(_) => push_service_doctor_check(
                checks,
                ServiceDoctorStatus::Blocker,
                "output",
                format!("generated template is stale or differs: {}", path.display()),
            ),
            Err(error) => push_service_doctor_check(
                checks,
                ServiceDoctorStatus::Blocker,
                "output",
                format!(
                    "generated template is missing or unreadable: {}: {error}",
                    path.display()
                ),
            ),
        }
    }

    let guide_path = out.join("SERVICES.md");
    match std::fs::read_to_string(&guide_path) {
        Ok(content)
            if content.contains("DeepSeekCode Service Lifecycle")
                && content.contains(&config.bin)
                && content.contains(&config.workdir) =>
        {
            push_service_doctor_check(
                checks,
                ServiceDoctorStatus::Ok,
                "output",
                format!(
                    "SERVICES.md matches target metadata: {}",
                    guide_path.display()
                ),
            );
        }
        Ok(_) => push_service_doctor_check(
            checks,
            ServiceDoctorStatus::Blocker,
            "output",
            format!(
                "SERVICES.md is stale or missing target metadata: {}",
                guide_path.display()
            ),
        ),
        Err(error) => push_service_doctor_check(
            checks,
            ServiceDoctorStatus::Blocker,
            "output",
            format!(
                "SERVICES.md is missing or unreadable: {}: {error}",
                guide_path.display()
            ),
        ),
    }
}

fn push_service_doctor_check(
    checks: &mut Vec<ServiceDoctorCheck>,
    status: ServiceDoctorStatus,
    name: impl Into<String>,
    message: impl Into<String>,
) {
    checks.push(ServiceDoctorCheck {
        status,
        name: name.into(),
        message: message.into(),
    });
}

fn expected_service_template_paths(kind: AgentsServiceKind) -> Vec<&'static str> {
    let mut paths = Vec::new();
    if matches!(kind, AgentsServiceKind::Systemd | AgentsServiceKind::All) {
        paths.extend([
            "systemd/deepseek-runtime.service",
            "systemd/deepseek-agents.service",
            "systemd/deepseek-diagnostics.service",
            "systemd/deepseek-shell-supervisor.service",
        ]);
    }
    if matches!(kind, AgentsServiceKind::Launchd | AgentsServiceKind::All) {
        paths.extend([
            "launchd/com.deepseek.runtime.plist",
            "launchd/com.deepseek.agents.plist",
            "launchd/com.deepseek.diagnostics.plist",
            "launchd/com.deepseek.shell-supervisor.plist",
        ]);
    }
    paths
}

fn service_value_is_path(value: &str) -> bool {
    value.contains('/') || value.contains('\\')
}

fn command_on_path(command: &str) -> bool {
    if command.trim().is_empty() || service_value_is_path(command) {
        return false;
    }
    let Some(paths) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&paths).any(|dir| {
        let candidate = dir.join(command);
        if candidate.is_file() {
            return true;
        }
        #[cfg(windows)]
        {
            let exe = dir.join(format!("{command}.exe"));
            if exe.is_file() {
                return true;
            }
        }
        false
    })
}

fn render_service_doctor_text(report: &ServiceDoctorReport) -> String {
    let mut out = String::new();
    out.push_str("DeepSeekCode service doctor\n");
    out.push_str(&format!(
        "  kind: {}\n",
        service_kind_label(report.config.kind)
    ));
    out.push_str(&format!("  binary: {}\n", report.config.bin));
    out.push_str(&format!("  workdir: {}\n", report.config.workdir));
    out.push_str(&format!("  runtime_addr: {}\n", report.config.addr));
    if let Some(out_dir) = &report.config.out {
        out.push_str(&format!("  output: {}\n", out_dir.display()));
    }
    out.push('\n');
    for check in &report.checks {
        out.push_str(&format!(
            "[{}] {}: {}\n",
            service_doctor_status_label(check.status),
            check.name,
            check.message
        ));
    }
    out.push_str(&format!(
        "\nsummary: {} blocker(s), {} warning(s)\n",
        report.blocker_count(),
        report.warning_count()
    ));
    out
}

fn render_service_doctor_json(report: &ServiceDoctorReport) -> String {
    let out = report
        .config
        .out
        .as_ref()
        .map(|path| path.display().to_string())
        .unwrap_or_default();
    let checks = report
        .checks
        .iter()
        .map(|check| {
            format!(
                "{{\"status\":\"{}\",\"name\":\"{}\",\"message\":\"{}\"}}",
                service_doctor_status_label(check.status),
                json_escape(&check.name),
                json_escape(&check.message)
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    format!(
        "{{\"kind\":\"deepseek.agents.service_doctor.v1\",\"service_kind\":\"{}\",\"binary\":\"{}\",\"workdir\":\"{}\",\"runtime_addr\":\"{}\",\"output_dir\":\"{}\",\"blockers\":{},\"warnings\":{},\"checks\":[{}]}}",
        service_kind_label(report.config.kind),
        json_escape(&report.config.bin),
        json_escape(&report.config.workdir),
        json_escape(&report.config.addr),
        json_escape(&out),
        report.blocker_count(),
        report.warning_count(),
        checks
    )
}

fn service_kind_label(kind: AgentsServiceKind) -> &'static str {
    match kind {
        AgentsServiceKind::Systemd => "systemd",
        AgentsServiceKind::Launchd => "launchd",
        AgentsServiceKind::All => "all",
    }
}

fn service_doctor_status_label(status: ServiceDoctorStatus) -> &'static str {
    match status {
        ServiceDoctorStatus::Ok => "ok",
        ServiceDoctorStatus::Warn => "warn",
        ServiceDoctorStatus::Blocker => "blocker",
    }
}

#[derive(Debug, Clone)]
struct ServiceSmokeReport {
    binary: String,
    workdir: PathBuf,
    requested_addr: String,
    resolved_addr: String,
    addr_error: Option<String>,
    timeout_ms: u64,
    checks: Vec<ServiceDoctorCheck>,
}

fn run_service_smoke(args: AgentsServiceSmokeArgs) -> AppResult<()> {
    let json = args.json;
    let mut report = build_service_smoke_report(args);
    run_service_smoke_checks(&mut report);
    if json {
        println!("{}", render_service_smoke_json(&report));
    } else {
        print!("{}", render_service_smoke_text(&report));
    }
    if service_check_blocker_count(&report.checks) > 0 {
        return Err(app_error(format!(
            "service smoke found {} blocker(s)",
            service_check_blocker_count(&report.checks)
        )));
    }
    Ok(())
}

fn build_service_smoke_report(args: AgentsServiceSmokeArgs) -> ServiceSmokeReport {
    let binary = resolve_service_smoke_binary(args.bin);
    let workdir = args
        .workdir
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    let requested_addr = args.addr;
    let (resolved_addr, addr_error) = match resolve_service_smoke_addr(&requested_addr) {
        Ok(addr) => (addr, None),
        Err(error) => (requested_addr.clone(), Some(error.to_string())),
    };
    ServiceSmokeReport {
        binary,
        workdir,
        requested_addr,
        resolved_addr,
        addr_error,
        timeout_ms: args.timeout_ms.max(100),
        checks: Vec::new(),
    }
}

fn run_service_smoke_checks(report: &mut ServiceSmokeReport) {
    service_smoke_check_binary(report);
    service_smoke_check_workdir(report);
    service_smoke_check_addr(report);
    if service_check_blocker_count(&report.checks) == 0 {
        service_smoke_check_runtime(report);
        service_smoke_check_shell_supervisor(report);
    }
}

fn service_smoke_check_binary(report: &mut ServiceSmokeReport) {
    if service_value_is_path(&report.binary) {
        if Path::new(&report.binary).is_file() {
            push_service_doctor_check(
                &mut report.checks,
                ServiceDoctorStatus::Ok,
                "binary",
                format!("binary path exists: {}", report.binary),
            );
        } else {
            push_service_doctor_check(
                &mut report.checks,
                ServiceDoctorStatus::Blocker,
                "binary",
                format!("binary path is missing or not a file: {}", report.binary),
            );
        }
    } else if command_on_path(&report.binary) {
        push_service_doctor_check(
            &mut report.checks,
            ServiceDoctorStatus::Ok,
            "binary",
            format!("binary command is on PATH: {}", report.binary),
        );
    } else {
        push_service_doctor_check(
            &mut report.checks,
            ServiceDoctorStatus::Blocker,
            "binary",
            format!("binary command is not on PATH: {}", report.binary),
        );
    }
}

fn service_smoke_check_workdir(report: &mut ServiceSmokeReport) {
    if report.workdir.is_dir() {
        push_service_doctor_check(
            &mut report.checks,
            ServiceDoctorStatus::Ok,
            "workdir",
            format!("workspace directory exists: {}", report.workdir.display()),
        );
    } else {
        push_service_doctor_check(
            &mut report.checks,
            ServiceDoctorStatus::Blocker,
            "workdir",
            format!(
                "workspace directory is missing or not a directory: {}",
                report.workdir.display()
            ),
        );
    }
}

fn service_smoke_check_addr(report: &mut ServiceSmokeReport) {
    if let Some(error) = &report.addr_error {
        push_service_doctor_check(
            &mut report.checks,
            ServiceDoctorStatus::Blocker,
            "runtime_addr",
            format!(
                "failed to resolve service smoke address {}: {error}",
                report.requested_addr
            ),
        );
    } else {
        push_service_doctor_check(
            &mut report.checks,
            ServiceDoctorStatus::Ok,
            "runtime_addr",
            format!(
                "runtime smoke address resolved: {} -> {}",
                report.requested_addr, report.resolved_addr
            ),
        );
    }
}

fn service_smoke_check_runtime(report: &mut ServiceSmokeReport) {
    let timeout = Duration::from_millis(report.timeout_ms);
    let mut child = match Command::new(&report.binary)
        .arg("serve")
        .arg("--http")
        .arg("--addr")
        .arg(&report.resolved_addr)
        .arg("--once")
        .current_dir(&report.workdir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(error) => {
            push_service_doctor_check(
                &mut report.checks,
                ServiceDoctorStatus::Blocker,
                "runtime",
                format!("failed to start HTTP runtime smoke child: {error}"),
            );
            return;
        }
    };

    match wait_for_http_health(&report.resolved_addr, timeout, &mut child) {
        Ok(_) => push_service_doctor_check(
            &mut report.checks,
            ServiceDoctorStatus::Ok,
            "runtime",
            format!("HTTP runtime /health responded at {}", report.resolved_addr),
        ),
        Err(error) => push_service_doctor_check(
            &mut report.checks,
            ServiceDoctorStatus::Blocker,
            "runtime",
            format!("HTTP runtime /health smoke failed: {error}"),
        ),
    }

    match wait_child_exit(&mut child, timeout) {
        Ok(status) if status.success() => push_service_doctor_check(
            &mut report.checks,
            ServiceDoctorStatus::Ok,
            "runtime",
            format!("HTTP runtime smoke child exited successfully: {status}"),
        ),
        Ok(status) => {
            let stderr = read_child_stderr(&mut child);
            push_service_doctor_check(
                &mut report.checks,
                ServiceDoctorStatus::Blocker,
                "runtime",
                format!(
                    "HTTP runtime smoke child exited with failure: {status}{}",
                    child_stderr_suffix(&stderr)
                ),
            );
        }
        Err(error) => {
            terminate_child(&mut child);
            push_service_doctor_check(
                &mut report.checks,
                ServiceDoctorStatus::Blocker,
                "runtime",
                format!("HTTP runtime smoke child did not exit cleanly: {error}"),
            );
        }
    }
}

#[cfg(unix)]
fn service_smoke_check_shell_supervisor(report: &mut ServiceSmokeReport) {
    let timeout = Duration::from_millis(report.timeout_ms);
    let socket = service_smoke_shell_supervisor_socket_path(&report.workdir);
    if let Some(path_bytes) = unix_socket_path_too_long(&socket) {
        push_service_doctor_check(
            &mut report.checks,
            ServiceDoctorStatus::Blocker,
            "shell_supervisor",
            format!(
                "shell supervisor socket path is too long for Unix domain sockets: {} ({} bytes; limit is < {} bytes). Rerun with a shorter --workdir such as /tmp/dsc-smk",
                socket.display(),
                path_bytes,
                SHELL_SUPERVISOR_UNIX_SOCKET_MAX_BYTES
            ),
        );
        return;
    }
    if shell_supervisor_socket_is_active(&socket) {
        push_service_doctor_check(
            &mut report.checks,
            ServiceDoctorStatus::Blocker,
            "shell_supervisor",
            format!(
                "shell supervisor socket is already active at {}; rerun with an isolated --workdir",
                socket.display()
            ),
        );
        return;
    }

    let mut child = match Command::new(&report.binary)
        .arg("agents")
        .arg("shell-supervisor")
        .arg("--json")
        .current_dir(&report.workdir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(error) => {
            push_service_doctor_check(
                &mut report.checks,
                ServiceDoctorStatus::Blocker,
                "shell_supervisor",
                format!("failed to start shell supervisor smoke child: {error}"),
            );
            return;
        }
    };

    let supervisor_healthy = match wait_for_shell_supervisor_health(&socket, timeout, &mut child) {
        Ok(_) => {
            push_service_doctor_check(
                &mut report.checks,
                ServiceDoctorStatus::Ok,
                "shell_supervisor",
                format!("shell supervisor health responded at {}", socket.display()),
            );
            true
        }
        Err(error) => {
            push_service_doctor_check(
                &mut report.checks,
                ServiceDoctorStatus::Blocker,
                "shell_supervisor",
                format!("shell supervisor health smoke failed: {error}"),
            );
            false
        }
    };

    if supervisor_healthy {
        match shell_supervisor_control_smoke(&socket, report.timeout_ms) {
            Ok(summary) => push_service_doctor_check(
                &mut report.checks,
                ServiceDoctorStatus::Ok,
                "shell_supervisor_control",
                summary,
            ),
            Err(error) => push_service_doctor_check(
                &mut report.checks,
                ServiceDoctorStatus::Blocker,
                "shell_supervisor_control",
                format!("shell supervisor control smoke failed: {error}"),
            ),
        }
    }

    match shell_supervisor_request(&socket, "shutdown") {
        Ok(_) => {}
        Err(error) => push_service_doctor_check(
            &mut report.checks,
            ServiceDoctorStatus::Warn,
            "shell_supervisor",
            format!("shell supervisor shutdown request failed: {error}"),
        ),
    }

    match wait_child_exit(&mut child, timeout) {
        Ok(status) if status.success() => push_service_doctor_check(
            &mut report.checks,
            ServiceDoctorStatus::Ok,
            "shell_supervisor",
            format!("shell supervisor smoke child exited successfully: {status}"),
        ),
        Ok(status) => {
            let stderr = read_child_stderr(&mut child);
            push_service_doctor_check(
                &mut report.checks,
                ServiceDoctorStatus::Blocker,
                "shell_supervisor",
                format!(
                    "shell supervisor smoke child exited with failure: {status}{}",
                    child_stderr_suffix(&stderr)
                ),
            );
        }
        Err(error) => {
            terminate_child(&mut child);
            push_service_doctor_check(
                &mut report.checks,
                ServiceDoctorStatus::Blocker,
                "shell_supervisor",
                format!("shell supervisor smoke child did not exit cleanly: {error}"),
            );
        }
    }
}

#[cfg(not(unix))]
fn service_smoke_check_shell_supervisor(report: &mut ServiceSmokeReport) {
    push_service_doctor_check(
        &mut report.checks,
        ServiceDoctorStatus::Warn,
        "shell_supervisor",
        "shell supervisor smoke is only available on Unix",
    );
}

fn resolve_service_smoke_binary(bin: Option<String>) -> String {
    let Some(bin) = bin else {
        return std::env::current_exe()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|_| "deepseek".to_string());
    };
    let path = PathBuf::from(&bin);
    if service_value_is_path(&bin) && path.is_relative() {
        if let Ok(cwd) = std::env::current_dir() {
            return cwd.join(path).display().to_string();
        }
    }
    bin
}

fn resolve_service_smoke_addr(addr: &str) -> AppResult<String> {
    use std::net::{SocketAddr, TcpListener};

    let socket = addr
        .parse::<SocketAddr>()
        .map_err(|error| app_error(format!("invalid service-smoke address `{addr}`: {error}")))?;
    if socket.port() != 0 {
        return Ok(socket.to_string());
    }
    let listener = TcpListener::bind(socket).map_err(|error| {
        app_error(format!(
            "failed to reserve service-smoke loopback address {addr}: {error}"
        ))
    })?;
    Ok(listener.local_addr()?.to_string())
}

fn wait_for_http_health(addr: &str, timeout: Duration, child: &mut Child) -> AppResult<String> {
    use std::net::TcpStream;

    let start = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            let stderr = read_child_stderr(child);
            return Err(app_error(format!(
                "HTTP runtime exited before /health responded: {status}{}",
                child_stderr_suffix(&stderr)
            )));
        }
        match TcpStream::connect(addr) {
            Ok(mut stream) => {
                stream.set_read_timeout(Some(Duration::from_millis(500)))?;
                stream.set_write_timeout(Some(Duration::from_millis(500)))?;
                stream.write_all(
                    b"GET /health HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
                )?;
                let mut response = String::new();
                stream.read_to_string(&mut response)?;
                if response.contains("200 OK") && response.contains("deepseek.runtime.health.v1") {
                    return Ok(response);
                }
                return Err(app_error("unexpected HTTP runtime /health response"));
            }
            Err(error) if start.elapsed() < timeout => {
                let _ = error;
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(error) => {
                return Err(app_error(format!(
                    "timed out waiting for HTTP runtime at {addr}: {error}"
                )));
            }
        }
    }
}

#[cfg(unix)]
fn wait_for_shell_supervisor_health(
    socket: &Path,
    timeout: Duration,
    child: &mut Child,
) -> AppResult<String> {
    let start = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            let stderr = read_child_stderr(child);
            return Err(app_error(format!(
                "shell supervisor exited before health responded: {status}{}",
                child_stderr_suffix(&stderr)
            )));
        }
        match shell_supervisor_request(socket, "health") {
            Ok(response) => return Ok(response),
            Err(error) if start.elapsed() < timeout => {
                let _ = error;
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(error) => {
                return Err(app_error(format!(
                    "timed out waiting for shell supervisor at {}: {error}",
                    socket.display()
                )));
            }
        }
    }
}

#[cfg(unix)]
fn shell_supervisor_socket_is_active(socket: &Path) -> bool {
    shell_supervisor_request(socket, "health").is_ok()
}

#[cfg(all(unix, target_os = "linux"))]
const SHELL_SUPERVISOR_UNIX_SOCKET_MAX_BYTES: usize = 108;

#[cfg(all(unix, not(target_os = "linux")))]
const SHELL_SUPERVISOR_UNIX_SOCKET_MAX_BYTES: usize = 100;

#[cfg(unix)]
fn service_smoke_shell_supervisor_socket_path(workdir: &Path) -> PathBuf {
    let base = workdir.canonicalize().unwrap_or_else(|_| {
        if workdir.is_absolute() {
            workdir.to_path_buf()
        } else {
            std::env::current_dir()
                .map(|cwd| cwd.join(workdir))
                .unwrap_or_else(|_| workdir.to_path_buf())
        }
    });
    base.join(".dscode/shell-supervisor/supervisor.sock")
}

#[cfg(unix)]
fn unix_socket_path_too_long(path: &Path) -> Option<usize> {
    use std::os::unix::ffi::OsStrExt;

    let path_bytes = path.as_os_str().as_bytes().len();
    (path_bytes >= SHELL_SUPERVISOR_UNIX_SOCKET_MAX_BYTES).then_some(path_bytes)
}

#[cfg(unix)]
fn shell_supervisor_request(socket: &Path, method: &str) -> AppResult<String> {
    let request = format!("{{\"method\":\"{}\"}}\n", json_escape(method));
    shell_supervisor_request_raw(socket, method, &request)
}

#[cfg(unix)]
fn shell_supervisor_request_raw(socket: &Path, method: &str, request: &str) -> AppResult<String> {
    use std::os::unix::net::UnixStream;

    let mut stream = UnixStream::connect(socket)?;
    stream.set_read_timeout(Some(Duration::from_millis(500)))?;
    stream.set_write_timeout(Some(Duration::from_millis(500)))?;
    stream.write_all(request.as_bytes())?;
    let mut reader = BufReader::new(stream);
    let mut response = String::new();
    reader.read_line(&mut response)?;
    if response.contains("\"status\":\"ok\"")
        && response.contains(&format!("\"method\":\"{method}\""))
    {
        Ok(response)
    } else {
        Err(app_error(format!(
            "unexpected shell supervisor {method} response: {}",
            response.trim()
        )))
    }
}

#[cfg(unix)]
fn shell_supervisor_control_smoke(socket: &Path, timeout_ms: u64) -> AppResult<String> {
    let tty = cfg!(all(unix, target_os = "linux"));
    let start_request = format!(
        "{{\"method\":\"start\",\"arguments\":{{\"command\":\"echo deepseek-shell-supervisor-smoke\",\"tty\":{},\"tty_rows\":24,\"tty_cols\":80,\"timeout_ms\":{}}}}}\n",
        if tty { "true" } else { "false" },
        timeout_ms.min(5000)
    );
    let start_response = shell_supervisor_request_raw(socket, "start", &start_request)?;
    let task_id = shell_supervisor_response_string(&start_response, "task_id")
        .ok_or_else(|| app_error("shell supervisor start smoke response missing task_id"))?;
    if tty && !start_response.contains(r#""job_pty_backend":"native-supervisor""#) {
        return Err(app_error(
            "shell supervisor tty smoke did not start a native-supervisor PTY job",
        ));
    }

    let wait_request = format!(
        "{{\"method\":\"wait\",\"arguments\":{{\"task_id\":\"{}\",\"timeout_ms\":{}}}}}\n",
        json_escape(&task_id),
        timeout_ms.min(5000)
    );
    shell_supervisor_request_raw(socket, "wait", &wait_request)?;

    let attach_request = format!(
        "{{\"method\":\"attach\",\"arguments\":{{\"task_id\":\"{}\",\"tail\":true,\"limit_bytes\":4096}}}}\n",
        json_escape(&task_id)
    );
    let attach_response = shell_supervisor_request_raw(socket, "attach", &attach_request)?;
    if !attach_response.contains("deepseek-shell-supervisor-smoke") {
        return Err(app_error(
            "shell supervisor attach smoke did not replay command output",
        ));
    }
    Ok(format!(
        "shell supervisor start/wait/attach control smoke passed for task {task_id} (tty={tty})"
    ))
}

#[cfg(unix)]
fn shell_supervisor_response_string(response: &str, key: &str) -> Option<String> {
    let value = parse_json_value(response.trim()).ok()?;
    let object = json_as_object(&value)?;
    json_as_string(object.get(key)?).map(str::to_string)
}

fn wait_child_exit(child: &mut Child, timeout: Duration) -> AppResult<std::process::ExitStatus> {
    let start = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        if start.elapsed() >= timeout {
            return Err(app_error("timed out waiting for child process exit"));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn terminate_child(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

fn read_child_stderr(child: &mut Child) -> String {
    let mut output = String::new();
    if let Some(stderr) = child.stderr.as_mut() {
        let _ = stderr.read_to_string(&mut output);
    }
    output.trim().to_string()
}

fn child_stderr_suffix(stderr: &str) -> String {
    if stderr.is_empty() {
        String::new()
    } else {
        format!("; stderr: {stderr}")
    }
}

fn service_check_blocker_count(checks: &[ServiceDoctorCheck]) -> usize {
    checks
        .iter()
        .filter(|check| check.status == ServiceDoctorStatus::Blocker)
        .count()
}

fn service_check_warning_count(checks: &[ServiceDoctorCheck]) -> usize {
    checks
        .iter()
        .filter(|check| check.status == ServiceDoctorStatus::Warn)
        .count()
}

fn render_service_smoke_text(report: &ServiceSmokeReport) -> String {
    let mut out = String::new();
    out.push_str("DeepSeekCode service smoke\n");
    out.push_str(&format!("  binary: {}\n", report.binary));
    out.push_str(&format!("  workdir: {}\n", report.workdir.display()));
    out.push_str(&format!("  requested_addr: {}\n", report.requested_addr));
    out.push_str(&format!("  resolved_addr: {}\n", report.resolved_addr));
    out.push_str(&format!("  timeout_ms: {}\n\n", report.timeout_ms));
    for check in &report.checks {
        out.push_str(&format!(
            "[{}] {}: {}\n",
            service_doctor_status_label(check.status),
            check.name,
            check.message
        ));
    }
    out.push_str(&format!(
        "\nsummary: {} blocker(s), {} warning(s)\n",
        service_check_blocker_count(&report.checks),
        service_check_warning_count(&report.checks)
    ));
    out
}

fn render_service_smoke_json(report: &ServiceSmokeReport) -> String {
    let checks = report
        .checks
        .iter()
        .map(|check| {
            format!(
                "{{\"status\":\"{}\",\"name\":\"{}\",\"message\":\"{}\"}}",
                service_doctor_status_label(check.status),
                json_escape(&check.name),
                json_escape(&check.message)
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    format!(
        "{{\"kind\":\"deepseek.agents.service_smoke.v1\",\"binary\":\"{}\",\"workdir\":\"{}\",\"requested_addr\":\"{}\",\"resolved_addr\":\"{}\",\"timeout_ms\":{},\"blockers\":{},\"warnings\":{},\"checks\":[{}]}}",
        json_escape(&report.binary),
        json_escape(&report.workdir.display().to_string()),
        json_escape(&report.requested_addr),
        json_escape(&report.resolved_addr),
        report.timeout_ms,
        service_check_blocker_count(&report.checks),
        service_check_warning_count(&report.checks),
        checks
    )
}

fn run_runtime_daemon_tick(
    config: &AppConfig,
    store: &RuntimeStore,
    budget: Option<usize>,
    json: bool,
) -> AppResult<RuntimeDaemonTick> {
    let mut tick = RuntimeDaemonTick::default();
    let now = current_epoch_seconds();

    for automation in store.list_automations(None, None, 1_000)? {
        if !automation_is_due(&automation, now) {
            continue;
        }
        match store.trigger_automation(&automation.id, None) {
            Ok((updated, task)) => {
                tick.triggered_automations += 1;
                let next_run_at = next_run_for_schedule(&updated.schedule, now);
                let updated = store.update_automation_next_run(&updated.id, next_run_at)?;
                if json {
                    println!(
                        "{}",
                        json_value_to_string(&daemon_automation_event(&updated, &task))
                    );
                }
            }
            Err(error) => {
                tick.failed_automations += 1;
                if json {
                    println!(
                        "{}",
                        json_value_to_string(&daemon_error_event(
                            "automation_failed",
                            &automation.id,
                            &error.to_string(),
                        ))
                    );
                } else {
                    eprintln!("automation {} failed: {error}", automation.id);
                }
            }
        }
    }

    run_runtime_daemon_rlm_recovery(config, json, &mut tick)?;
    run_runtime_daemon_rlm_live_turn(config, store, json, &mut tick)?;

    let mut pending = store
        .list_tasks(None, None, 1_000)?
        .into_iter()
        .filter(|task| {
            task.status == "pending" && task.thread_id.is_some() && task.kind != "rlm_process"
        })
        .collect::<Vec<_>>();
    pending.sort_by(|a, b| {
        a.created_at
            .cmp(&b.created_at)
            .then_with(|| a.id.cmp(&b.id))
    });

    for task in pending.into_iter().take(1) {
        match run_runtime_task(config.clone(), &task.id, budget, json) {
            Ok(()) => tick.executed_tasks += 1,
            Err(error) => {
                tick.failed_tasks += 1;
                if json {
                    println!(
                        "{}",
                        json_value_to_string(&daemon_error_event(
                            "task_failed",
                            &task.id,
                            &error.to_string(),
                        ))
                    );
                } else {
                    eprintln!("task {} failed: {error}", task.id);
                }
            }
        }
    }

    run_runtime_daemon_compactions(config, store, json, &mut tick)?;

    Ok(tick)
}

fn run_runtime_daemon_rlm_recovery(
    config: &AppConfig,
    json: bool,
    tick: &mut RuntimeDaemonTick,
) -> AppResult<()> {
    let output = RlmLiveRecoverTool {
        config: config.clone(),
    }
    .execute(
        ToolInput::new()
            .with_arg("all", "true")
            .with_arg("reason", "runtime daemon stale live RLM owner recovery"),
    );
    match output {
        Ok(output) => {
            let recovered_count = parse_json_value(&output.summary)
                .ok()
                .and_then(|value| match value {
                    JsonValue::Object(root) => root.get("recovered_count").and_then(json_as_u64),
                    _ => None,
                })
                .unwrap_or(0) as usize;
            tick.recovered_rlm_turns += recovered_count;
            if json && recovered_count > 0 {
                println!(
                    "{}",
                    json_value_to_string(&daemon_rlm_recovery_event(
                        recovered_count,
                        Some(&output.summary),
                    ))
                );
            }
        }
        Err(error) => {
            tick.failed_rlm_recoveries += 1;
            if json {
                println!(
                    "{}",
                    json_value_to_string(&daemon_error_event(
                        "rlm_recovery_failed",
                        "all",
                        &error.to_string(),
                    ))
                );
            } else {
                eprintln!("live RLM recovery failed: {error}");
            }
        }
    }
    Ok(())
}

fn run_runtime_daemon_rlm_live_turn(
    config: &AppConfig,
    store: &RuntimeStore,
    json: bool,
    tick: &mut RuntimeDaemonTick,
) -> AppResult<()> {
    let sessions_by_thread = rlm_live_session_ids_by_runtime_thread(config)?;
    if sessions_by_thread.is_empty() {
        return Ok(());
    }
    let mut pending = store
        .list_tasks(None, None, 1_000)?
        .into_iter()
        .filter(|task| {
            task.kind == "rlm_process"
                && task.status == "pending"
                && task
                    .thread_id
                    .as_ref()
                    .is_some_and(|thread_id| sessions_by_thread.contains_key(thread_id))
        })
        .collect::<Vec<_>>();
    pending.sort_by(|a, b| {
        a.created_at
            .cmp(&b.created_at)
            .then_with(|| a.id.cmp(&b.id))
    });
    let Some(task) = pending.into_iter().next() else {
        return Ok(());
    };
    let Some(thread_id) = task.thread_id.as_deref() else {
        return Ok(());
    };
    let Some(session_id) = sessions_by_thread.get(thread_id).cloned() else {
        return Ok(());
    };

    let output = RlmLiveRunNextTool {
        config: config.clone(),
        parent_depth: 0,
    }
    .execute(
        ToolInput::new()
            .with_arg("session_id", session_id.clone())
            .with_arg("task_id", task.id.clone()),
    );
    match output {
        Ok(output) => {
            tick.executed_rlm_turns += 1;
            if json {
                println!(
                    "{}",
                    json_value_to_string(&daemon_rlm_turn_event(
                        "rlm_turn_completed",
                        &session_id,
                        &task.id,
                        Some(&output.summary),
                    ))
                );
            }
        }
        Err(error) => {
            tick.failed_rlm_turns += 1;
            if json {
                println!(
                    "{}",
                    json_value_to_string(&daemon_rlm_turn_event(
                        "rlm_turn_failed",
                        &session_id,
                        &task.id,
                        Some(&error.to_string()),
                    ))
                );
            } else {
                eprintln!("live RLM turn {} failed: {error}", task.id);
            }
        }
    }
    Ok(())
}

fn run_runtime_daemon_compactions(
    config: &AppConfig,
    store: &RuntimeStore,
    json: bool,
    tick: &mut RuntimeDaemonTick,
) -> AppResult<()> {
    run_runtime_daemon_compactions_with_summary_provider(store, json, tick, |store, thread| {
        automatic_model_compaction_summary(config, store, thread, DAEMON_COMPACTION_KEEP_TAIL_TURNS)
    })
}

fn run_runtime_daemon_compactions_with_summary_provider<F>(
    store: &RuntimeStore,
    json: bool,
    tick: &mut RuntimeDaemonTick,
    mut summary_provider: F,
) -> AppResult<()>
where
    F: FnMut(&RuntimeStore, &ThreadRecord) -> AppResult<Option<String>>,
{
    for thread in store.list_threads(1_000)? {
        if !thread_needs_compaction(store, &thread)? {
            continue;
        }
        let model_summary = match summary_provider(store, &thread) {
            Ok(summary) => summary,
            Err(error) => {
                if json {
                    println!(
                        "{}",
                        json_value_to_string(&daemon_error_event(
                            "compaction_summary_failed",
                            &thread.id,
                            &error.to_string(),
                        ))
                    );
                } else {
                    eprintln!("compaction summary {} failed: {error}", thread.id);
                }
                None
            }
        };
        let result = if let Some(summary) = model_summary {
            store.compact_thread_with_summary_source(
                &thread.id,
                DAEMON_COMPACTION_KEEP_TAIL_TURNS,
                summary,
                "model",
            )
        } else {
            store.compact_thread(&thread.id, DAEMON_COMPACTION_KEEP_TAIL_TURNS, None)
        };
        match result {
            Ok(compaction) => {
                tick.compacted_threads += 1;
                if json {
                    println!(
                        "{}",
                        json_value_to_string(&daemon_compaction_event(&compaction))
                    );
                }
            }
            Err(error) => {
                tick.failed_compactions += 1;
                if json {
                    println!(
                        "{}",
                        json_value_to_string(&daemon_error_event(
                            "compaction_failed",
                            &thread.id,
                            &error.to_string(),
                        ))
                    );
                } else {
                    eprintln!("compaction {} failed: {error}", thread.id);
                }
            }
        }
    }
    Ok(())
}

fn automatic_model_compaction_summary(
    config: &AppConfig,
    store: &RuntimeStore,
    thread: &ThreadRecord,
    keep_tail_turns: usize,
) -> AppResult<Option<String>> {
    if !model_api_key_configured(&config.model.api_key_env) {
        return Ok(None);
    }
    let turns = store.list_turns(&thread.id)?;
    if turns.len() <= keep_tail_turns {
        return Ok(None);
    }
    let client = DeepSeekClient {
        config: config.model.clone(),
    };
    model_compaction_summary_with_client(&client, thread, &turns, keep_tail_turns).map(Some)
}

fn model_api_key_configured(api_key_env: &str) -> bool {
    std::env::var(api_key_env)
        .ok()
        .map(|value| !value.trim().is_empty())
        .unwrap_or(false)
}

fn model_compaction_summary_with_client<C: ModelClient>(
    client: &C,
    thread: &ThreadRecord,
    turns: &[TurnRecord],
    keep_tail_turns: usize,
) -> AppResult<String> {
    if keep_tail_turns >= turns.len() {
        return Err(app_error(
            "model compaction requires at least one older turn to summarize",
        ));
    }
    let request = build_model_compaction_request(thread, turns, keep_tail_turns);
    let mut events = crate::ui::stream::NoopStreamEvents;
    let (response, _usage) = client.respond(request, &mut events)?;
    if !matches!(response.action, ModelAction::Finish) {
        return Err(app_error(
            "model compaction summary returned a tool call; expected final summary text",
        ));
    }
    let summary = response.message.trim();
    if summary.is_empty() {
        return Err(app_error("model compaction summary was empty"));
    }
    Ok(summary.to_string())
}

fn build_model_compaction_request(
    thread: &ThreadRecord,
    turns: &[TurnRecord],
    keep_tail_turns: usize,
) -> ModelRequest {
    ModelRequest {
        system_prompt: "Summarize older durable runtime context for automatic compaction. Return only the summary text. Preserve user intent, decisions, constraints, changed files, tool outcomes, unresolved tasks, and anything the next assistant turn must remember. Be concise and actionable.".to_string(),
        task: render_model_compaction_task(thread, turns, keep_tail_turns),
        image_inputs: Vec::new(),
        profile_name: "runtime-compaction".to_string(),
        profile_hints: vec![
            "No tools are available.".to_string(),
            "Write a durable context summary, not a user-facing answer.".to_string(),
        ],
        primary_file: None,
        suggested_test_command: None,
        available_tools: Vec::new(),
        observations: Vec::new(),
        todos: Vec::new(),
        planning_mode: false,
        recent_steps: Vec::new(),
    }
}

fn render_model_compaction_task(
    thread: &ThreadRecord,
    turns: &[TurnRecord],
    keep_tail_turns: usize,
) -> String {
    const MAX_SUMMARIZED_TURNS: usize = 32;
    const MAX_TURN_CHARS: usize = 700;

    let split_at = turns.len().saturating_sub(keep_tail_turns);
    let summarized_turns = &turns[..split_at];
    let kept_turns = &turns[split_at..];
    let omitted = summarized_turns.len().saturating_sub(MAX_SUMMARIZED_TURNS);
    let summarized_window = summarized_turns.iter().skip(omitted).collect::<Vec<_>>();

    let mut task = String::new();
    task.push_str("Create a compact durable summary for older turns in this runtime thread.\n");
    task.push_str("Thread title: ");
    task.push_str(&thread.title);
    task.push('\n');
    task.push_str("Thread id: ");
    task.push_str(&thread.id);
    task.push('\n');
    task.push_str("Older turns to summarize: ");
    task.push_str(&summarized_turns.len().to_string());
    task.push('\n');
    task.push_str("Tail turns preserved verbatim: ");
    task.push_str(&kept_turns.len().to_string());
    task.push_str("\n\n");
    if omitted > 0 {
        task.push_str("The oldest ");
        task.push_str(&omitted.to_string());
        task.push_str(" summarized turn(s) were omitted from this bounded summary prompt.\n\n");
    }
    task.push_str("Summarized turn window:\n");
    for turn in summarized_window {
        task.push_str("- #");
        task.push_str(&turn.index.to_string());
        task.push(' ');
        task.push_str(&turn.role);
        task.push_str(" turn_id=");
        task.push_str(&turn.id);
        task.push_str(": ");
        task.push_str(&compaction_excerpt(&turn.content, MAX_TURN_CHARS));
        task.push('\n');
    }
    if let Some(first_kept) = kept_turns.first() {
        task.push_str("\nThe live tail begins at turn #");
        task.push_str(&first_kept.index.to_string());
        task.push_str(" (turn_id=");
        task.push_str(&first_kept.id);
        task.push_str(
            "). Do not restate the tail verbatim; summarize only what older turns establish.\n",
        );
    }
    task
}

fn compaction_excerpt(content: &str, max_chars: usize) -> String {
    let normalized = content.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut excerpt = normalized.chars().take(max_chars).collect::<String>();
    if normalized.chars().count() > max_chars {
        excerpt.push_str("...");
    }
    excerpt
}

fn thread_needs_compaction(store: &RuntimeStore, thread: &ThreadRecord) -> AppResult<bool> {
    let Some(latest_usage) = store.list_usage(Some(&thread.id), 1)?.into_iter().next() else {
        return Ok(false);
    };
    if latest_usage.total_tokens < DAEMON_COMPACTION_THRESHOLD_TOKENS {
        return Ok(false);
    }
    if store.list_turns(&thread.id)?.len() <= DAEMON_COMPACTION_KEEP_TAIL_TURNS {
        return Ok(false);
    }

    let events = store.read_events(&thread.id, 0)?;
    let last_usage_seq = events
        .iter()
        .filter(|event| event.kind == "usage_recorded")
        .map(|event| event.seq)
        .max()
        .unwrap_or(0);
    let last_compaction_seq = events
        .iter()
        .filter(|event| event.kind == "thread_compacted")
        .map(|event| event.seq)
        .max()
        .unwrap_or(0);
    Ok(last_usage_seq > last_compaction_seq)
}

fn run_runtime_task_loop(
    config: &AppConfig,
    store: &RuntimeStore,
    task: &TaskRecord,
    thread: &ThreadRecord,
    budget: Option<usize>,
    json: bool,
) -> AppResult<RunResult> {
    let agent = AgentLoop::new(config.clone());
    let approval_resolver: SharedAgentApprovalResolver =
        Rc::new(RefCell::new(RuntimeTaskApprovalResolver {
            store: store.clone(),
            thread_id: thread.id.clone(),
            poll_interval: Duration::from_millis(250),
            max_polls: None,
        }));
    let user_input_resolver: SharedAgentUserInputResolver =
        Rc::new(RefCell::new(RuntimeTaskUserInputResolver {
            store: store.clone(),
            thread_id: thread.id.clone(),
            poll_interval: Duration::from_millis(250),
            max_polls: None,
        }));
    let options = AgentLoopOptions {
        steps: budget.unwrap_or_else(|| AgentLoopOptions::default().steps),
        initial_recent_steps: store.recent_reasoning_replay_entries(&thread.id, 3)?,
        emit_progress: !json,
        persist_session: false,
        approval_resolver: Some(approval_resolver),
        user_input_resolver: Some(user_input_resolver),
        ..AgentLoopOptions::default()
    };
    agent.run_with(TaskContext::new(task.summary.clone(), None), options)
}

struct RuntimeTaskApprovalResolver {
    store: RuntimeStore,
    thread_id: String,
    poll_interval: Duration,
    max_polls: Option<usize>,
}

impl AgentApprovalResolver for RuntimeTaskApprovalResolver {
    fn resolve(&mut self, request: &AgentApprovalRequest) -> AppResult<AgentApprovalDecision> {
        let approval = self.store.append_permission_request(
            &self.thread_id,
            None,
            request.tool_name.clone(),
            request.kind.clone(),
            request.target.clone(),
            request.input.clone(),
        )?;
        let mut polls = 0_usize;
        loop {
            for event in self.store.read_events(&self.thread_id, approval.seq)? {
                if let Some(decision) = approval_response_decision(&event, &approval.id) {
                    return Ok(decision);
                }
            }
            polls = polls.saturating_add(1);
            if self.max_polls.is_some_and(|max_polls| polls >= max_polls) {
                return Err(app_error(format!(
                    "timed out waiting for permission response {}",
                    approval.id
                )));
            }
            std::thread::sleep(self.poll_interval);
        }
    }
}

struct RuntimeTaskUserInputResolver {
    store: RuntimeStore,
    thread_id: String,
    poll_interval: Duration,
    max_polls: Option<usize>,
}

impl AgentUserInputResolver for RuntimeTaskUserInputResolver {
    fn resolve(&mut self, request: &AgentUserInputRequest) -> AppResult<AgentUserInputResponse> {
        let raw_questions = request
            .input
            .get("questions")
            .ok_or_else(|| app_error("request_user_input requires `questions`"))?;
        let questions = parse_json_value(raw_questions.trim())
            .map_err(|error| app_error(format!("Invalid request_user_input payload: {error}")))?;
        let user_input = self
            .store
            .append_user_input_request(&self.thread_id, None, questions)?;
        let mut polls = 0_usize;
        loop {
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
            std::thread::sleep(self.poll_interval);
        }
    }
}

fn user_input_response_answers(
    event: &crate::core::runtime::RuntimeEvent,
    request_id: &str,
) -> Option<std::collections::BTreeMap<String, String>> {
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
        .collect::<std::collections::BTreeMap<_, _>>();
    if answers.is_empty() {
        None
    } else {
        Some(answers)
    }
}

fn approval_response_decision(
    event: &crate::core::runtime::RuntimeEvent,
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

fn record_runtime_task_result(
    store: &RuntimeStore,
    task: &TaskRecord,
    thread: &ThreadRecord,
    result: &RunResult,
) -> AppResult<String> {
    let user = store.append_turn(&thread.id, "user".to_string(), task.summary.clone())?;
    store.append_item(
        &thread.id,
        Some(&user.id),
        "message".to_string(),
        Some("user".to_string()),
        task.summary.clone(),
        "completed".to_string(),
    )?;
    let message = non_empty_message(&result.final_message);
    let assistant = store.append_turn(&thread.id, "assistant".to_string(), message.clone())?;
    store.append_item(
        &thread.id,
        Some(&assistant.id),
        "message".to_string(),
        Some("assistant".to_string()),
        message.clone(),
        "completed".to_string(),
    )?;
    for event in &result.tool_events {
        store.append_item(
            &thread.id,
            Some(&assistant.id),
            "tool_result".to_string(),
            Some("tool".to_string()),
            format_tool_event(event),
            tool_item_status(event),
        )?;
    }
    let usage_model = result.usage.model.as_deref().unwrap_or(&thread.model);
    store.append_usage_with_cache(
        &thread.id,
        Some(&assistant.id),
        usage_model.to_string(),
        "runtime_runner".to_string(),
        result.usage.prompt,
        result.usage.completion,
        result.usage.prompt_cache_hit,
        result.usage.prompt_cache_miss,
    )?;
    store.update_task(&task.id, "completed".to_string(), message)?;
    Ok(assistant.id)
}

fn record_runtime_task_failure(
    store: &RuntimeStore,
    task: &TaskRecord,
    thread: &ThreadRecord,
    error: &str,
) -> AppResult<()> {
    let message = format!("runtime task failed: {error}");
    let assistant = store.append_turn(&thread.id, "assistant".to_string(), message.clone())?;
    store.append_item(
        &thread.id,
        Some(&assistant.id),
        "message".to_string(),
        Some("assistant".to_string()),
        message.clone(),
        "failed".to_string(),
    )?;
    store.update_task(&task.id, "failed".to_string(), message)?;
    Ok(())
}

fn non_empty_message(message: &str) -> String {
    if message.trim().is_empty() {
        "runtime task completed without assistant output".to_string()
    } else {
        message.to_string()
    }
}

fn format_tool_event(event: &ToolEvent) -> String {
    let args = event
        .input
        .iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect::<Vec<_>>()
        .join(" ");
    if args.is_empty() {
        format!("tool: {}\n{}", event.tool_name, event.output)
    } else {
        format!(
            "tool: {}\nargs: {}\n{}",
            event.tool_name, args, event.output
        )
    }
}

fn tool_item_status(event: &ToolEvent) -> String {
    match event.status {
        ObservationStatus::Ok => "completed".to_string(),
        ObservationStatus::Failed => "failed".to_string(),
    }
}

fn runner_event(
    event_type: &str,
    task_id: &str,
    thread_id: &str,
    runner_id: Option<&str>,
    message: Option<&str>,
) -> JsonValue {
    let mut root = std::collections::BTreeMap::new();
    root.insert(
        "type".to_string(),
        JsonValue::String(event_type.to_string()),
    );
    root.insert(
        "task_id".to_string(),
        JsonValue::String(task_id.to_string()),
    );
    root.insert(
        "thread_id".to_string(),
        JsonValue::String(thread_id.to_string()),
    );
    if let Some(runner_id) = runner_id {
        root.insert(
            "runner_id".to_string(),
            JsonValue::String(runner_id.to_string()),
        );
    }
    if let Some(message) = message {
        root.insert(
            "message".to_string(),
            JsonValue::String(message.to_string()),
        );
    }
    JsonValue::Object(root)
}

fn daemon_tick_event(tick: &RuntimeDaemonTick) -> JsonValue {
    let mut root = std::collections::BTreeMap::new();
    root.insert(
        "type".to_string(),
        JsonValue::String("daemon_tick".to_string()),
    );
    root.insert(
        "triggered_automations".to_string(),
        JsonValue::Number(tick.triggered_automations.to_string()),
    );
    root.insert(
        "executed_tasks".to_string(),
        JsonValue::Number(tick.executed_tasks.to_string()),
    );
    root.insert(
        "executed_rlm_turns".to_string(),
        JsonValue::Number(tick.executed_rlm_turns.to_string()),
    );
    root.insert(
        "recovered_rlm_turns".to_string(),
        JsonValue::Number(tick.recovered_rlm_turns.to_string()),
    );
    root.insert(
        "compacted_threads".to_string(),
        JsonValue::Number(tick.compacted_threads.to_string()),
    );
    root.insert(
        "failed_automations".to_string(),
        JsonValue::Number(tick.failed_automations.to_string()),
    );
    root.insert(
        "failed_tasks".to_string(),
        JsonValue::Number(tick.failed_tasks.to_string()),
    );
    root.insert(
        "failed_rlm_turns".to_string(),
        JsonValue::Number(tick.failed_rlm_turns.to_string()),
    );
    root.insert(
        "failed_rlm_recoveries".to_string(),
        JsonValue::Number(tick.failed_rlm_recoveries.to_string()),
    );
    root.insert(
        "failed_compactions".to_string(),
        JsonValue::Number(tick.failed_compactions.to_string()),
    );
    JsonValue::Object(root)
}

fn daemon_rlm_recovery_event(recovered_count: usize, summary: Option<&str>) -> JsonValue {
    let mut root = std::collections::BTreeMap::new();
    root.insert(
        "type".to_string(),
        JsonValue::String("rlm_recovery_completed".to_string()),
    );
    root.insert(
        "recovered_count".to_string(),
        JsonValue::Number(recovered_count.to_string()),
    );
    root.insert(
        "summary".to_string(),
        summary
            .map(|value| JsonValue::String(value.to_string()))
            .unwrap_or(JsonValue::Null),
    );
    JsonValue::Object(root)
}

fn daemon_rlm_turn_event(
    event_type: &str,
    session_id: &str,
    task_id: &str,
    summary: Option<&str>,
) -> JsonValue {
    let mut root = std::collections::BTreeMap::new();
    root.insert(
        "type".to_string(),
        JsonValue::String(event_type.to_string()),
    );
    root.insert(
        "session_id".to_string(),
        JsonValue::String(session_id.to_string()),
    );
    root.insert(
        "task_id".to_string(),
        JsonValue::String(task_id.to_string()),
    );
    root.insert(
        "summary".to_string(),
        summary
            .map(|value| JsonValue::String(value.to_string()))
            .unwrap_or(JsonValue::Null),
    );
    JsonValue::Object(root)
}

fn daemon_compaction_event(compaction: &ThreadCompactionRecord) -> JsonValue {
    let mut root = std::collections::BTreeMap::new();
    root.insert(
        "type".to_string(),
        JsonValue::String("thread_compacted".to_string()),
    );
    root.insert(
        "thread_id".to_string(),
        JsonValue::String(compaction.thread_id.clone()),
    );
    root.insert(
        "summary_turn_id".to_string(),
        JsonValue::String(compaction.summary_turn.id.clone()),
    );
    root.insert(
        "summarized_turn_count".to_string(),
        JsonValue::Number(compaction.summarized_turn_count.to_string()),
    );
    root.insert(
        "keep_tail_turns".to_string(),
        JsonValue::Number(compaction.keep_tail_turns.to_string()),
    );
    root.insert(
        "summary_source".to_string(),
        JsonValue::String(compaction.summary_source.clone()),
    );
    JsonValue::Object(root)
}

fn daemon_automation_event(automation: &AutomationRecord, task: &TaskRecord) -> JsonValue {
    let mut root = std::collections::BTreeMap::new();
    root.insert(
        "type".to_string(),
        JsonValue::String("automation_triggered".to_string()),
    );
    root.insert(
        "automation_id".to_string(),
        JsonValue::String(automation.id.clone()),
    );
    root.insert("task_id".to_string(), JsonValue::String(task.id.clone()));
    root.insert(
        "next_run_at".to_string(),
        automation
            .next_run_at
            .as_ref()
            .map(|value| JsonValue::String(value.clone()))
            .unwrap_or(JsonValue::Null),
    );
    JsonValue::Object(root)
}

fn daemon_error_event(event_type: &str, id: &str, message: &str) -> JsonValue {
    let mut root = std::collections::BTreeMap::new();
    root.insert(
        "type".to_string(),
        JsonValue::String(event_type.to_string()),
    );
    root.insert("id".to_string(), JsonValue::String(id.to_string()));
    root.insert(
        "message".to_string(),
        JsonValue::String(message.to_string()),
    );
    JsonValue::Object(root)
}

fn automation_is_due(automation: &AutomationRecord, now_secs: u64) -> bool {
    automation.status == "active"
        && automation
            .next_run_at
            .as_deref()
            .and_then(parse_epoch_seconds)
            .is_some_and(|next_run_at| next_run_at <= now_secs)
}

fn next_run_for_schedule(schedule: &str, now_secs: u64) -> Option<String> {
    parse_schedule_interval_seconds(schedule)
        .map(|interval| format_epoch_seconds(now_secs.saturating_add(interval)))
}

fn parse_schedule_interval_seconds(schedule: &str) -> Option<u64> {
    let lower = schedule.trim().to_ascii_lowercase();
    if matches!(lower.as_str(), "" | "manual" | "once" | "@once") {
        return None;
    }
    let token = lower
        .strip_prefix("@every ")
        .or_else(|| lower.strip_prefix("every "))
        .or_else(|| lower.strip_prefix("every:"))
        .or_else(|| lower.strip_prefix("interval "))
        .or_else(|| lower.strip_prefix("interval:"))
        .unwrap_or(&lower)
        .split_whitespace()
        .next()
        .unwrap_or("");
    parse_duration_seconds(token)
}

fn parse_duration_seconds(token: &str) -> Option<u64> {
    let split = token
        .char_indices()
        .find(|(_, ch)| !ch.is_ascii_digit())
        .map(|(index, _)| index)
        .unwrap_or(token.len());
    let (digits, unit) = token.split_at(split);
    let value = digits.parse::<u64>().ok()?;
    if value == 0 {
        return None;
    }
    let multiplier = match unit {
        "" | "s" | "sec" | "secs" | "second" | "seconds" => 1,
        "m" | "min" | "mins" | "minute" | "minutes" => 60,
        "h" | "hr" | "hrs" | "hour" | "hours" => 60 * 60,
        "d" | "day" | "days" => 24 * 60 * 60,
        _ => return None,
    };
    value.checked_mul(multiplier)
}

fn parse_epoch_seconds(value: &str) -> Option<u64> {
    value
        .strip_prefix("epoch+")
        .unwrap_or(value)
        .parse::<u64>()
        .ok()
}

fn format_epoch_seconds(value: u64) -> String {
    format!("epoch+{value}")
}

fn current_epoch_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

fn list_threads(config_dir: &str) -> AppResult<()> {
    let dir = agent_threads_dir(config_dir);
    let active = read_active_thread(config_dir).unwrap_or_default();
    let Ok(entries) = std::fs::read_dir(&dir) else {
        println!("No subagent threads recorded.");
        return Ok(());
    };
    let mut threads = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|value| value.to_str()) == Some("md"))
        .collect::<Vec<_>>();
    threads.sort();
    if threads.is_empty() {
        println!("No subagent threads recorded.");
        return Ok(());
    }

    println!("Subagent threads:");
    for path in threads {
        let id = path
            .file_stem()
            .and_then(|value| value.to_str())
            .unwrap_or("-");
        let marker = if id == active { "*" } else { "-" };
        let title = std::fs::read_to_string(&path)
            .ok()
            .and_then(|body| body.lines().next().map(str::to_string))
            .unwrap_or_else(|| "# Agent Thread".to_string());
        println!("{marker} {id}: {}", title.trim_start_matches("# "));
    }
    Ok(())
}

fn show_thread(config_dir: &str, id: &str) -> AppResult<()> {
    let path = valid_thread_path(config_dir, id)?;
    let body = std::fs::read_to_string(&path)
        .map_err(|error| app_error(format!("failed to read thread {}: {error}", path.display())))?;
    println!("{body}");
    Ok(())
}

fn switch_thread(config_dir: &str, id: &str) -> AppResult<()> {
    let path = valid_thread_path(config_dir, id)?;
    let active = active_agent_thread_path(config_dir);
    if let Some(parent) = active.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&active, id)?;
    println!("active subagent thread: {id}");
    println!("source: {}", path.display());
    Ok(())
}

fn current_thread(config_dir: &str) -> AppResult<()> {
    match read_active_thread(config_dir) {
        Some(id) => {
            println!("active subagent thread: {id}");
            if let Some(path) = thread_file_path(config_dir, &id) {
                println!("source: {}", path.display());
            }
        }
        None => println!("No active subagent thread."),
    }
    Ok(())
}

fn clear_thread(config_dir: &str) -> AppResult<()> {
    let active = active_agent_thread_path(config_dir);
    if active.exists() {
        std::fs::remove_file(&active)?;
    }
    println!("active subagent thread cleared");
    Ok(())
}

fn valid_thread_path(config_dir: &str, id: &str) -> AppResult<std::path::PathBuf> {
    if !validate_thread_id(id) {
        return Err(app_error("invalid subagent thread id"));
    }
    let path =
        thread_file_path(config_dir, id).ok_or_else(|| app_error("invalid subagent thread id"))?;
    if !path.is_file() {
        return Err(app_error(format!("subagent thread `{id}` not found")));
    }
    Ok(path)
}

fn read_active_thread(config_dir: &str) -> Option<String> {
    std::fs::read_to_string(active_agent_thread_path(config_dir))
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| validate_thread_id(value))
}

#[allow(dead_code)]
fn only_valid(results: Vec<AgentLoadResult>) -> Vec<crate::core::agents::AgentSpec> {
    results.into_iter().filter_map(Result::ok).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_valid_filters_invalid_results() {
        let results = vec![
            Ok(crate::core::agents::AgentSpec {
                name: "reviewer".to_string(),
                description: "Reviews code".to_string(),
                tools: Vec::new(),
                model: None,
                prompt: "Review.".to_string(),
                path: ".dscode/agents/reviewer.md".into(),
                source: AgentSource::Project,
            }),
            Err(crate::core::agents::AgentLoadError {
                path: ".dscode/agents/bad.md".into(),
                message: "bad".to_string(),
            }),
        ];

        let valid = only_valid(results);

        assert_eq!(valid.len(), 1);
        assert_eq!(valid[0].name, "reviewer");
    }

    #[test]
    fn agents_shell_cli_args_build_protocol_requests() {
        let start = agents_shell_request_json(&AgentsShellArgs {
            action: AgentsShellAction::Start {
                command: "echo hello".to_string(),
                cwd: Some("subdir".to_string()),
                tty: true,
                tty_rows: Some(33),
                tty_cols: Some(101),
            },
            json: true,
        });
        let start = json_as_object(&start).unwrap();
        assert_eq!(start.get("method").and_then(json_as_string), Some("start"));
        assert_eq!(
            start.get("command").and_then(json_as_string),
            Some("echo hello")
        );
        assert_eq!(start.get("cwd").and_then(json_as_string), Some("subdir"));
        assert!(matches!(start.get("tty"), Some(JsonValue::Bool(true))));
        assert_eq!(start.get("tty_rows").and_then(json_as_u64), Some(33));
        assert_eq!(start.get("tty_cols").and_then(json_as_u64), Some(101));

        let stdin = agents_shell_request_json(&AgentsShellArgs {
            action: AgentsShellAction::Stdin {
                task_id: "task-1".to_string(),
                input: Some("probe\n".to_string()),
                close_stdin: true,
                timeout_ms: Some(100),
            },
            json: false,
        });
        let stdin = json_as_object(&stdin).unwrap();
        assert_eq!(stdin.get("method").and_then(json_as_string), Some("stdin"));
        assert_eq!(
            stdin.get("task_id").and_then(json_as_string),
            Some("task-1")
        );
        assert_eq!(stdin.get("input").and_then(json_as_string), Some("probe\n"));
        assert!(matches!(
            stdin.get("close_stdin"),
            Some(JsonValue::Bool(true))
        ));
        assert_eq!(stdin.get("timeout_ms").and_then(json_as_u64), Some(100));

        let cancel = agents_shell_request_json(&AgentsShellArgs {
            action: AgentsShellAction::Cancel {
                task_id: None,
                all: true,
            },
            json: false,
        });
        let cancel = json_as_object(&cancel).unwrap();
        assert_eq!(
            cancel.get("method").and_then(json_as_string),
            Some("cancel")
        );
        assert!(matches!(cancel.get("all"), Some(JsonValue::Bool(true))));
    }

    #[test]
    fn thread_commands_switch_and_clear_active_thread() {
        let root = temp_root("threads");
        let config_dir = root.join(".dscode");
        let threads = agent_threads_dir(config_dir.to_str().unwrap());
        std::fs::create_dir_all(&threads).unwrap();
        std::fs::write(threads.join("thread-1.md"), "# Agent Thread thread-1\n").unwrap();

        switch_thread(config_dir.to_str().unwrap(), "thread-1").unwrap();
        assert_eq!(
            read_active_thread(config_dir.to_str().unwrap()).as_deref(),
            Some("thread-1")
        );

        current_thread(config_dir.to_str().unwrap()).unwrap();
        clear_thread(config_dir.to_str().unwrap()).unwrap();
        assert!(read_active_thread(config_dir.to_str().unwrap()).is_none());
    }

    #[test]
    fn show_thread_rejects_unsafe_id() {
        let root = temp_root("unsafe-thread");
        let config_dir = root.join(".dscode");

        let error = show_thread(config_dir.to_str().unwrap(), "../bad").unwrap_err();

        assert!(error.to_string().contains("invalid subagent thread id"));
    }

    #[test]
    fn rlm_cli_read_lifecycle_args_build_tool_inputs() {
        let status = rlm_status_tool_input(&AgentsRlmStatusArgs {
            session_id: Some("live.1".to_string()),
            limit: Some(5),
            json: true,
        });
        assert_eq!(status.get("session_id"), Some("live.1"));
        assert_eq!(status.get("limit"), Some("5"));

        let events = rlm_events_tool_input(&AgentsRlmEventsArgs {
            session_id: "live.1".to_string(),
            cursor: Some(7),
            limit: Some(3),
            json: false,
        });
        assert_eq!(events.get("session_id"), Some("live.1"));
        assert_eq!(events.get("cursor"), Some("7"));
        assert_eq!(events.get("limit"), Some("3"));

        let wait = rlm_wait_tool_input(&AgentsRlmWaitArgs {
            session_id: "live.1".to_string(),
            cursor: Some(9),
            limit: Some(4),
            timeout_ms: Some(2500),
            poll_interval_ms: Some(50),
            json: true,
        });
        assert_eq!(wait.get("session_id"), Some("live.1"));
        assert_eq!(wait.get("cursor"), Some("9"));
        assert_eq!(wait.get("limit"), Some("4"));
        assert_eq!(wait.get("timeout_ms"), Some("2500"));
        assert_eq!(wait.get("poll_interval_ms"), Some("50"));
    }

    #[test]
    fn rlm_cli_stateful_lifecycle_args_build_tool_inputs() {
        let cancel = rlm_cancel_tool_input(&AgentsRlmCancelArgs {
            session_id: "live.1".to_string(),
            task_id: Some("task-1".to_string()),
            all: false,
            force: true,
            reason: Some("operator stop".to_string()),
            json: true,
        });
        assert_eq!(cancel.get("session_id"), Some("live.1"));
        assert_eq!(cancel.get("task_id"), Some("task-1"));
        assert_eq!(cancel.get("force"), Some("true"));
        assert_eq!(cancel.get("reason"), Some("operator stop"));

        let recover = rlm_recover_tool_input(&AgentsRlmRecoverArgs {
            session_id: None,
            all: true,
            mode: Some("fail".to_string()),
            dry_run: true,
            force: true,
            limit: Some(8),
            reason: Some("takeover".to_string()),
            json: false,
        });
        assert_eq!(recover.get("all"), Some("true"));
        assert_eq!(recover.get("mode"), Some("fail"));
        assert_eq!(recover.get("dry_run"), Some("true"));
        assert_eq!(recover.get("force"), Some("true"));
        assert_eq!(recover.get("limit"), Some("8"));
        assert_eq!(recover.get("reason"), Some("takeover"));

        let stop = rlm_stop_tool_input(&AgentsRlmStopArgs {
            session_id: "live.1".to_string(),
            reason: Some("done".to_string()),
            json: false,
        });
        assert_eq!(stop.get("session_id"), Some("live.1"));
        assert_eq!(stop.get("reason"), Some("done"));

        let run_next = rlm_run_next_tool_input(&AgentsRlmRunNextArgs {
            session_id: "live.1".to_string(),
            task_id: Some("task-2".to_string()),
            dry_run: true,
            json: false,
        });
        assert_eq!(run_next.get("session_id"), Some("live.1"));
        assert_eq!(run_next.get("task_id"), Some("task-2"));
        assert_eq!(run_next.get("dry_run"), Some("true"));

        let drain = rlm_drain_tool_input(&AgentsRlmDrainArgs {
            session_id: "live.1".to_string(),
            max_turns: Some(4),
            dry_run: true,
            json: true,
        });
        assert_eq!(drain.get("session_id"), Some("live.1"));
        assert_eq!(drain.get("max_turns"), Some("4"));
        assert_eq!(drain.get("dry_run"), Some("true"));
    }

    #[test]
    fn shell_supervisor_protocol_parses_methods_and_defaults_to_health() {
        assert_eq!(parse_shell_supervisor_method("").unwrap(), "health");
        assert_eq!(
            parse_shell_supervisor_method(r#"{"method":"status"}"#).unwrap(),
            "status"
        );
        assert!(parse_shell_supervisor_method("[]")
            .unwrap_err()
            .to_string()
            .contains("must be a JSON object"));
    }

    #[test]
    fn shell_supervisor_protocol_reports_unsupported_unknown_method() {
        let response = shell_supervisor_protocol_response(
            "native_pty",
            Path::new("/work/repo"),
            Path::new("/work/repo/.dscode/shell-supervisor/supervisor.sock"),
            "epoch+1",
        );
        let object = json_as_object(&response).unwrap();

        assert_eq!(
            json_as_string(object.get("status").unwrap()),
            Some("unsupported")
        );
        assert_eq!(
            json_as_string(object.get("pty_backend").unwrap()),
            Some("none")
        );
        assert!(matches!(
            object.get("native_pty"),
            Some(JsonValue::Bool(value)) if *value == native_supervisor_pty_supported()
        ));
        assert!(matches!(
            object.get("active_jobs"),
            Some(JsonValue::Number(value)) if value == "0"
        ));
        assert!(json_as_string(object.get("error").unwrap())
            .unwrap()
            .contains("is not supported by this protocol"));
    }

    #[test]
    fn shell_supervisor_protocol_start_creates_durable_job() {
        let root = temp_root("shell-supervisor-start");
        let state_dir = root.join(".dscode/shell-supervisor");
        std::fs::create_dir_all(&state_dir).unwrap();
        let request = parse_shell_supervisor_request(
            r#"{"method":"start","arguments":{"command":"tail -f /dev/null","tty":false}}"#,
        )
        .unwrap();

        let response = shell_supervisor_protocol_response_for_request(
            &request,
            &root,
            &state_dir.join("supervisor.sock"),
            "epoch+start",
        );
        let object = json_as_object(&response).unwrap();
        let task_id = json_as_string(object.get("task_id").unwrap()).unwrap();
        let manifest = std::fs::read_to_string(
            root.join(".dscode/shell-jobs")
                .join(task_id)
                .join("manifest.json"),
        )
        .unwrap();
        let supervisor_manifest = std::fs::read_to_string(state_dir.join("manifest.json")).unwrap();

        assert_eq!(json_as_string(object.get("status").unwrap()), Some("ok"));
        assert_eq!(json_as_string(object.get("method").unwrap()), Some("start"));
        assert_eq!(
            json_as_string(object.get("job_pty_backend").unwrap()),
            Some("none")
        );
        assert!(matches!(
            object.get("job_tty"),
            Some(JsonValue::Bool(false))
        ));
        assert!(matches!(
            object.get("native_pty"),
            Some(JsonValue::Bool(value)) if *value == native_supervisor_pty_supported()
        ));
        assert!(matches!(
            object.get("active_jobs"),
            Some(JsonValue::Number(value)) if value == "1"
        ));
        assert!(
            manifest.contains(r#""command":"tail -f /dev/null""#),
            "{manifest}"
        );
        assert!(
            supervisor_manifest
                .contains(r#""methods":["health","status","show","start","wait","replay","attach","stdin","resize","cancel","shutdown"]"#),
            "{supervisor_manifest}"
        );
        assert!(
            supervisor_manifest.contains(r#""unsupported_methods":[]"#),
            "{supervisor_manifest}"
        );
        assert!(
            supervisor_manifest.contains(r#""active_jobs":1"#),
            "{supervisor_manifest}"
        );

        let _ = crate::tools::exec_shell::ExecShellCancelTool.execute(
            ToolInput::new()
                .with_arg("cwd", root.display().to_string())
                .with_arg("task_id", task_id.to_string()),
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn shell_supervisor_protocol_tty_start_records_native_pty_events() {
        if !native_supervisor_pty_supported() {
            return;
        }
        let root = temp_root("shell-supervisor-native-pty");
        let state_dir = root.join(".dscode/shell-supervisor");
        std::fs::create_dir_all(&state_dir).unwrap();
        let socket = state_dir.join("supervisor.sock");
        let start = parse_shell_supervisor_request(
            r#"{"method":"start","arguments":{"command":"echo native-pty-ready","tty":true,"tty_rows":24,"tty_cols":80}}"#,
        )
        .unwrap();

        let response =
            shell_supervisor_protocol_response_for_request(&start, &root, &socket, "epoch+native");
        let object = json_as_object(&response).unwrap();
        assert_eq!(
            json_as_string(object.get("status").unwrap()),
            Some("ok"),
            "{response:?}"
        );
        let task_id = json_as_string(object.get("task_id").unwrap())
            .unwrap()
            .to_string();
        assert_eq!(
            json_as_string(object.get("job_pty_backend").unwrap()),
            Some("native-supervisor")
        );
        assert!(matches!(object.get("job_tty"), Some(JsonValue::Bool(true))));

        let wait = parse_shell_supervisor_request(&format!(
            r#"{{"method":"wait","arguments":{{"task_id":"{task_id}","timeout_ms":2000}}}}"#
        ))
        .unwrap();
        let response =
            shell_supervisor_protocol_response_for_request(&wait, &root, &socket, "epoch+wait");
        let object = json_as_object(&response).unwrap();
        let wait_summary = json_as_string(object.get("wait_summary").unwrap()).unwrap();
        assert!(wait_summary.contains("pty_backend: native-supervisor"));
        assert!(wait_summary.contains("attachable: true"));

        let manifest = std::fs::read_to_string(
            root.join(".dscode/shell-jobs")
                .join(&task_id)
                .join("manifest.json"),
        )
        .unwrap();
        assert!(manifest.contains(r#""pty_backend":"native-supervisor""#));
        assert!(manifest.contains(r#""terminal_event_log":"terminal-events.jsonl""#));

        let mut replay_summary = String::new();
        for _ in 0..20 {
            let replay = parse_shell_supervisor_request(&format!(
                r#"{{"method":"replay","arguments":{{"task_id":"{task_id}","stream":"terminal","cursor":0,"limit_bytes":4000}}}}"#
            ))
            .unwrap();
            let response = shell_supervisor_protocol_response_for_request(
                &replay,
                &root,
                &socket,
                "epoch+replay",
            );
            let object = json_as_object(&response).unwrap();
            replay_summary = json_as_string(object.get("replay_summary").unwrap())
                .unwrap()
                .to_string();
            if replay_summary.contains("native-pty-ready") {
                break;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        assert!(
            replay_summary.contains("stream: terminal"),
            "{replay_summary}"
        );
        assert!(
            replay_summary.contains("native-pty-ready"),
            "{replay_summary}"
        );

        let _ = crate::tools::exec_shell::ExecShellCancelTool.execute(
            ToolInput::new()
                .with_arg("cwd", root.display().to_string())
                .with_arg("task_id", task_id),
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn shell_supervisor_protocol_native_pty_resize_records_event() {
        if !native_supervisor_pty_supported() {
            return;
        }
        let root = temp_root("shell-supervisor-native-resize");
        let state_dir = root.join(".dscode/shell-supervisor");
        std::fs::create_dir_all(&state_dir).unwrap();
        let socket = state_dir.join("supervisor.sock");
        let start = parse_shell_supervisor_request(
            r#"{"method":"start","arguments":{"command":"tail -f /dev/null","tty":true,"tty_rows":24,"tty_cols":80}}"#,
        )
        .unwrap();
        let response =
            shell_supervisor_protocol_response_for_request(&start, &root, &socket, "epoch+native");
        let object = json_as_object(&response).unwrap();
        assert_eq!(
            json_as_string(object.get("status").unwrap()),
            Some("ok"),
            "{response:?}"
        );
        let task_id = json_as_string(object.get("task_id").unwrap())
            .unwrap()
            .to_string();

        let resize = parse_shell_supervisor_request(&format!(
            r#"{{"method":"resize","arguments":{{"task_id":"{task_id}","tty_rows":32,"tty_cols":100}}}}"#
        ))
        .unwrap();
        let response =
            shell_supervisor_protocol_response_for_request(&resize, &root, &socket, "epoch+resize");
        let object = json_as_object(&response).unwrap();
        let resize_summary = json_as_string(object.get("resize_summary").unwrap()).unwrap();
        assert!(resize_summary.contains("meta.live_resize=native_tiocswinsz"));
        assert!(resize_summary.contains("pty_backend: native-supervisor"));
        assert!(resize_summary.contains("tty_rows: 32"));
        assert!(resize_summary.contains("tty_cols: 100"));

        let replay = parse_shell_supervisor_request(&format!(
            r#"{{"method":"replay","arguments":{{"task_id":"{task_id}","stream":"terminal","cursor":0,"limit_bytes":4000}}}}"#
        ))
        .unwrap();
        let response =
            shell_supervisor_protocol_response_for_request(&replay, &root, &socket, "epoch+replay");
        let object = json_as_object(&response).unwrap();
        let replay_summary = json_as_string(object.get("replay_summary").unwrap()).unwrap();
        assert!(replay_summary.contains("[2 resize"));
        assert!(replay_summary.contains("rows=32 cols=100"));

        let _ = crate::tools::exec_shell::ExecShellCancelTool.execute(
            ToolInput::new()
                .with_arg("cwd", root.display().to_string())
                .with_arg("task_id", task_id),
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn shell_supervisor_protocol_controls_durable_jobs() {
        let root = temp_root("shell-supervisor-control");
        let state_dir = root.join(".dscode/shell-supervisor");
        std::fs::create_dir_all(&state_dir).unwrap();
        let socket = state_dir.join("supervisor.sock");

        let start = parse_shell_supervisor_request(
            r#"{"method":"start","arguments":{"command":"echo supervisor-control","tty":false}}"#,
        )
        .unwrap();
        let response =
            shell_supervisor_protocol_response_for_request(&start, &root, &socket, "epoch+start");
        let object = json_as_object(&response).unwrap();
        let task_id = json_as_string(object.get("task_id").unwrap())
            .unwrap()
            .to_string();
        assert_eq!(json_as_string(object.get("status").unwrap()), Some("ok"));
        assert!(matches!(
            object.get("active_jobs"),
            Some(JsonValue::Number(value)) if value == "1"
        ));

        let wait = parse_shell_supervisor_request(&format!(
            r#"{{"method":"wait","arguments":{{"task_id":"{task_id}","timeout_ms":1000}}}}"#
        ))
        .unwrap();
        let response =
            shell_supervisor_protocol_response_for_request(&wait, &root, &socket, "epoch+wait");
        let object = json_as_object(&response).unwrap();
        let wait_summary = json_as_string(object.get("wait_summary").unwrap()).unwrap();
        assert_eq!(json_as_string(object.get("status").unwrap()), Some("ok"));
        assert!(wait_summary.contains("status: completed"), "{wait_summary}");
        assert!(
            wait_summary.contains("supervisor-control"),
            "{wait_summary}"
        );
        assert!(matches!(
            object.get("active_jobs"),
            Some(JsonValue::Number(value)) if value == "0"
        ));

        let replay = parse_shell_supervisor_request(&format!(
            r#"{{"method":"replay","arguments":{{"task_id":"{task_id}","stream":"stdout"}}}}"#
        ))
        .unwrap();
        let response =
            shell_supervisor_protocol_response_for_request(&replay, &root, &socket, "epoch+replay");
        let object = json_as_object(&response).unwrap();
        let replay_summary = json_as_string(object.get("replay_summary").unwrap()).unwrap();
        assert_eq!(json_as_string(object.get("status").unwrap()), Some("ok"));
        assert!(
            replay_summary.contains("supervisor-control"),
            "{replay_summary}"
        );

        let attach = parse_shell_supervisor_request(&format!(
            r#"{{"method":"attach","arguments":{{"task_id":"{task_id}","cursor":0}}}}"#
        ))
        .unwrap();
        let response =
            shell_supervisor_protocol_response_for_request(&attach, &root, &socket, "epoch+attach");
        let object = json_as_object(&response).unwrap();
        let attach_summary = json_as_string(object.get("attach_summary").unwrap()).unwrap();
        assert_eq!(json_as_string(object.get("status").unwrap()), Some("ok"));
        assert!(
            attach_summary.contains("mode: terminal_attach_replay"),
            "{attach_summary}"
        );
        assert!(
            attach_summary.contains("supervisor-control"),
            "{attach_summary}"
        );

        let stdin_start = parse_shell_supervisor_request(
            r#"{"method":"start","arguments":{"command":"cat -","tty":false}}"#,
        )
        .unwrap();
        let response = shell_supervisor_protocol_response_for_request(
            &stdin_start,
            &root,
            &socket,
            "epoch+stdin-start",
        );
        let object = json_as_object(&response).unwrap();
        let stdin_task_id = json_as_string(object.get("task_id").unwrap())
            .unwrap()
            .to_string();

        let stdin = parse_shell_supervisor_request(&format!(
            r#"{{"method":"stdin","arguments":{{"task_id":"{stdin_task_id}","input":"hello supervisor stdin\n","close_stdin":true,"timeout_ms":1000}}}}"#
        ))
        .unwrap();
        let response =
            shell_supervisor_protocol_response_for_request(&stdin, &root, &socket, "epoch+stdin");
        let object = json_as_object(&response).unwrap();
        let stdin_summary = json_as_string(object.get("stdin_summary").unwrap()).unwrap();
        assert_eq!(json_as_string(object.get("status").unwrap()), Some("ok"));
        assert!(
            stdin_summary.contains("status: completed"),
            "{stdin_summary}"
        );
        assert!(
            stdin_summary.contains("hello supervisor stdin"),
            "{stdin_summary}"
        );

        let cancel_start = parse_shell_supervisor_request(
            r#"{"method":"start","arguments":{"command":"tail -f /dev/null","tty":false}}"#,
        )
        .unwrap();
        let response = shell_supervisor_protocol_response_for_request(
            &cancel_start,
            &root,
            &socket,
            "epoch+cancel-start",
        );
        let object = json_as_object(&response).unwrap();
        let cancel_task_id = json_as_string(object.get("task_id").unwrap())
            .unwrap()
            .to_string();
        assert!(matches!(
            object.get("active_jobs"),
            Some(JsonValue::Number(value)) if value == "1"
        ));

        let cancel = parse_shell_supervisor_request(&format!(
            r#"{{"method":"cancel","arguments":{{"task_id":"{cancel_task_id}"}}}}"#
        ))
        .unwrap();
        let response =
            shell_supervisor_protocol_response_for_request(&cancel, &root, &socket, "epoch+cancel");
        let object = json_as_object(&response).unwrap();
        let cancel_summary = json_as_string(object.get("cancel_summary").unwrap()).unwrap();
        assert_eq!(json_as_string(object.get("status").unwrap()), Some("ok"));
        assert!(
            cancel_summary.contains("Canceled background shell job"),
            "{cancel_summary}"
        );
        assert!(matches!(
            object.get("active_jobs"),
            Some(JsonValue::Number(value)) if value == "0"
        ));

        let resize = parse_shell_supervisor_request(
            r#"{"method":"resize","arguments":{"tty_rows":40,"tty_cols":120}}"#,
        )
        .unwrap();
        let response =
            shell_supervisor_protocol_response_for_request(&resize, &root, &socket, "epoch+resize");
        let object = json_as_object(&response).unwrap();
        assert_eq!(
            json_as_string(object.get("method").unwrap()),
            Some("resize")
        );
        assert_eq!(json_as_string(object.get("status").unwrap()), Some("error"));
        assert!(json_as_string(object.get("error").unwrap())
            .unwrap()
            .contains("requires task_id"));

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn shell_supervisor_protocol_show_includes_job_inventory() {
        let root = temp_root("shell-supervisor-show");
        let task_id = "shell-one";
        let job_dir = root.join(".dscode/shell-jobs").join(task_id);
        std::fs::create_dir_all(&job_dir).unwrap();
        std::fs::write(job_dir.join("stdout.log"), "durable\n").unwrap();
        let manifest = JsonValue::Object(BTreeMap::from([
            (
                "kind".to_string(),
                JsonValue::String("deepseek.exec_shell.job.v1".to_string()),
            ),
            ("id".to_string(), JsonValue::String(task_id.to_string())),
            (
                "command".to_string(),
                JsonValue::String("echo durable".to_string()),
            ),
            (
                "cwd".to_string(),
                JsonValue::String(root.display().to_string()),
            ),
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
                JsonValue::Number("8".to_string()),
            ),
            (
                "stderr_total_bytes".to_string(),
                JsonValue::Number("0".to_string()),
            ),
        ]));
        std::fs::write(
            job_dir.join("manifest.json"),
            json_value_to_string(&manifest),
        )
        .unwrap();

        let response = shell_supervisor_protocol_response(
            "show",
            &root,
            &root.join(".dscode/shell-supervisor/supervisor.sock"),
            "epoch+3",
        );
        let object = json_as_object(&response).unwrap();
        let inventory = json_as_string(object.get("job_inventory").unwrap()).unwrap();

        assert_eq!(json_as_string(object.get("status").unwrap()), Some("ok"));
        assert_eq!(json_as_string(object.get("method").unwrap()), Some("show"));
        assert!(inventory.contains("Background shell jobs"), "{inventory}");
        assert!(inventory.contains(task_id), "{inventory}");
        assert!(inventory.contains("echo durable"), "{inventory}");
        assert!(inventory.contains("stdout=8"), "{inventory}");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn shell_supervisor_protocol_status_counts_active_durable_jobs() {
        let root = temp_root("shell-supervisor-status-active");
        let running_dir = root.join(".dscode/shell-jobs").join("shell-running");
        let exited_dir = root.join(".dscode/shell-jobs").join("shell-exited");
        std::fs::create_dir_all(&running_dir).unwrap();
        std::fs::create_dir_all(&exited_dir).unwrap();
        for (dir, id, status, pid) in [
            (
                &running_dir,
                "shell-running",
                "running",
                std::process::id().to_string(),
            ),
            (&exited_dir, "shell-exited", "exited", "0".to_string()),
        ] {
            let manifest = JsonValue::Object(BTreeMap::from([
                ("id".to_string(), JsonValue::String(id.to_string())),
                (
                    "command".to_string(),
                    JsonValue::String(format!("echo {id}")),
                ),
                (
                    "cwd".to_string(),
                    JsonValue::String(root.display().to_string()),
                ),
                ("status".to_string(), JsonValue::String(status.to_string())),
                ("pid".to_string(), JsonValue::Number(pid)),
                (
                    "started_at".to_string(),
                    JsonValue::String("epoch+1".to_string()),
                ),
                (
                    "updated_at".to_string(),
                    JsonValue::String("epoch+2".to_string()),
                ),
            ]));
            std::fs::write(dir.join("manifest.json"), json_value_to_string(&manifest)).unwrap();
        }

        let response = shell_supervisor_protocol_response(
            "status",
            &root,
            &root.join(".dscode/shell-supervisor/supervisor.sock"),
            "epoch+3",
        );
        let object = json_as_object(&response).unwrap();

        assert_eq!(json_as_string(object.get("status").unwrap()), Some("ok"));
        assert_eq!(
            json_as_string(object.get("method").unwrap()),
            Some("status")
        );
        assert!(matches!(
            object.get("active_jobs"),
            Some(JsonValue::Number(value)) if value == "1"
        ));
        assert!(!object.contains_key("active_jobs_error"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn shell_supervisor_protocol_refreshes_manifest_job_count() {
        let root = temp_root("shell-supervisor-manifest-refresh");
        let state_dir = root.join(".dscode/shell-supervisor");
        let job_dir = root.join(".dscode/shell-jobs").join("shell-running");
        std::fs::create_dir_all(&state_dir).unwrap();
        std::fs::create_dir_all(&job_dir).unwrap();
        std::fs::write(
            state_dir.join("manifest.json"),
            r#"{"kind":"deepseek.exec_shell.supervisor.v1","supervisor_pid":0,"supervisor_socket":"old.sock","supervisor_epoch":"epoch+old","protocol":"newline-json-v1","methods":["health","status","show","start","wait","replay","attach","stdin","resize","cancel","shutdown"],"unsupported_methods":[],"active_jobs":0,"started_at":"epoch+old","updated_at":"epoch+old","control_token_hash":"sha256:do-not-print"}"#,
        )
        .unwrap();
        let manifest = JsonValue::Object(BTreeMap::from([
            (
                "id".to_string(),
                JsonValue::String("shell-running".to_string()),
            ),
            (
                "command".to_string(),
                JsonValue::String("sleep 60".to_string()),
            ),
            (
                "cwd".to_string(),
                JsonValue::String(root.display().to_string()),
            ),
            (
                "status".to_string(),
                JsonValue::String("running".to_string()),
            ),
            (
                "pid".to_string(),
                JsonValue::Number(std::process::id().to_string()),
            ),
            (
                "started_at".to_string(),
                JsonValue::String("epoch+1".to_string()),
            ),
            (
                "updated_at".to_string(),
                JsonValue::String("epoch+2".to_string()),
            ),
        ]));
        std::fs::write(
            job_dir.join("manifest.json"),
            json_value_to_string(&manifest),
        )
        .unwrap();

        let socket = state_dir.join("supervisor.sock");
        let response = shell_supervisor_protocol_response("status", &root, &socket, "epoch+fresh");
        let object = json_as_object(&response).unwrap();
        let refreshed = std::fs::read_to_string(state_dir.join("manifest.json")).unwrap();

        assert_eq!(json_as_string(object.get("status").unwrap()), Some("ok"));
        assert!(!object.contains_key("manifest_refresh_error"));
        assert!(refreshed.contains(r#""active_jobs":1"#), "{refreshed}");
        assert!(
            refreshed.contains(r#""supervisor_socket":"#) && refreshed.contains("supervisor.sock"),
            "{refreshed}"
        );
        assert!(refreshed.contains(r#""supervisor_epoch":"epoch+fresh""#));
        assert!(refreshed.contains(r#""updated_at":"epoch+"#));
        assert!(refreshed.contains(r#""control_token_hash":null"#));
        assert!(!refreshed.contains("do-not-print"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    #[cfg(unix)]
    fn shell_supervisor_manifest_writes_protocol_without_control_secret() {
        let root = temp_root("shell-supervisor-manifest");
        let state_dir = root.join(".dscode/shell-supervisor");
        std::fs::create_dir_all(&state_dir).unwrap();
        let socket = state_dir.join("supervisor.sock");

        write_shell_supervisor_manifest(&root, &socket, "epoch+1").unwrap();

        let manifest = std::fs::read_to_string(state_dir.join("manifest.json")).unwrap();
        assert!(manifest.contains(r#""kind":"deepseek.exec_shell.supervisor.v1""#));
        assert!(manifest.contains(r#""protocol":"newline-json-v1""#));
        assert!(manifest.contains(r#""methods":["health","status","show","start","wait","replay","attach","stdin","resize","cancel","shutdown"]"#));
        assert!(manifest.contains(r#""unsupported_methods":[]"#));
        assert!(manifest.contains(r#""control_token_hash":null"#));
        assert!(!manifest.contains("control_token\":\""));
    }

    #[test]
    #[cfg(unix)]
    fn shell_supervisor_stream_handles_status_and_invalid_request() {
        let (shutdown, response) = shell_supervisor_stream_roundtrip(r#"{"method":"status"}"#);
        assert!(!shutdown);
        assert!(response.contains(r#""method":"status""#));
        assert!(response.contains(r#""status":"ok""#));

        let (shutdown, response) = shell_supervisor_stream_roundtrip("[]");
        assert!(!shutdown);
        assert!(response.contains(r#""method":"invalid_request""#));
        assert!(response.contains(r#""status":"error""#));
    }

    #[test]
    fn service_templates_render_runtime_and_agent_supervisors() {
        let config = ServiceTemplateConfig {
            kind: AgentsServiceKind::All,
            out: None,
            bin: "/usr/local/bin/deepseek".to_string(),
            workdir: "/work/repo".to_string(),
            addr: "127.0.0.1:9876".to_string(),
            interval_ms: 750,
            budget: Some(6),
        };

        let templates = service_templates(&config);
        let paths = templates
            .iter()
            .map(|template| template.path)
            .collect::<Vec<_>>();
        assert_eq!(
            paths,
            vec![
                "systemd/deepseek-runtime.service",
                "systemd/deepseek-agents.service",
                "systemd/deepseek-diagnostics.service",
                "systemd/deepseek-shell-supervisor.service",
                "launchd/com.deepseek.runtime.plist",
                "launchd/com.deepseek.agents.plist",
                "launchd/com.deepseek.diagnostics.plist",
                "launchd/com.deepseek.shell-supervisor.plist",
            ]
        );
        assert!(templates[0]
            .body
            .contains("serve --http --addr 127.0.0.1:9876"));
        assert!(templates[1]
            .body
            .contains("agents daemon --interval-ms 750 --budget 6 --json"));
        assert!(templates[1].body.contains("queued live RLM turn per tick"));
        assert!(templates[2]
            .body
            .contains("diagnostics --watch --changed --interval-ms 750 --json"));
        assert!(templates[3].body.contains("agents shell-supervisor --json"));
        assert!(templates[3]
            .body
            .contains("including native-supervisor PTY jobs where supported"));
        assert!(!templates[3]
            .body
            .contains("native PTY sessions are not implemented yet"));
        assert!(templates[4]
            .body
            .contains("<string>com.deepseek.runtime</string>"));
        assert!(templates[5]
            .body
            .contains("<string>com.deepseek.agents</string>"));
        assert!(templates[5].body.contains("queued live RLM turn per tick"));
        assert!(templates[6]
            .body
            .contains("<string>com.deepseek.diagnostics</string>"));
        assert!(templates[6].body.contains("<string>--json</string>"));
        assert!(templates[7]
            .body
            .contains("<string>com.deepseek.shell-supervisor</string>"));
        assert!(templates[7]
            .body
            .contains("<string>shell-supervisor</string>"));
        assert!(templates[7]
            .body
            .contains("including native-supervisor PTY jobs where supported"));
        assert!(!templates[7]
            .body
            .contains("native PTY sessions are not implemented yet"));
    }

    #[test]
    fn render_agent_services_writes_lifecycle_guide() {
        let out = temp_root("service-lifecycle").join("services");
        render_agent_services(AgentsServiceArgs {
            kind: AgentsServiceKind::All,
            out: Some(out.display().to_string()),
            bin: Some("/usr/local/bin/deepseek".to_string()),
            workdir: Some("/work/repo".to_string()),
            addr: "127.0.0.1:9876".to_string(),
            interval_ms: 750,
            budget: Some(6),
        })
        .unwrap();

        assert!(out.join("systemd/deepseek-runtime.service").is_file());
        assert!(out.join("launchd/com.deepseek.runtime.plist").is_file());
        let guide = std::fs::read_to_string(out.join("SERVICES.md")).unwrap();
        assert!(guide.contains("DeepSeekCode Service Lifecycle"));
        assert!(guide.contains("systemctl --user enable --now"));
        assert!(guide.contains("launchctl load -w"));
        assert!(guide.contains("journalctl --user"));
        assert!(guide.contains("launchctl kickstart -k"));
        assert!(guide.contains("curl -fsS http://127.0.0.1:9876/v1/health"));
        assert!(guide.contains("/usr/local/bin/deepseek agents shell status --json"));
    }

    #[test]
    fn service_doctor_reports_generated_service_health() {
        let workdir = temp_root("service-doctor-workdir");
        let out = workdir.join("services");
        let bin = std::env::current_exe().unwrap();
        render_agent_services(AgentsServiceArgs {
            kind: AgentsServiceKind::Systemd,
            out: Some(out.display().to_string()),
            bin: Some(bin.display().to_string()),
            workdir: Some(workdir.display().to_string()),
            addr: "127.0.0.1:9876".to_string(),
            interval_ms: 750,
            budget: Some(6),
        })
        .unwrap();

        let report = build_service_doctor_report(ServiceTemplateConfig {
            kind: AgentsServiceKind::Systemd,
            out: Some(out.clone()),
            bin: bin.display().to_string(),
            workdir: workdir.display().to_string(),
            addr: "127.0.0.1:9876".to_string(),
            interval_ms: 750,
            budget: Some(6),
        });

        assert_eq!(report.blocker_count(), 0, "{:?}", report.checks);
        assert!(report
            .checks
            .iter()
            .any(|check| check.name == "shell_supervisor_service"
                && check.status == ServiceDoctorStatus::Ok));
        let json = render_service_doctor_json(&report);
        assert!(json.contains("\"kind\":\"deepseek.agents.service_doctor.v1\""));
        assert!(json.contains("\"service_kind\":\"systemd\""));
        assert!(json.contains("\"blockers\":0"));
    }

    #[test]
    fn service_doctor_detects_stale_generated_template() {
        let workdir = temp_root("service-doctor-stale");
        let out = workdir.join("services");
        let bin = std::env::current_exe().unwrap();
        render_agent_services(AgentsServiceArgs {
            kind: AgentsServiceKind::Systemd,
            out: Some(out.display().to_string()),
            bin: Some(bin.display().to_string()),
            workdir: Some(workdir.display().to_string()),
            addr: "127.0.0.1:9876".to_string(),
            interval_ms: 750,
            budget: None,
        })
        .unwrap();
        std::fs::write(out.join("systemd/deepseek-runtime.service"), "stale").unwrap();

        let report = build_service_doctor_report(ServiceTemplateConfig {
            kind: AgentsServiceKind::Systemd,
            out: Some(out.clone()),
            bin: bin.display().to_string(),
            workdir: workdir.display().to_string(),
            addr: "127.0.0.1:9876".to_string(),
            interval_ms: 750,
            budget: None,
        });

        assert!(report.blocker_count() >= 1, "{:?}", report.checks);
        assert!(report.checks.iter().any(|check| {
            check.status == ServiceDoctorStatus::Blocker
                && check.message.contains("stale or differs")
        }));
        let text = render_service_doctor_text(&report);
        assert!(text.contains("[blocker] output"));
        assert!(text.contains("summary:"));
    }

    #[test]
    fn service_smoke_resolves_ephemeral_loopback_addr() {
        let addr = resolve_service_smoke_addr("127.0.0.1:0").unwrap();
        let parsed = addr.parse::<std::net::SocketAddr>().unwrap();
        assert_eq!(parsed.ip().to_string(), "127.0.0.1");
        assert_ne!(parsed.port(), 0);
    }

    #[test]
    fn service_smoke_json_reports_blockers_and_warnings() {
        let mut report = ServiceSmokeReport {
            binary: "/missing/deepseek".to_string(),
            workdir: PathBuf::from("/work/repo"),
            requested_addr: "127.0.0.1:0".to_string(),
            resolved_addr: "127.0.0.1:4567".to_string(),
            addr_error: None,
            timeout_ms: 2500,
            checks: Vec::new(),
        };
        push_service_doctor_check(
            &mut report.checks,
            ServiceDoctorStatus::Blocker,
            "runtime",
            "health failed",
        );
        push_service_doctor_check(
            &mut report.checks,
            ServiceDoctorStatus::Warn,
            "shell_supervisor",
            "unsupported platform",
        );

        let json = render_service_smoke_json(&report);
        assert!(json.contains("\"kind\":\"deepseek.agents.service_smoke.v1\""));
        assert!(json.contains("\"blockers\":1"));
        assert!(json.contains("\"warnings\":1"));
        assert!(json.contains("\"resolved_addr\":\"127.0.0.1:4567\""));
    }

    #[cfg(unix)]
    #[test]
    fn service_smoke_shell_supervisor_control_smoke_runs_start_wait_attach() {
        use std::io::{BufRead as _, Write as _};

        let root = std::env::temp_dir().join(format!(
            "dsc-smk-ctl-{}-{}",
            std::process::id(),
            current_epoch_seconds()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let socket = root.join("supervisor.sock");
        let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        let tty = cfg!(all(unix, target_os = "linux"));
        let handle = std::thread::spawn(move || {
            for expected in ["start", "wait", "attach"] {
                let (mut stream, _) = listener.accept().unwrap();
                let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
                let mut request = String::new();
                reader.read_line(&mut request).unwrap();
                assert!(request.contains(&format!(r#""method":"{expected}""#)));
                match expected {
                    "start" => {
                        assert!(request.contains(if tty {
                            r#""tty":true"#
                        } else {
                            r#""tty":false"#
                        }));
                        let backend = if tty { "native-supervisor" } else { "none" };
                        stream
                            .write_all(
                                format!(
                                    r#"{{"status":"ok","method":"start","task_id":"task-smoke","job_pty_backend":"{backend}"}}"#
                                )
                                .as_bytes(),
                            )
                            .unwrap();
                    }
                    "wait" => {
                        assert!(request.contains(r#""task_id":"task-smoke""#));
                        stream
                            .write_all(
                                br#"{"status":"ok","method":"wait","wait_summary":"status: exited"}"#,
                            )
                            .unwrap();
                    }
                    "attach" => {
                        assert!(request.contains(r#""task_id":"task-smoke""#));
                        stream
                            .write_all(
                                br#"{"status":"ok","method":"attach","attach_summary":"deepseek-shell-supervisor-smoke"}"#,
                            )
                            .unwrap();
                    }
                    _ => unreachable!(),
                }
                stream.write_all(b"\n").unwrap();
            }
        });

        let summary = shell_supervisor_control_smoke(&socket, 2500).unwrap();
        handle.join().unwrap();

        assert!(summary.contains("task-smoke"));
        assert!(summary.contains(&format!("tty={tty}")));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(unix)]
    #[test]
    fn service_smoke_blocks_too_long_shell_supervisor_socket_path() {
        let long_workdir =
            PathBuf::from(format!("/tmp/{}", "dsc-service-smoke-long-path-".repeat(5)));
        let mut report = ServiceSmokeReport {
            binary: "/missing/deepseek".to_string(),
            workdir: long_workdir,
            requested_addr: "127.0.0.1:0".to_string(),
            resolved_addr: "127.0.0.1:4567".to_string(),
            addr_error: None,
            timeout_ms: 2500,
            checks: Vec::new(),
        };

        service_smoke_check_shell_supervisor(&mut report);

        assert!(report.checks.iter().any(|check| {
            check.status == ServiceDoctorStatus::Blocker
                && check.name == "shell_supervisor"
                && check.message.contains("socket path is too long")
        }));
    }

    #[cfg(unix)]
    #[test]
    fn service_smoke_blocks_existing_shell_supervisor_socket() {
        use std::io::{BufRead as _, Write as _};

        let root = std::env::temp_dir().join(format!(
            "dsc-smk-{}-{}",
            std::process::id(),
            current_epoch_seconds()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let state_dir = root.join(".dscode/shell-supervisor");
        std::fs::create_dir_all(&state_dir).unwrap();
        let socket = state_dir.join("supervisor.sock");
        let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
            let mut request = String::new();
            reader.read_line(&mut request).unwrap();
            assert!(request.contains("\"method\":\"health\""));
            stream
                .write_all(b"{\"status\":\"ok\",\"method\":\"health\"}\n")
                .unwrap();
        });
        let mut report = ServiceSmokeReport {
            binary: "/missing/deepseek".to_string(),
            workdir: root,
            requested_addr: "127.0.0.1:0".to_string(),
            resolved_addr: "127.0.0.1:4567".to_string(),
            addr_error: None,
            timeout_ms: 2500,
            checks: Vec::new(),
        };

        service_smoke_check_shell_supervisor(&mut report);
        handle.join().unwrap();

        assert!(report.checks.iter().any(|check| {
            check.status == ServiceDoctorStatus::Blocker
                && check.name == "shell_supervisor"
                && check.message.contains("already active")
        }));
        let _ = std::fs::remove_dir_all(&report.workdir);
    }

    #[test]
    fn runtime_task_approval_resolver_waits_for_durable_response() {
        let store = RuntimeStore::new(temp_root("runtime-approval"));
        let session = store
            .create_session("Runtime approval".to_string(), ".".to_string())
            .unwrap();
        let thread = store
            .create_thread_for_session(
                &session.id,
                "Approval work".to_string(),
                ".".to_string(),
                "deepseek-v4-flash".to_string(),
                "agent".to_string(),
            )
            .unwrap();
        let responder_store = store.clone();
        let responder_thread_id = thread.id.clone();
        let responder = std::thread::spawn(move || {
            for _ in 0..50 {
                let events = responder_store
                    .read_events(&responder_thread_id, 0)
                    .expect("events should read");
                if let Some(request) = events
                    .iter()
                    .find(|event| event.kind == "permission_request")
                {
                    responder_store
                        .append_permission_response(
                            &responder_thread_id,
                            None,
                            request.id.clone(),
                            "approved".to_string(),
                        )
                        .expect("response should append");
                    return;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            panic!("permission request was not written");
        });

        let mut resolver = RuntimeTaskApprovalResolver {
            store: store.clone(),
            thread_id: thread.id.clone(),
            poll_interval: Duration::from_millis(5),
            max_polls: Some(200),
        };
        let decision = resolver
            .resolve(&AgentApprovalRequest {
                tool_name: "apply_patch".to_string(),
                input: std::collections::BTreeMap::new(),
                kind: "write".to_string(),
                target: "src/lib.rs".to_string(),
            })
            .unwrap();
        responder.join().unwrap();

        assert_eq!(decision, AgentApprovalDecision::Approved);
        let events = store.read_events(&thread.id, 0).unwrap();
        assert!(events
            .iter()
            .any(|event| event.kind == "permission_request"));
        assert!(events
            .iter()
            .any(|event| event.kind == "permission_response"));
    }

    #[test]
    fn runtime_task_user_input_resolver_waits_for_durable_response() {
        let store = RuntimeStore::new(temp_root("runtime-user-input"));
        let session = store
            .create_session("Runtime user input".to_string(), ".".to_string())
            .unwrap();
        let thread = store
            .create_thread_for_session(
                &session.id,
                "Clarify work".to_string(),
                ".".to_string(),
                "deepseek-v4-flash".to_string(),
                "agent".to_string(),
            )
            .unwrap();
        let responder_store = store.clone();
        let responder_thread_id = thread.id.clone();
        let responder = std::thread::spawn(move || {
            for _ in 0..50 {
                let events = responder_store
                    .read_events(&responder_thread_id, 0)
                    .expect("events should read");
                if let Some(request) = events
                    .iter()
                    .find(|event| event.kind == "user_input_request")
                {
                    responder_store
                        .append_user_input_response(
                            &responder_thread_id,
                            None,
                            request.id.clone(),
                            std::collections::BTreeMap::from([(
                                "mode".to_string(),
                                "Plan".to_string(),
                            )]),
                        )
                        .expect("response should append");
                    return;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            panic!("user input request was not written");
        });

        let mut resolver = RuntimeTaskUserInputResolver {
            store: store.clone(),
            thread_id: thread.id.clone(),
            poll_interval: Duration::from_millis(5),
            max_polls: Some(200),
        };
        let questions = r#"[{"header":"Mode","id":"mode","question":"Which mode?","options":[{"label":"Plan","description":"Plan first."},{"label":"Apply","description":"Implement directly."}]}]"#;
        let response = resolver
            .resolve(&AgentUserInputRequest {
                input: std::collections::BTreeMap::from([(
                    "questions".to_string(),
                    questions.to_string(),
                )]),
            })
            .unwrap();
        responder.join().unwrap();

        assert_eq!(
            response.answers.get("mode").map(String::as_str),
            Some("Plan")
        );
        let events = store.read_events(&thread.id, 0).unwrap();
        assert!(events
            .iter()
            .any(|event| event.kind == "user_input_request"));
        assert!(events
            .iter()
            .any(|event| event.kind == "user_input_response"));
    }

    #[test]
    fn record_runtime_task_result_writes_turns_items_usage_and_task_status() {
        let store = RuntimeStore::new(temp_root("runtime-task-result"));
        let session = store
            .create_session("Runtime runner".to_string(), ".".to_string())
            .unwrap();
        let thread = store
            .create_thread_for_session(
                &session.id,
                "Queued work".to_string(),
                ".".to_string(),
                "deepseek-v4-flash".to_string(),
                "agent".to_string(),
            )
            .unwrap();
        let task = store
            .create_task(
                Some(&session.id),
                Some(&thread.id),
                None,
                "automation".to_string(),
                "running".to_string(),
                "run queued work".to_string(),
            )
            .unwrap();
        let mut usage = crate::model::protocol::TokenUsage::new(8, 2);
        usage.model = Some("deepseek-v4-flash".to_string());
        let result = RunResult {
            final_message: "done".to_string(),
            tool_events: vec![ToolEvent {
                tool_name: "read_file".to_string(),
                input: std::collections::BTreeMap::from([(
                    "path".to_string(),
                    "README.md".to_string(),
                )]),
                output: "ok".to_string(),
                status: ObservationStatus::Ok,
            }],
            usage,
        };

        let assistant_turn_id =
            record_runtime_task_result(&store, &task, &thread, &result).unwrap();

        assert!(assistant_turn_id.starts_with("turn-"));
        let updated = store.load_task(&task.id).unwrap();
        assert_eq!(updated.status, "completed");
        assert_eq!(updated.summary, "done");
        let turns = store.list_turns(&thread.id).unwrap();
        assert_eq!(turns.len(), 2);
        assert_eq!(turns[0].role, "user");
        assert_eq!(turns[1].role, "assistant");
        let items = store.list_items(&thread.id, None).unwrap();
        assert_eq!(items.len(), 3);
        assert_eq!(items[2].item_type, "tool_result");
        assert!(items[2].content.contains("tool: read_file"));
        let usage = store.list_usage(Some(&thread.id), 10).unwrap();
        assert_eq!(usage.len(), 1);
        assert_eq!(usage[0].source, "runtime_runner");
        assert_eq!(usage[0].total_tokens, 10);
        let events = store.read_events(&thread.id, 0).unwrap();
        assert!(events.iter().any(|event| event.kind == "task_updated"));
        assert!(events.iter().any(|event| event.kind == "usage_recorded"));
    }

    #[test]
    fn record_runtime_task_failure_writes_failed_item_and_status() {
        let store = RuntimeStore::new(temp_root("runtime-task-failure"));
        let thread = store
            .create_thread(
                "Queued work".to_string(),
                ".".to_string(),
                "deepseek-v4-flash".to_string(),
                "agent".to_string(),
            )
            .unwrap();
        let task = store
            .create_task(
                None,
                Some(&thread.id),
                None,
                "automation".to_string(),
                "running".to_string(),
                "run queued work".to_string(),
            )
            .unwrap();

        record_runtime_task_failure(&store, &task, &thread, "boom").unwrap();

        let updated = store.load_task(&task.id).unwrap();
        assert_eq!(updated.status, "failed");
        assert!(updated.summary.contains("boom"));
        let items = store.list_items(&thread.id, None).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].status, "failed");
        assert!(items[0].content.contains("runtime task failed"));
    }

    #[test]
    fn run_runtime_task_executes_pending_thread_task_end_to_end() {
        let root = temp_root("runtime-task-run");
        let workspace = root.join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::write(workspace.join("README.md"), "hello runtime task\n").unwrap();
        let config_dir = root.join(".dscode");
        let mut config = AppConfig::default();
        config.workspace.config_dir = config_dir.display().to_string();
        config.workspace.session_dir = config_dir.join("sessions").display().to_string();
        config.model.api_key_env = "DSCODE_TEST_NO_KEY".to_string();
        let store = RuntimeStore::new(config_dir.join("runtime"));
        let session = store
            .create_session(
                "Runtime runner".to_string(),
                workspace.display().to_string(),
            )
            .unwrap();
        let thread = store
            .create_thread_for_session(
                &session.id,
                "Queued work".to_string(),
                workspace.display().to_string(),
                "deepseek-coder".to_string(),
                "agent".to_string(),
            )
            .unwrap();
        let task = store
            .create_task(
                Some(&session.id),
                Some(&thread.id),
                None,
                "automation".to_string(),
                "pending".to_string(),
                "inspect repository layout".to_string(),
            )
            .unwrap();
        let original_dir = {
            let _cwd_lock = crate::util::cwd::lock_cwd().unwrap();
            std::env::current_dir().unwrap()
        };

        run_runtime_task(config, &task.id, Some(1), true).unwrap();

        let restored_dir = {
            let _cwd_lock = crate::util::cwd::lock_cwd().unwrap();
            std::env::current_dir().unwrap()
        };
        assert_eq!(restored_dir, original_dir);
        let updated = store.load_task(&task.id).unwrap();
        assert_eq!(updated.status, "completed");
        let turns = store.list_turns(&thread.id).unwrap();
        assert_eq!(turns.len(), 2);
        let items = store.list_items(&thread.id, None).unwrap();
        assert!(items
            .iter()
            .any(|item| item.item_type == "tool_result" && item.content.contains("README.md")));
        let events = store.read_events(&thread.id, 0).unwrap();
        assert!(events.iter().any(|event| event.kind == "task_claimed"));
        assert!(events.iter().any(|event| event.kind == "task_updated"));
    }

    #[test]
    fn daemon_schedule_parser_accepts_common_interval_shapes() {
        assert_eq!(parse_schedule_interval_seconds("every:5m"), Some(300));
        assert_eq!(parse_schedule_interval_seconds("@every 2h"), Some(7_200));
        assert_eq!(parse_schedule_interval_seconds("interval:30s"), Some(30));
        assert_eq!(parse_schedule_interval_seconds("manual"), None);
        assert_eq!(parse_schedule_interval_seconds("once"), None);
        assert_eq!(parse_schedule_interval_seconds("0s"), None);
    }

    #[test]
    fn runtime_daemon_tick_executes_pending_thread_task() {
        let root = temp_root("runtime-daemon-task");
        let workspace = root.join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::write(workspace.join("README.md"), "hello runtime daemon\n").unwrap();
        let config_dir = root.join(".dscode");
        let mut config = AppConfig::default();
        config.workspace.config_dir = config_dir.display().to_string();
        config.workspace.session_dir = config_dir.join("sessions").display().to_string();
        config.model.api_key_env = "DSCODE_TEST_NO_KEY".to_string();
        let store = RuntimeStore::new(config_dir.join("runtime"));
        let thread = store
            .create_thread(
                "Queued work".to_string(),
                workspace.display().to_string(),
                "deepseek-coder".to_string(),
                "agent".to_string(),
            )
            .unwrap();
        let task = store
            .create_task(
                None,
                Some(&thread.id),
                None,
                "manual".to_string(),
                "pending".to_string(),
                "inspect repository layout".to_string(),
            )
            .unwrap();

        let tick = run_runtime_daemon_tick(&config, &store, Some(1), true).unwrap();

        assert_eq!(tick.executed_tasks, 1);
        assert_eq!(tick.triggered_automations, 0);
        let updated = store.load_task(&task.id).unwrap();
        assert_eq!(updated.status, "completed");
    }

    #[test]
    fn runtime_daemon_tick_routes_live_rlm_turns_through_rlm_worker() {
        let root = temp_root("runtime-daemon-rlm-live");
        let workspace = root.join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::write(workspace.join("README.md"), "hello live rlm daemon\n").unwrap();
        let config_dir = root.join(".dscode");
        let mut config = AppConfig::default();
        config.workspace.config_dir = config_dir.display().to_string();
        config.workspace.session_dir = config_dir.join("sessions").display().to_string();
        config.model.api_key_env = "DSCODE_TEST_NO_KEY".to_string();
        let store = RuntimeStore::new(config_dir.join("runtime"));
        let queued = crate::tools::rlm::RlmTool {
            tool_name: "rlm_process",
            config: config.clone(),
            parent_depth: 0,
        }
        .execute(
            ToolInput::new()
                .with_arg("task", "summarize live daemon payload")
                .with_arg("content", "live daemon payload")
                .with_arg("session_id", "daemon.rlm")
                .with_arg("live", "true")
                .with_arg("cwd", workspace.display().to_string()),
        )
        .unwrap();
        let turn_id = meta_value(&queued.summary, "meta.rlm_turn_id").unwrap();

        let tick = run_runtime_daemon_tick(&config, &store, Some(1), true).unwrap();

        assert_eq!(tick.executed_rlm_turns, 1);
        assert_eq!(tick.executed_tasks, 0);
        let updated = store.load_task(&turn_id).unwrap();
        assert_eq!(updated.status, "completed");
        let events = crate::tools::rlm::RlmLiveEventsTool {
            config: config.clone(),
        }
        .execute(ToolInput::new().with_arg("session_id", "daemon.rlm"))
        .unwrap();
        assert!(events.summary.contains(r#""kind":"turn_started""#));
        assert!(events.summary.contains(r#""kind":"turn_completed""#));
    }

    #[test]
    fn runtime_daemon_tick_recovers_stale_live_rlm_owner_before_running_queue() {
        let root = temp_root("runtime-daemon-rlm-stale-owner");
        let workspace = root.join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::write(workspace.join("README.md"), "hello stale rlm owner\n").unwrap();
        let config_dir = root.join(".dscode");
        let mut config = AppConfig::default();
        config.workspace.config_dir = config_dir.display().to_string();
        config.workspace.session_dir = config_dir.join("sessions").display().to_string();
        config.model.api_key_env = "DSCODE_TEST_NO_KEY".to_string();
        let store = RuntimeStore::new(config_dir.join("runtime"));
        let queued = crate::tools::rlm::RlmTool {
            tool_name: "rlm_process",
            config: config.clone(),
            parent_depth: 0,
        }
        .execute(
            ToolInput::new()
                .with_arg("task", "recover stale daemon owner payload")
                .with_arg("content", "stale owner payload")
                .with_arg("session_id", "daemon.stale")
                .with_arg("live", "true")
                .with_arg("cwd", workspace.display().to_string()),
        )
        .unwrap();
        let turn_id = meta_value(&queued.summary, "meta.rlm_turn_id").unwrap();
        let thread_id = meta_value(&queued.summary, "meta.rlm_runtime_thread_id").unwrap();
        store
            .claim_task(&turn_id, "test-stale-owner".to_string())
            .unwrap();
        let manifest_dir = config_dir.join("rlm-daemon").join("daemon.stale");
        std::fs::create_dir_all(&manifest_dir).unwrap();
        std::fs::write(
            manifest_dir.join("manifest.json"),
            format!(
                r#"{{"session_id":"daemon.stale","status":"running","daemon_pid":{},"daemon_epoch":"epoch+stale","runtime_thread_id":"{}","runtime_session_id":null,"active_turn_id":"{}","queued_turns":0,"model":"deepseek-coder","workspace":"{}","created_at":"epoch+1","updated_at":"epoch+2","last_error":null}}"#,
                i32::MAX as u64 + 1,
                thread_id,
                turn_id,
                workspace.display()
            ),
        )
        .unwrap();

        let tick = run_runtime_daemon_tick(&config, &store, Some(1), true).unwrap();

        assert_eq!(tick.recovered_rlm_turns, 1);
        assert_eq!(tick.executed_rlm_turns, 1);
        assert_eq!(tick.failed_rlm_recoveries, 0);
        let updated = store.load_task(&turn_id).unwrap();
        assert_eq!(updated.status, "completed");
        let events = crate::tools::rlm::RlmLiveEventsTool {
            config: config.clone(),
        }
        .execute(ToolInput::new().with_arg("session_id", "daemon.stale"))
        .unwrap();
        assert!(events.summary.contains(r#""kind":"turn_recovered""#));
        assert!(events.summary.contains(r#""kind":"turn_completed""#));
    }

    struct RecordingSummaryClient {
        request: std::cell::RefCell<Option<ModelRequest>>,
        message: String,
    }

    impl ModelClient for RecordingSummaryClient {
        fn respond(
            &self,
            input: ModelRequest,
            _events: &mut dyn crate::ui::stream::StreamEvents,
        ) -> AppResult<(
            crate::model::protocol::ModelResponse,
            Option<crate::model::protocol::TokenUsage>,
        )> {
            *self.request.borrow_mut() = Some(input);
            Ok((
                crate::model::protocol::ModelResponse {
                    message: self.message.clone(),
                    action: ModelAction::Finish,
                },
                None,
            ))
        }
    }

    #[test]
    fn model_compaction_summary_request_captures_prior_context() {
        let store = RuntimeStore::new(temp_root("model-compact-request"));
        let thread = store
            .create_thread(
                "Model compact".to_string(),
                ".".to_string(),
                "deepseek-v4-flash".to_string(),
                "agent".to_string(),
            )
            .unwrap();
        store
            .append_turn(
                &thread.id,
                "user".to_string(),
                "key decision: keep the Rust CLI local-first".to_string(),
            )
            .unwrap();
        store
            .append_turn(
                &thread.id,
                "assistant".to_string(),
                "implemented runtime state and wrote docs".to_string(),
            )
            .unwrap();
        store
            .append_turn(&thread.id, "user".to_string(), "tail request".to_string())
            .unwrap();
        store
            .append_turn(
                &thread.id,
                "assistant".to_string(),
                "tail answer".to_string(),
            )
            .unwrap();
        let turns = store.list_turns(&thread.id).unwrap();
        let client = RecordingSummaryClient {
            request: std::cell::RefCell::new(None),
            message: "Model summary: local-first Rust CLI, runtime docs done.".to_string(),
        };

        let summary = model_compaction_summary_with_client(&client, &thread, &turns, 2).unwrap();

        assert_eq!(
            summary,
            "Model summary: local-first Rust CLI, runtime docs done."
        );
        let request = client.request.borrow();
        let request = request.as_ref().expect("expected model request");
        assert_eq!(request.profile_name, "runtime-compaction");
        assert!(request.available_tools.is_empty());
        assert!(!request.planning_mode);
        assert!(request.system_prompt.contains("automatic compaction"));
        assert!(request.task.contains("Thread title: Model compact"));
        assert!(request
            .task
            .contains("key decision: keep the Rust CLI local-first"));
        assert!(request.task.contains("Tail turns preserved verbatim: 2"));
        assert!(request.task.contains("The live tail begins at turn #3"));
    }

    #[test]
    fn runtime_daemon_tick_compacts_threads_after_usage_warning() {
        let root = temp_root("runtime-daemon-compact");
        let config_dir = root.join(".dscode");
        let mut config = AppConfig::default();
        config.workspace.config_dir = config_dir.display().to_string();
        config.model.api_key_env = "DSCODE_TEST_NO_KEY".to_string();
        let store = RuntimeStore::new(config_dir.join("runtime"));
        let thread = store
            .create_thread(
                "Long context".to_string(),
                ".".to_string(),
                "deepseek-v4-flash".to_string(),
                "agent".to_string(),
            )
            .unwrap();
        let mut latest_turn_id = String::new();
        for index in 1..=10 {
            latest_turn_id = store
                .append_turn(&thread.id, "assistant".to_string(), format!("turn {index}"))
                .unwrap()
                .id;
        }
        store
            .append_usage_with_cache(
                &thread.id,
                Some(&latest_turn_id),
                "deepseek-v4-flash".to_string(),
                "test".to_string(),
                850_000,
                25,
                200_000,
                650_000,
            )
            .unwrap();

        let first_tick = run_runtime_daemon_tick(&config, &store, None, false).unwrap();
        let second_tick = run_runtime_daemon_tick(&config, &store, None, false).unwrap();

        assert_eq!(first_tick.compacted_threads, 1);
        assert_eq!(second_tick.compacted_threads, 0);
        let events = store.read_events(&thread.id, 0).unwrap();
        assert_eq!(
            events
                .iter()
                .filter(|event| event.kind == "thread_compacted")
                .count(),
            1
        );
    }

    #[test]
    fn runtime_daemon_compaction_uses_model_summary_provider() {
        let root = temp_root("runtime-daemon-model-compact");
        let config_dir = root.join(".dscode");
        let store = RuntimeStore::new(config_dir.join("runtime"));
        let thread = store
            .create_thread(
                "Long model context".to_string(),
                ".".to_string(),
                "deepseek-v4-flash".to_string(),
                "agent".to_string(),
            )
            .unwrap();
        let mut latest_turn_id = String::new();
        for index in 1..=10 {
            latest_turn_id = store
                .append_turn(&thread.id, "assistant".to_string(), format!("turn {index}"))
                .unwrap()
                .id;
        }
        store
            .append_usage_with_cache(
                &thread.id,
                Some(&latest_turn_id),
                "deepseek-v4-flash".to_string(),
                "test".to_string(),
                850_000,
                25,
                200_000,
                650_000,
            )
            .unwrap();
        let mut tick = RuntimeDaemonTick::default();
        let mut called = 0usize;

        run_runtime_daemon_compactions_with_summary_provider(
            &store,
            false,
            &mut tick,
            |_store, _thread| {
                called += 1;
                Ok(Some("Generated model context summary".to_string()))
            },
        )
        .unwrap();

        assert_eq!(called, 1);
        assert_eq!(tick.compacted_threads, 1);
        let items = store.list_items(&thread.id, None).unwrap();
        assert!(items.iter().any(|item| {
            item.item_type == "summary" && item.content == "Generated model context summary"
        }));
        let events = store.read_events(&thread.id, 0).unwrap();
        let compaction_event = events
            .iter()
            .find(|event| event.kind == "thread_compacted")
            .expect("expected compaction event");
        let JsonValue::Object(payload) = &compaction_event.payload else {
            panic!("expected object payload");
        };
        assert_eq!(
            payload
                .get("summary_source")
                .and_then(crate::util::json::json_as_string),
            Some("model")
        );
    }

    #[test]
    fn runtime_daemon_tick_triggers_due_automation_and_runs_task() {
        let root = temp_root("runtime-daemon-automation");
        let workspace = root.join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::write(workspace.join("README.md"), "hello automation\n").unwrap();
        let config_dir = root.join(".dscode");
        let mut config = AppConfig::default();
        config.workspace.config_dir = config_dir.display().to_string();
        config.workspace.session_dir = config_dir.join("sessions").display().to_string();
        config.model.api_key_env = "DSCODE_TEST_NO_KEY".to_string();
        let store = RuntimeStore::new(config_dir.join("runtime"));
        let session = store
            .create_session(
                "Runtime daemon".to_string(),
                workspace.display().to_string(),
            )
            .unwrap();
        let thread = store
            .create_thread_for_session(
                &session.id,
                "Scheduled work".to_string(),
                workspace.display().to_string(),
                "deepseek-coder".to_string(),
                "agent".to_string(),
            )
            .unwrap();
        let due_at = format_epoch_seconds(current_epoch_seconds().saturating_sub(1));
        let automation = store
            .create_automation(
                Some(&session.id),
                Some(&thread.id),
                "Nightly check".to_string(),
                "active".to_string(),
                "every:60s".to_string(),
                "inspect repository layout".to_string(),
                None,
                Some(due_at),
            )
            .unwrap();

        let tick = run_runtime_daemon_tick(&config, &store, Some(1), true).unwrap();

        assert_eq!(tick.triggered_automations, 1);
        assert_eq!(tick.executed_tasks, 1);
        let tasks = store
            .list_tasks(Some(&session.id), Some(&thread.id), 10)
            .unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].status, "completed");
        let updated_automation = store.load_automation(&automation.id).unwrap();
        assert!(updated_automation.last_run_at.is_some());
        assert!(updated_automation.next_run_at.is_some());
        assert_ne!(updated_automation.next_run_at.as_deref(), Some("epoch+0"));
        let events = store.read_events(&thread.id, 0).unwrap();
        assert!(events
            .iter()
            .any(|event| event.kind == "automation_triggered"));
        assert!(events
            .iter()
            .any(|event| event.kind == "automation_scheduled"));
    }

    fn temp_root(name: &str) -> std::path::PathBuf {
        let suffix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "deepseek-agents-{name}-{}-{suffix}",
            std::process::id()
        ))
    }

    #[cfg(unix)]
    fn shell_supervisor_stream_roundtrip(request: &str) -> (bool, String) {
        let (mut client, server) = std::os::unix::net::UnixStream::pair().unwrap();
        let handle = std::thread::spawn(move || {
            handle_shell_supervisor_stream(
                server,
                Path::new("/work/repo"),
                Path::new("/work/repo/.dscode/shell-supervisor/supervisor.sock"),
                "epoch+1",
            )
            .unwrap()
        });

        client.write_all(request.as_bytes()).unwrap();
        client.write_all(b"\n").unwrap();
        let mut response = String::new();
        let mut reader = BufReader::new(&mut client);
        reader.read_line(&mut response).unwrap();
        let shutdown = handle.join().unwrap();
        (shutdown, response)
    }

    fn meta_value(summary: &str, key: &str) -> Option<String> {
        summary
            .lines()
            .find_map(|line| line.strip_prefix(&format!("{key}=")))
            .map(str::trim)
            .map(str::to_string)
    }
}
