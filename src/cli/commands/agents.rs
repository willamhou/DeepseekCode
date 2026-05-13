use std::cell::RefCell;
use std::path::Path;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::cli::app::{AgentsAction, AgentsServiceArgs, AgentsServiceKind};
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
use crate::tools::rlm::{rlm_live_session_ids_by_runtime_thread, RlmLiveRunNextTool};
use crate::tools::types::{Tool, ToolInput};
use crate::util::json::{json_as_object, json_as_string, parse_json_value};
use crate::util::json::{json_value_to_string, JsonValue};

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
        AgentsAction::Service(args) => render_agent_services(args),
        AgentsAction::Threads => list_threads(&config.workspace.config_dir),
        AgentsAction::ShowThread { id } => show_thread(&config.workspace.config_dir, &id),
        AgentsAction::SwitchThread { id } => switch_thread(&config.workspace.config_dir, &id),
        AgentsAction::CurrentThread => current_thread(&config.workspace.config_dir),
        AgentsAction::ClearThread => clear_thread(&config.workspace.config_dir),
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
    compacted_threads: usize,
    failed_automations: usize,
    failed_tasks: usize,
    failed_rlm_turns: usize,
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
            || tick.compacted_threads > 0
            || tick.failed_automations > 0
            || tick.failed_tasks > 0
            || tick.failed_rlm_turns > 0
            || tick.failed_compactions > 0
        {
            println!(
                "daemon tick: triggered={} executed={} rlm_executed={} compacted={} automation_errors={} task_errors={} rlm_errors={} compaction_errors={}",
                tick.triggered_automations,
                tick.executed_tasks,
                tick.executed_rlm_turns,
                tick.compacted_threads,
                tick.failed_automations,
                tick.failed_tasks,
                tick.failed_rlm_turns,
                tick.failed_compactions
            );
        }

        if once {
            return Ok(());
        }
        std::thread::sleep(interval);
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
    }
    templates
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
    )
}

fn launchd_plist(
    label: &str,
    workdir: &str,
    args: &[String],
    stdout_path: &str,
    stderr_path: &str,
) -> String {
    let mut body = String::new();
    body.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    body.push_str("<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" ");
    body.push_str("\"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n");
    body.push_str("<plist version=\"1.0\">\n<dict>\n");
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
                "next: install systemd/*.service into ~/.config/systemd/user, then run `systemctl --user daemon-reload`"
            );
        }
        AgentsServiceKind::Launchd => {
            println!(
                "next: install launchd/*.plist into ~/Library/LaunchAgents, then load with `launchctl load -w <plist>`"
            );
        }
        AgentsServiceKind::All => {
            println!(
                "next: choose the files under {} for your supervisor and install them with the platform tool",
                out.display()
            );
        }
    }
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
        "failed_compactions".to_string(),
        JsonValue::Number(tick.failed_compactions.to_string()),
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
                "launchd/com.deepseek.runtime.plist",
                "launchd/com.deepseek.agents.plist",
                "launchd/com.deepseek.diagnostics.plist",
            ]
        );
        assert!(templates[0]
            .body
            .contains("serve --http --addr 127.0.0.1:9876"));
        assert!(templates[1]
            .body
            .contains("agents daemon --interval-ms 750 --budget 6 --json"));
        assert!(templates[2]
            .body
            .contains("diagnostics --watch --changed --interval-ms 750 --json"));
        assert!(templates[3]
            .body
            .contains("<string>com.deepseek.runtime</string>"));
        assert!(templates[4]
            .body
            .contains("<string>com.deepseek.agents</string>"));
        assert!(templates[5]
            .body
            .contains("<string>com.deepseek.diagnostics</string>"));
        assert!(templates[5].body.contains("<string>--json</string>"));
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

    fn meta_value(summary: &str, key: &str) -> Option<String> {
        summary
            .lines()
            .find_map(|line| line.strip_prefix(&format!("{key}=")))
            .map(str::trim)
            .map(str::to_string)
    }
}
