use crate::error::{app_error, AppResult};
use crate::tools::run_shell::{
    apply_shell_env, is_safe_shell_command, shell_env_from_input, RunShellTool,
};
use crate::tools::types::{Tool, ToolInput, ToolOutput};
use crate::util::json::{
    json_as_string, json_as_u64, json_value_to_string, parse_root_object, JsonValue,
};
use std::collections::BTreeMap;
use std::error::Error;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const DEFAULT_WAIT_MS: u64 = 5_000;
const MAX_TIMEOUT_MS: u64 = 600_000;
const DEFAULT_REPLAY_LIMIT_BYTES: u64 = 20_000;
const MAX_REPLAY_LIMIT_BYTES: u64 = 100_000;
const PTY_BACKEND_SCRIPT: &str = "script";
const PTY_BACKEND_NONE: &str = "none";
pub const SHELL_SUPERVISOR_SUPPORTED_METHODS: &[&str] = &["health", "status", "show", "shutdown"];
pub const SHELL_SUPERVISOR_UNSUPPORTED_PTY_METHODS: &[&str] = &[
    "start", "wait", "replay", "attach", "stdin", "resize", "cancel",
];

static JOB_COUNTER: AtomicU64 = AtomicU64::new(0);
static SHELL_JOBS: OnceLock<Mutex<BackgroundShellManager>> = OnceLock::new();

pub struct ExecShellTool;

pub struct ExecShellWaitTool {
    pub tool_name: &'static str,
}

pub struct ExecShellListTool;

pub struct ExecShellShowTool;

pub struct ExecShellReplayTool;

pub struct ExecShellAttachTool;

pub struct ExecShellSupervisorStatusTool;

pub struct ExecShellResizeTool;

pub struct ExecShellInteractTool {
    pub tool_name: &'static str,
}

pub struct ExecShellCancelTool;

pub struct TaskShellStartTool;

pub struct TaskShellWaitTool;

pub fn run_trusted_background_shell(command: &str, cwd: &str) -> AppResult<ToolOutput> {
    let command = command.trim();
    if command.is_empty() {
        return Err(app_error("trusted background shell requires a command"));
    }
    let task_id =
        shell_manager()
            .lock()
            .unwrap()
            .spawn(command, cwd, None, ShellTtyOptions::default())?;
    Ok(ToolOutput {
        summary: format!(
            "task_id: {task_id}\nstatus: running\ncommand: {command}\ncwd: {cwd}\ntrusted_foreground_approval: true\nPoll with exec_shell_wait task_id={task_id} or cancel with exec_shell_cancel task_id={task_id}."
        ),
    })
}

impl Tool for ExecShellTool {
    fn name(&self) -> &str {
        "exec_shell"
    }

    fn execute(&self, input: ToolInput) -> AppResult<ToolOutput> {
        let command = input
            .get("command")
            .ok_or_else(|| app_error("exec_shell requires a command"))?;
        if !is_safe_shell_command(command) {
            return Err(app_error(format!("command not allowed: {command}")));
        }
        let shell_env = shell_env_from_input(&input);
        let background = truthy(input.get("background"));
        let tty_options = parse_tty_options(&input)?;
        if !background {
            if tty_options.requested() {
                return Err(app_error("exec_shell tty options require background=true"));
            }
            if let Some(detach_after_ms) = foreground_detach_after_ms(&input)? {
                let cwd = input.get("cwd").unwrap_or(".");
                let stdin = input
                    .get("stdin")
                    .or_else(|| input.get("input"))
                    .or_else(|| input.get("data"));
                return run_managed_foreground_shell(
                    command,
                    cwd,
                    stdin,
                    detach_after_ms,
                    &shell_env,
                );
            }
            let mut shell_input = ToolInput::new().with_arg("command", command.to_string());
            if let Some(cwd) = input.get("cwd") {
                shell_input = shell_input.with_arg("cwd", cwd.to_string());
            }
            for (key, value) in &shell_env {
                shell_input = shell_input.with_arg(format!("env.{key}"), value.clone());
            }
            return RunShellTool.execute(shell_input);
        }

        let cwd = input.get("cwd").unwrap_or(".");
        let stdin = input
            .get("stdin")
            .or_else(|| input.get("input"))
            .or_else(|| input.get("data"));
        let task_id = shell_manager().lock().unwrap().spawn_with_env(
            command,
            cwd,
            stdin,
            tty_options,
            &shell_env,
        )?;
        Ok(ToolOutput {
            summary: format!(
                "task_id: {task_id}\nstatus: running\ncommand: {command}\ncwd: {cwd}\ntty: {}\npty_backend: {}\nattachable: false\nresizable: false\ntty_rows: {}\ntty_cols: {}\nPoll with exec_shell_wait task_id={task_id} or cancel with exec_shell_cancel task_id={task_id}.",
                tty_options.enabled,
                pty_backend_label(tty_options),
                tty_rows_label(tty_options.size),
                tty_cols_label(tty_options.size)
            ),
        })
    }
}

fn run_managed_foreground_shell(
    command: &str,
    cwd: &str,
    stdin: Option<&str>,
    detach_after_ms: u64,
    env: &BTreeMap<String, String>,
) -> AppResult<ToolOutput> {
    let task_id = shell_manager().lock().unwrap().spawn_with_env(
        command,
        cwd,
        stdin,
        ShellTtyOptions::default(),
        env,
    )?;
    let deadline = Instant::now() + Duration::from_millis(detach_after_ms.min(MAX_TIMEOUT_MS));
    loop {
        let mut manager = shell_manager().lock().unwrap();
        manager.refresh(&task_id)?;
        if manager.is_finished(&task_id)? {
            let snapshot = manager.render_snapshot(&task_id)?;
            return Ok(ToolOutput {
                summary: format!(
                    "meta.foreground_managed=true\nmeta.backgrounded=false\nmeta.detach_after_ms={detach_after_ms}\n{snapshot}"
                ),
            });
        }
        if Instant::now() >= deadline {
            let delta = manager.render_delta(&task_id)?;
            return Ok(ToolOutput {
                summary: format!(
                    "meta.foreground_managed=true\nmeta.backgrounded=true\nmeta.detach_after_ms={detach_after_ms}\n{delta}\nPoll with exec_shell_wait task_id={task_id} or cancel with exec_shell_cancel task_id={task_id}."
                ),
            });
        }
        drop(manager);
        thread::sleep(Duration::from_millis(25));
    }
}

impl Tool for TaskShellStartTool {
    fn name(&self) -> &str {
        "task_shell_start"
    }

    fn execute(&self, input: ToolInput) -> AppResult<ToolOutput> {
        let command = input
            .get("command")
            .ok_or_else(|| app_error("task_shell_start requires a command"))?;
        let mut shell_input = ToolInput::new()
            .with_arg("command", command.to_string())
            .with_arg("background", "true");
        if let Some(cwd) = input.get("cwd") {
            shell_input = shell_input.with_arg("cwd", cwd.to_string());
        }
        if let Some(stdin) = input.get("stdin").or_else(|| input.get("input")) {
            shell_input = shell_input.with_arg("stdin", stdin.to_string());
        }
        if let Some(timeout_ms) = input.get("timeout_ms") {
            shell_input = shell_input.with_arg("timeout_ms", timeout_ms.to_string());
        }
        let tty = truthy(input.get("tty"));
        if tty {
            shell_input = shell_input.with_arg("tty", "true");
        }
        if let Some(tty_rows) = input.get("tty_rows") {
            shell_input = shell_input.with_arg("tty_rows", tty_rows.to_string());
        }
        if let Some(tty_cols) = input.get("tty_cols") {
            shell_input = shell_input.with_arg("tty_cols", tty_cols.to_string());
        }
        for (key, value) in &input.args {
            if key.starts_with("env.") {
                shell_input = shell_input.with_arg(key.clone(), value.clone());
            }
        }
        let mut output = ExecShellTool.execute(shell_input)?;
        output.summary = output
            .summary
            .replace("Poll with exec_shell_wait", "Poll with task_shell_wait");
        output.summary.push_str("\nmeta.task_shell=true");
        if tty {
            output.summary.push_str("\nmeta.tty=true");
        } else if input.get("tty").is_some() {
            output.summary.push_str("\nmeta.tty=false");
        }
        Ok(output)
    }
}

impl Tool for TaskShellWaitTool {
    fn name(&self) -> &str {
        "task_shell_wait"
    }

    fn execute(&self, input: ToolInput) -> AppResult<ToolOutput> {
        let mut output = ExecShellWaitTool {
            tool_name: "task_shell_wait",
        }
        .execute(input.clone())?;
        if let Some(gate) = input
            .get("gate")
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            output.summary = format!("meta.gate={gate}\n{}", output.summary);
        }
        if let Some(command) = input
            .get("command")
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            output.summary = format!("meta.command={command}\n{}", output.summary);
        }
        Ok(output)
    }
}

impl Tool for ExecShellWaitTool {
    fn name(&self) -> &str {
        self.tool_name
    }

    fn execute(&self, input: ToolInput) -> AppResult<ToolOutput> {
        let task_id = required_task_id(&input)?;
        let cwd = input.get("cwd").unwrap_or(".");
        let wait = input.get("wait").map_or(true, |value| truthy(Some(value)));
        let timeout_ms = input_u64(&input, "timeout_ms", DEFAULT_WAIT_MS).min(MAX_TIMEOUT_MS);
        let deadline = Instant::now() + Duration::from_millis(timeout_ms);
        loop {
            let mut manager = shell_manager().lock().unwrap();
            if !manager.contains(task_id) {
                return Ok(ToolOutput {
                    summary: render_durable_snapshot(cwd, task_id)?,
                });
            }
            manager.refresh(task_id)?;
            if !wait || manager.is_finished(task_id)? || Instant::now() >= deadline {
                return Ok(ToolOutput {
                    summary: manager.render_delta(task_id)?,
                });
            }
            drop(manager);
            thread::sleep(Duration::from_millis(25));
        }
    }
}

impl Tool for ExecShellListTool {
    fn name(&self) -> &str {
        "exec_shell_list"
    }

    fn execute(&self, input: ToolInput) -> AppResult<ToolOutput> {
        let cwd = input.get("cwd").unwrap_or(".");
        let mut manager = shell_manager().lock().unwrap();
        manager.refresh_all()?;
        Ok(ToolOutput {
            summary: manager.render_list(cwd)?,
        })
    }
}

impl Tool for ExecShellShowTool {
    fn name(&self) -> &str {
        "exec_shell_show"
    }

    fn execute(&self, input: ToolInput) -> AppResult<ToolOutput> {
        let task_id = required_task_id(&input)?;
        let cwd = input.get("cwd").unwrap_or(".");
        let mut manager = shell_manager().lock().unwrap();
        if !manager.contains(task_id) {
            return Ok(ToolOutput {
                summary: render_durable_snapshot(cwd, task_id)?,
            });
        }
        manager.refresh(task_id)?;
        Ok(ToolOutput {
            summary: manager.render_snapshot(task_id)?,
        })
    }
}

impl Tool for ExecShellReplayTool {
    fn name(&self) -> &str {
        "exec_shell_replay"
    }

    fn execute(&self, input: ToolInput) -> AppResult<ToolOutput> {
        let task_id = required_task_id(&input)?;
        let cwd = input.get("cwd").unwrap_or(".");
        let stream = input
            .get("stream")
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("stdout");
        let offset = input_u64(&input, "offset", 0) as usize;
        let limit_bytes = input_u64(&input, "limit_bytes", DEFAULT_REPLAY_LIMIT_BYTES)
            .min(MAX_REPLAY_LIMIT_BYTES) as usize;
        let tail = truthy(input.get("tail"));
        Ok(ToolOutput {
            summary: render_shell_replay(cwd, task_id, stream, offset, limit_bytes, tail)?,
        })
    }
}

impl Tool for ExecShellAttachTool {
    fn name(&self) -> &str {
        "exec_shell_attach"
    }

    fn execute(&self, input: ToolInput) -> AppResult<ToolOutput> {
        let task_id = required_task_id(&input)?;
        let cwd = input.get("cwd").unwrap_or(".");
        let offset = input_u64(&input, "offset", 0) as usize;
        let cursor = input_u64(&input, "cursor", offset as u64) as usize;
        let limit_bytes = input_u64(&input, "limit_bytes", DEFAULT_REPLAY_LIMIT_BYTES)
            .min(MAX_REPLAY_LIMIT_BYTES) as usize;
        let tail = truthy(input.get("tail"));
        let wait_ms = input_u64(&input, "wait_ms", 0).min(MAX_TIMEOUT_MS);
        {
            let mut manager = shell_manager().lock().unwrap();
            if manager.contains(task_id) {
                manager.refresh(task_id)?;
            }
        }
        Ok(ToolOutput {
            summary: render_shell_attach(cwd, task_id, cursor, limit_bytes, tail, wait_ms)?,
        })
    }
}

impl Tool for ExecShellSupervisorStatusTool {
    fn name(&self) -> &str {
        "exec_shell_supervisor_status"
    }

    fn execute(&self, input: ToolInput) -> AppResult<ToolOutput> {
        let cwd = input.get("cwd").unwrap_or(".");
        Ok(ToolOutput {
            summary: render_shell_supervisor_status(cwd)?,
        })
    }
}

impl Tool for ExecShellResizeTool {
    fn name(&self) -> &str {
        "exec_shell_resize"
    }

    fn execute(&self, input: ToolInput) -> AppResult<ToolOutput> {
        let task_id = required_task_id(&input)?;
        let cwd = input.get("cwd").unwrap_or(".");
        let size = required_resize_tty_size(&input)?;
        let mut manager = shell_manager().lock().unwrap();
        if !manager.contains(task_id) {
            drop(manager);
            return Ok(ToolOutput {
                summary: resize_detached_shell_job(cwd, task_id, size)?,
            });
        }
        Ok(ToolOutput {
            summary: manager.resize(task_id, size)?,
        })
    }
}

impl Tool for ExecShellInteractTool {
    fn name(&self) -> &str {
        self.tool_name
    }

    fn execute(&self, input: ToolInput) -> AppResult<ToolOutput> {
        let task_id = required_task_id(&input)?;
        let cwd = input.get("cwd").unwrap_or(".");
        let data = input
            .get("input")
            .or_else(|| input.get("stdin"))
            .or_else(|| input.get("data"))
            .unwrap_or("");
        let close_stdin = truthy(input.get("close_stdin"));
        let timeout_ms = input_u64(&input, "timeout_ms", 1_000).min(MAX_TIMEOUT_MS);
        {
            let mut manager = shell_manager().lock().unwrap();
            if !manager.contains(task_id) {
                drop(manager);
                return Ok(ToolOutput {
                    summary: interact_detached_shell_job(
                        cwd,
                        task_id,
                        data,
                        close_stdin,
                        timeout_ms,
                    )?,
                });
            }
            manager.write_stdin(task_id, data, close_stdin)?;
        }
        ExecShellWaitTool {
            tool_name: self.tool_name,
        }
        .execute(
            ToolInput::new()
                .with_arg("task_id", task_id.to_string())
                .with_arg("cwd", cwd.to_string())
                .with_arg("timeout_ms", timeout_ms.to_string()),
        )
    }
}

impl Tool for ExecShellCancelTool {
    fn name(&self) -> &str {
        "exec_shell_cancel"
    }

    fn execute(&self, input: ToolInput) -> AppResult<ToolOutput> {
        if truthy(input.get("all")) {
            let cwd = input.get("cwd").unwrap_or(".");
            let report = cancel_all_shell_jobs(cwd)?;
            return Ok(ToolOutput {
                summary: if report.is_empty() {
                    "No running background shell jobs.".to_string()
                } else {
                    report.render_summary()
                },
            });
        }
        let task_id = required_task_id(&input)?;
        let cwd = input.get("cwd").unwrap_or(".");
        let mut manager = shell_manager().lock().unwrap();
        if !manager.contains(task_id) {
            drop(manager);
            return Ok(ToolOutput {
                summary: cancel_detached_shell_job(cwd, task_id)?,
            });
        }
        manager.cancel(task_id)?;
        Ok(ToolOutput {
            summary: format!("Canceled background shell job: {task_id}"),
        })
    }
}

#[derive(Default)]
struct ShellCancelAllReport {
    managed: Vec<String>,
    detached: Vec<String>,
    failures: Vec<String>,
}

impl ShellCancelAllReport {
    fn is_empty(&self) -> bool {
        self.managed.is_empty() && self.detached.is_empty() && self.failures.is_empty()
    }

    fn render_summary(&self) -> String {
        let total = self.managed.len() + self.detached.len();
        let mut lines = vec![format!(
            "Canceled {total} background shell job{}.",
            if total == 1 { "" } else { "s" }
        )];
        if !self.managed.is_empty() {
            lines.push(format!("managed: {}", self.managed.join(", ")));
        }
        if !self.detached.is_empty() {
            lines.push(format!("detached: {}", self.detached.join(", ")));
        }
        if !self.failures.is_empty() {
            lines.push(format!("failures: {}", self.failures.join("; ")));
        }
        lines.join("\n")
    }
}

fn cancel_all_shell_jobs(cwd: &str) -> AppResult<ShellCancelAllReport> {
    let managed = shell_manager().lock().unwrap().cancel_all_in_cwd(cwd)?;
    let mut report = ShellCancelAllReport {
        managed,
        ..ShellCancelAllReport::default()
    };
    let durable = list_durable_shell_jobs(cwd)?;
    for record in durable {
        if record.status != "running" || report.managed.iter().any(|id| id == &record.id) {
            continue;
        }
        match cancel_detached_shell_job(cwd, &record.id) {
            Ok(_) => report.detached.push(record.id),
            Err(error) => report.failures.push(format!("{}: {error}", record.id)),
        }
    }
    Ok(report)
}

struct BackgroundShellManager {
    jobs: BTreeMap<String, BackgroundShellJob>,
}

struct BackgroundShellJob {
    id: String,
    command: String,
    cwd: String,
    tty_options: ShellTtyOptions,
    owner_pid: u32,
    child_pid: u32,
    process_group: u32,
    child: Option<Child>,
    stdin: Option<ShellStdinControl>,
    stdout_cursor: usize,
    stderr_cursor: usize,
    status: ShellJobStatus,
    exit_code: Option<i32>,
    started_at: String,
    updated_at: String,
    record_dir: PathBuf,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct ShellTtyOptions {
    enabled: bool,
    size: Option<ShellTtySize>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ShellTtySize {
    rows: u16,
    cols: u16,
}

enum ShellStdinControl {
    Pipe(ChildStdin),
    Fifo {
        path: PathBuf,
        keeper: Option<Child>,
        closed: bool,
    },
}

struct PreparedBackgroundStdin {
    stdio: Stdio,
    mode: PreparedBackgroundStdinMode,
}

#[allow(dead_code)]
enum PreparedBackgroundStdinMode {
    Pipe,
    #[cfg(unix)]
    Fifo {
        path: PathBuf,
        hold: File,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ShellJobStatus {
    Running,
    Completed,
    Failed,
    Killed,
}

impl BackgroundShellManager {
    fn spawn(
        &mut self,
        command: &str,
        cwd: &str,
        stdin_data: Option<&str>,
        tty_options: ShellTtyOptions,
    ) -> AppResult<String> {
        self.spawn_with_env(command, cwd, stdin_data, tty_options, &BTreeMap::new())
    }

    fn spawn_with_env(
        &mut self,
        command: &str,
        cwd: &str,
        stdin_data: Option<&str>,
        tty_options: ShellTtyOptions,
        env: &BTreeMap<String, String>,
    ) -> AppResult<String> {
        let id = generated_job_id();
        let record_dir = shell_job_record_dir(cwd, &id);
        fs::create_dir_all(&record_dir)?;
        let stdout_file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(record_dir.join("stdout.log"))?;
        let stderr_file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(record_dir.join("stderr.log"))?;
        let PreparedBackgroundStdin {
            stdio: stdin_stdio,
            mode: stdin_mode,
        } = prepare_background_stdin(&record_dir)?;
        let mut process = shell_process_for_background_job(command, tty_options)?;
        apply_shell_env(&mut process, env);
        process
            .current_dir(cwd)
            .stdin(stdin_stdio)
            .stdout(Stdio::from(stdout_file))
            .stderr(Stdio::from(stderr_file));
        configure_process_group(&mut process);
        let mut child = process.spawn()?;
        let child_pid = child.id();
        let owner_pid = std::process::id();
        let process_group = child_pid;
        let mut stdin = stdin_mode.into_control(&mut child)?;
        if let Some(data) = stdin_data {
            if let Some(control) = stdin.as_mut() {
                write_background_stdin_control(control, data)?;
            }
        }

        let now = epoch_label();
        self.jobs.insert(
            id.clone(),
            BackgroundShellJob {
                id: id.clone(),
                command: command.to_string(),
                cwd: cwd.to_string(),
                tty_options,
                owner_pid,
                child_pid,
                process_group,
                child: Some(child),
                stdin,
                stdout_cursor: 0,
                stderr_cursor: 0,
                status: ShellJobStatus::Running,
                exit_code: None,
                started_at: now.clone(),
                updated_at: now,
                record_dir,
            },
        );
        if let Some(job) = self.jobs.get(&id) {
            persist_job_snapshot(job)?;
        }
        Ok(id)
    }

    fn refresh(&mut self, task_id: &str) -> AppResult<()> {
        let job = self
            .jobs
            .get_mut(task_id)
            .ok_or_else(|| app_error(format!("unknown background shell task: {task_id}")))?;
        if job.status != ShellJobStatus::Running {
            return Ok(());
        }
        let Some(child) = job.child.as_mut() else {
            return Ok(());
        };
        if let Some(status) = child.try_wait()? {
            job.exit_code = status.code();
            job.status = if status.success() {
                ShellJobStatus::Completed
            } else {
                ShellJobStatus::Failed
            };
            job.child = None;
            close_background_stdin_control(job.stdin.take());
            job.updated_at = epoch_label();
            persist_job_snapshot(job)?;
        }
        Ok(())
    }

    fn contains(&self, task_id: &str) -> bool {
        self.jobs.contains_key(task_id)
    }

    fn is_finished(&self, task_id: &str) -> AppResult<bool> {
        let job = self
            .jobs
            .get(task_id)
            .ok_or_else(|| app_error(format!("unknown background shell task: {task_id}")))?;
        Ok(job.status != ShellJobStatus::Running)
    }

    fn refresh_all(&mut self) -> AppResult<()> {
        let ids = self.jobs.keys().cloned().collect::<Vec<_>>();
        for id in ids {
            self.refresh(&id)?;
        }
        Ok(())
    }

    fn render_list(&self, cwd: &str) -> AppResult<String> {
        let durable = list_durable_shell_jobs(cwd)?;
        if self.jobs.is_empty() && durable.is_empty() {
            return Ok("No background shell jobs.".to_string());
        }

        let mut lines = vec![format!(
            "Background shell jobs: {} active, {} durable",
            self.jobs.len(),
            durable.len()
        )];
        for job in self.jobs.values() {
            let stdout_total = durable_log_bytes(&job.record_dir, "stdout.log", 0);
            let stderr_total = durable_log_bytes(&job.record_dir, "stderr.log", 0);
            lines.push(format!(
                "- {} [{}] exit={} stdout={} stderr={} tty={} pty_backend={} attachable=false resizable=false tty_size={} cwd={}",
                job.id,
                job.status.as_str(),
                job.exit_code
                    .map(|code| code.to_string())
                    .unwrap_or_else(|| "null".to_string()),
                stdout_total,
                stderr_total,
                job.tty_options.enabled,
                pty_backend_label(job.tty_options),
                tty_size_label(job.tty_options.size),
                job.cwd
            ));
            lines.push(format!("  command: {}", job.command));
        }
        for record in durable {
            if self.jobs.contains_key(&record.id) {
                continue;
            }
            lines.push(format!(
                "- {} [{} detached] exit={} stdout={} stderr={} tty={} pty_backend={} attachable={} resizable={} tty_size={} cwd={}",
                record.id,
                record.status,
                record
                    .exit_code
                    .map(|code| code.to_string())
                    .unwrap_or_else(|| "null".to_string()),
                record.stdout_total_bytes,
                record.stderr_total_bytes,
                record.tty,
                durable_pty_backend_label(&record),
                record.attachable,
                record.resizable,
                tty_size_label(record.tty_size),
                record.cwd
            ));
            lines.push(format!("  command: {}", record.command));
        }
        lines.push(
            "Controls: shell show <id>, shell poll <id>, shell wait <id>, shell stdin <id> <input>, shell cancel <id>."
                .to_string(),
        );
        Ok(lines.join("\n"))
    }

    fn render_delta(&mut self, task_id: &str) -> AppResult<String> {
        let job = self
            .jobs
            .get_mut(task_id)
            .ok_or_else(|| app_error(format!("unknown background shell task: {task_id}")))?;
        let stdout_delta = read_log_delta(&job.record_dir, "stdout.log", &mut job.stdout_cursor);
        let stderr_delta = read_log_delta(&job.record_dir, "stderr.log", &mut job.stderr_cursor);
        let stdout_total = durable_log_bytes(&job.record_dir, "stdout.log", 0);
        let stderr_total = durable_log_bytes(&job.record_dir, "stderr.log", 0);
        let mut out = format!(
            "task_id: {}\nstatus: {}\nexit_code: {}\ncommand: {}\ncwd: {}\nowner_pid: {}\nowner_alive: {}\npid: {}\nprocess_group: {}\ntty: {}\npty_backend: {}\nattachable: false\nresizable: false\nsupervisor_pid: null\nsupervisor_alive: unknown\nsupervisor_socket: null\nsupervisor_epoch: null\nterminal_event_log: null\nterminal_event_seq: null\ntty_rows: {}\ntty_cols: {}\nstdout_total_bytes: {stdout_total}\nstderr_total_bytes: {stderr_total}\n",
            job.id,
            job.status.as_str(),
            job.exit_code
                .map(|code| code.to_string())
                .unwrap_or_else(|| "null".to_string()),
            job.command,
            job.cwd,
            job.owner_pid,
            process_is_alive(job.owner_pid),
            job.child_pid,
            job.process_group,
            job.tty_options.enabled,
            pty_backend_label(job.tty_options),
            tty_rows_label(job.tty_options.size),
            tty_cols_label(job.tty_options.size)
        );
        if !stdout_delta.trim().is_empty() {
            out.push_str("stdout_delta:\n");
            out.push_str(stdout_delta.trim_end());
            out.push('\n');
        }
        if !stderr_delta.trim().is_empty() {
            out.push_str("stderr_delta:\n");
            out.push_str(stderr_delta.trim_end());
            out.push('\n');
        }
        job.updated_at = epoch_label();
        persist_job_snapshot(job)?;
        Ok(out.trim_end().to_string())
    }

    fn render_snapshot(&self, task_id: &str) -> AppResult<String> {
        let job = self
            .jobs
            .get(task_id)
            .ok_or_else(|| app_error(format!("unknown background shell task: {task_id}")))?;
        let stdout = read_durable_log(&job.record_dir, "stdout.log");
        let stderr = read_durable_log(&job.record_dir, "stderr.log");
        let stdout_total = stdout.len();
        let stderr_total = stderr.len();
        let mut out = format!(
            "task_id: {}\nstatus: {}\nexit_code: {}\ncommand: {}\ncwd: {}\nowner_pid: {}\nowner_alive: {}\npid: {}\nprocess_group: {}\ntty: {}\npty_backend: {}\nattachable: false\nresizable: false\nsupervisor_pid: null\nsupervisor_alive: unknown\nsupervisor_socket: null\nsupervisor_epoch: null\nterminal_event_log: null\nterminal_event_seq: null\ntty_rows: {}\ntty_cols: {}\nstdout_total_bytes: {stdout_total}\nstderr_total_bytes: {stderr_total}\n",
            job.id,
            job.status.as_str(),
            job.exit_code
                .map(|code| code.to_string())
                .unwrap_or_else(|| "null".to_string()),
            job.command,
            job.cwd,
            job.owner_pid,
            process_is_alive(job.owner_pid),
            job.child_pid,
            job.process_group,
            job.tty_options.enabled,
            pty_backend_label(job.tty_options),
            tty_rows_label(job.tty_options.size),
            tty_cols_label(job.tty_options.size)
        );
        if !stdout.trim().is_empty() {
            out.push_str("stdout:\n");
            out.push_str(&clip_shell_snapshot(&stdout));
            out.push('\n');
        }
        if !stderr.trim().is_empty() {
            out.push_str("stderr:\n");
            out.push_str(&clip_shell_snapshot(&stderr));
            out.push('\n');
        }
        Ok(out.trim_end().to_string())
    }

    fn write_stdin(&mut self, task_id: &str, data: &str, close_stdin: bool) -> AppResult<()> {
        self.refresh(task_id)?;
        let job = self
            .jobs
            .get_mut(task_id)
            .ok_or_else(|| app_error(format!("unknown background shell task: {task_id}")))?;
        if job.status != ShellJobStatus::Running {
            return Err(app_error(format!(
                "background shell task {task_id} is {}",
                job.status.as_str()
            )));
        }
        let Some(control) = job.stdin.as_mut() else {
            return Err(app_error(format!(
                "stdin is not available for background shell task {task_id}"
            )));
        };
        if !data.is_empty() {
            write_background_stdin_control(control, data)?;
        }
        if close_stdin {
            close_background_stdin_control(job.stdin.take());
        }
        job.updated_at = epoch_label();
        persist_job_snapshot(job)?;
        Ok(())
    }

    fn resize(&mut self, task_id: &str, size: ShellTtySize) -> AppResult<String> {
        self.refresh(task_id)?;
        let live_control = {
            let job = self
                .jobs
                .get_mut(task_id)
                .ok_or_else(|| app_error(format!("unknown background shell task: {task_id}")))?;
            if !job.tty_options.enabled {
                return Err(app_error(format!(
                    "background shell task {task_id} was not started with tty=true"
                )));
            }
            let mut live_control = "metadata_only";
            if job.status == ShellJobStatus::Running {
                live_control = if let Some(control) = job.stdin.as_mut() {
                    write_background_stdin_control(control, &resize_stty_command(size))?;
                    "stdin_stty"
                } else {
                    "metadata_only_no_stdin"
                };
            }
            job.tty_options.size = Some(size);
            job.updated_at = epoch_label();
            persist_job_snapshot(job)?;
            live_control
        };
        let snapshot = self.render_snapshot(task_id)?;
        Ok(format!(
            "meta.live_resize={live_control}\nmeta.tty_size={}x{}\n{}",
            size.rows, size.cols, snapshot
        ))
    }

    fn cancel(&mut self, task_id: &str) -> AppResult<()> {
        let job = self
            .jobs
            .get_mut(task_id)
            .ok_or_else(|| app_error(format!("unknown background shell task: {task_id}")))?;
        if let Some(child) = job.child.as_mut() {
            kill_child_process_group(child);
            let _ = child.wait();
        }
        job.child = None;
        close_background_stdin_control(job.stdin.take());
        job.status = ShellJobStatus::Killed;
        job.updated_at = epoch_label();
        persist_job_snapshot(job)?;
        Ok(())
    }

    fn cancel_all_in_cwd(&mut self, cwd: &str) -> AppResult<Vec<String>> {
        let ids = self
            .jobs
            .iter()
            .filter_map(|(id, job)| {
                if job.status == ShellJobStatus::Running && job.cwd == cwd {
                    Some(id.clone())
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();
        for id in &ids {
            self.cancel(id)?;
        }
        Ok(ids)
    }
}

impl Default for BackgroundShellManager {
    fn default() -> Self {
        Self {
            jobs: BTreeMap::new(),
        }
    }
}

impl ShellJobStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Killed => "killed",
        }
    }
}

fn shell_manager() -> &'static Mutex<BackgroundShellManager> {
    SHELL_JOBS.get_or_init(|| Mutex::new(BackgroundShellManager::default()))
}

impl ShellTtyOptions {
    fn requested(self) -> bool {
        self.enabled || self.size.is_some()
    }
}

fn parse_tty_options(input: &ToolInput) -> AppResult<ShellTtyOptions> {
    let enabled = truthy(input.get("tty"));
    let rows = optional_input_u16(input, "tty_rows", 1, 2000)?;
    let cols = optional_input_u16(input, "tty_cols", 1, 2000)?;
    let size = match (rows, cols) {
        (Some(rows), Some(cols)) => Some(ShellTtySize { rows, cols }),
        (None, None) => None,
        _ => {
            return Err(app_error(
                "exec_shell tty_rows and tty_cols must be provided together",
            ));
        }
    };
    if size.is_some() && !enabled {
        return Err(app_error("exec_shell tty_rows/tty_cols require tty=true"));
    }
    Ok(ShellTtyOptions { enabled, size })
}

fn required_resize_tty_size(input: &ToolInput) -> AppResult<ShellTtySize> {
    let rows = required_input_u16(input, "tty_rows", "rows", 1, 2000)?;
    let cols = required_input_u16(input, "tty_cols", "cols", 1, 2000)?;
    Ok(ShellTtySize { rows, cols })
}

fn required_input_u16(
    input: &ToolInput,
    key: &str,
    alias: &str,
    min: u16,
    max: u16,
) -> AppResult<u16> {
    let raw = input
        .get(key)
        .or_else(|| input.get(alias))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| app_error(format!("exec_shell_resize requires {key}")))?;
    let value = raw
        .parse::<u16>()
        .map_err(|_| app_error(format!("exec_shell_resize {key} must be an integer")))?;
    if value < min || value > max {
        return Err(app_error(format!(
            "exec_shell_resize {key} must be between {min} and {max}"
        )));
    }
    Ok(value)
}

fn optional_input_u16(input: &ToolInput, key: &str, min: u16, max: u16) -> AppResult<Option<u16>> {
    let Some(raw) = input
        .get(key)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Ok(None);
    };
    let value = raw
        .parse::<u16>()
        .map_err(|_| app_error(format!("exec_shell {key} must be an integer")))?;
    if value < min || value > max {
        return Err(app_error(format!(
            "exec_shell {key} must be between {min} and {max}"
        )));
    }
    Ok(Some(value))
}

fn shell_process_for_background_job(
    command: &str,
    tty_options: ShellTtyOptions,
) -> AppResult<Command> {
    if tty_options.enabled {
        if !script_pty_backend_available() {
            return Err(app_error(
                "exec_shell/task_shell_start tty=true requires the `script` PTY utility",
            ));
        }
        let mut process = Command::new("script");
        let script_command = script_pty_command(command, tty_options.size);
        process.args(["-q", "-f", "-e", "-c", &script_command, "/dev/null"]);
        process.env("TERM", "xterm-256color");
        if let Some(size) = tty_options.size {
            process.env("LINES", size.rows.to_string());
            process.env("COLUMNS", size.cols.to_string());
        }
        Ok(process)
    } else {
        let mut process = Command::new("sh");
        process.args(["-lc", command]);
        Ok(process)
    }
}

fn script_pty_command(command: &str, size: Option<ShellTtySize>) -> String {
    match size {
        Some(size) => format!("stty rows {} cols {}; {command}", size.rows, size.cols),
        None => command.to_string(),
    }
}

fn resize_stty_command(size: ShellTtySize) -> String {
    format!("stty rows {} cols {}\n", size.rows, size.cols)
}

fn pty_backend_label(tty_options: ShellTtyOptions) -> &'static str {
    if tty_options.enabled {
        PTY_BACKEND_SCRIPT
    } else {
        PTY_BACKEND_NONE
    }
}

fn durable_pty_backend_label(record: &DurableShellJobRecord) -> &str {
    record.pty_backend.as_deref().unwrap_or(PTY_BACKEND_NONE)
}

fn tty_size_label(size: Option<ShellTtySize>) -> String {
    size.map(|size| format!("{}x{}", size.rows, size.cols))
        .unwrap_or_else(|| "null".to_string())
}

fn tty_rows_label(size: Option<ShellTtySize>) -> String {
    size.map(|size| size.rows.to_string())
        .unwrap_or_else(|| "null".to_string())
}

fn tty_cols_label(size: Option<ShellTtySize>) -> String {
    size.map(|size| size.cols.to_string())
        .unwrap_or_else(|| "null".to_string())
}

fn shell_optional_pid_label(pid: Option<u32>) -> String {
    pid.map(|pid| pid.to_string())
        .unwrap_or_else(|| "null".to_string())
}

fn owner_alive_label(pid: Option<u32>) -> String {
    pid.map(process_is_alive)
        .map(|alive| alive.to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

fn shell_optional_u64_label(value: Option<u64>) -> String {
    value
        .map(|value| value.to_string())
        .unwrap_or_else(|| "null".to_string())
}

fn shell_optional_string_label(value: Option<&str>) -> String {
    value
        .map(str::to_string)
        .unwrap_or_else(|| "null".to_string())
}

fn script_pty_backend_available() -> bool {
    Command::new("script")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn prepare_background_stdin(record_dir: &Path) -> AppResult<PreparedBackgroundStdin> {
    #[cfg(unix)]
    {
        let path = record_dir.join("stdin.fifo");
        create_fifo(&path)?;
        let hold = OpenOptions::new().read(true).write(true).open(&path)?;
        let child_read = OpenOptions::new().read(true).open(&path)?;
        Ok(PreparedBackgroundStdin {
            stdio: Stdio::from(child_read),
            mode: PreparedBackgroundStdinMode::Fifo { path, hold },
        })
    }
    #[cfg(not(unix))]
    {
        let _ = record_dir;
        Ok(PreparedBackgroundStdin {
            stdio: Stdio::piped(),
            mode: PreparedBackgroundStdinMode::Pipe,
        })
    }
}

impl PreparedBackgroundStdinMode {
    fn into_control(self, child: &mut Child) -> AppResult<Option<ShellStdinControl>> {
        match self {
            PreparedBackgroundStdinMode::Pipe => {
                Ok(child.stdin.take().map(ShellStdinControl::Pipe))
            }
            #[cfg(unix)]
            PreparedBackgroundStdinMode::Fifo { path, hold } => {
                let writer = OpenOptions::new().write(true).open(&path)?;
                let mut keeper = Command::new("sleep");
                keeper
                    .arg("2147483647")
                    .stdin(Stdio::null())
                    .stdout(Stdio::from(writer))
                    .stderr(Stdio::null());
                configure_process_group_id(&mut keeper, child.id());
                let keeper = keeper.spawn()?;
                drop(hold);
                Ok(Some(ShellStdinControl::Fifo {
                    path,
                    keeper: Some(keeper),
                    closed: false,
                }))
            }
        }
    }
}

fn write_background_stdin_control(control: &mut ShellStdinControl, data: &str) -> AppResult<()> {
    match control {
        ShellStdinControl::Pipe(stdin) => {
            stdin.write_all(data.as_bytes())?;
            stdin.flush()?;
        }
        ShellStdinControl::Fifo { path, closed, .. } => {
            if *closed {
                return Err(app_error("stdin is closed for background shell task"));
            }
            write_fifo_stdin(path, data)?;
        }
    }
    Ok(())
}

fn close_background_stdin_control(control: Option<ShellStdinControl>) {
    let Some(control) = control else {
        return;
    };
    match control {
        ShellStdinControl::Pipe(_) => {}
        ShellStdinControl::Fifo {
            keeper: Some(mut keeper),
            ..
        } => {
            let _ = keeper.kill();
            let _ = keeper.wait();
        }
        ShellStdinControl::Fifo { keeper: None, .. } => {}
    }
}

fn shell_stdin_manifest_fields(control: &ShellStdinControl) -> (JsonValue, JsonValue, JsonValue) {
    match control {
        ShellStdinControl::Pipe(_) => (JsonValue::Null, JsonValue::Null, JsonValue::Bool(false)),
        ShellStdinControl::Fifo {
            path,
            keeper,
            closed,
        } => (
            JsonValue::String(path.display().to_string()),
            keeper
                .as_ref()
                .map(|keeper| JsonValue::Number(keeper.id().to_string()))
                .unwrap_or(JsonValue::Null),
            JsonValue::Bool(*closed),
        ),
    }
}

fn required_task_id(input: &ToolInput) -> AppResult<&str> {
    input
        .get("task_id")
        .or_else(|| input.get("id"))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| app_error("background shell task_id is required"))
}

fn foreground_detach_after_ms(input: &ToolInput) -> AppResult<Option<u64>> {
    let Some((key, value)) = input
        .get("detach_after_ms")
        .map(|value| ("detach_after_ms", value))
        .or_else(|| input.get("timeout_ms").map(|value| ("timeout_ms", value)))
    else {
        return Ok(None);
    };
    let parsed = value
        .parse::<u64>()
        .map_err(|_| app_error(format!("exec_shell {key} must be an integer")))?;
    if parsed == 0 {
        return Err(app_error(format!(
            "exec_shell {key} must be greater than zero"
        )));
    }
    Ok(Some(parsed.min(MAX_TIMEOUT_MS)))
}

fn input_u64(input: &ToolInput, key: &str, default: u64) -> u64 {
    input
        .get(key)
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(default)
}

fn clip_shell_snapshot(value: &str) -> String {
    const MAX_CHARS: usize = 20_000;
    let trimmed = value.trim_end();
    if trimmed.chars().count() <= MAX_CHARS {
        return trimmed.to_string();
    }
    let mut clipped = trimmed.chars().rev().take(MAX_CHARS).collect::<Vec<_>>();
    clipped.reverse();
    format!(
        "[truncated to last {MAX_CHARS} chars]\n{}",
        clipped.into_iter().collect::<String>()
    )
}

fn truthy(value: Option<&str>) -> bool {
    matches!(
        value.map(str::trim),
        Some("1") | Some("true") | Some("TRUE") | Some("yes") | Some("on")
    )
}

fn generated_job_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    let counter = JOB_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("shell-{}-{nanos}-{counter}", std::process::id())
}

fn epoch_label() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    format!("epoch+{secs}")
}

#[derive(Debug, Clone)]
struct DurableShellJobRecord {
    id: String,
    command: String,
    cwd: String,
    tty: bool,
    pty_backend: Option<String>,
    tty_size: Option<ShellTtySize>,
    status: String,
    exit_code: Option<i32>,
    pid: u32,
    owner_pid: Option<u32>,
    process_group: Option<u32>,
    supervisor_pid: Option<u32>,
    supervisor_socket: Option<String>,
    supervisor_epoch: Option<String>,
    terminal_event_log: Option<String>,
    terminal_event_seq: Option<u64>,
    control_token_hash: Option<String>,
    attachable: bool,
    resizable: bool,
    stdin_path: Option<String>,
    stdin_keeper_pid: Option<u32>,
    stdin_closed: bool,
    started_at: String,
    updated_at: String,
    stdout_total_bytes: usize,
    stderr_total_bytes: usize,
}

#[derive(Debug, Default)]
struct ShellSupervisorManifest {
    kind: Option<String>,
    supervisor_pid: Option<u32>,
    supervisor_socket: Option<String>,
    supervisor_epoch: Option<String>,
    protocol: Option<String>,
    methods: Vec<String>,
    unsupported_methods: Vec<String>,
    started_at: Option<String>,
    updated_at: Option<String>,
    active_jobs: Option<u64>,
}

fn shell_job_record_dir(cwd: &str, task_id: &str) -> PathBuf {
    Path::new(cwd).join(".dscode/shell-jobs").join(task_id)
}

fn shell_job_manifest_path(cwd: &str, task_id: &str) -> PathBuf {
    shell_job_record_dir(cwd, task_id).join("manifest.json")
}

fn shell_supervisor_state_dir(cwd: &str) -> PathBuf {
    Path::new(cwd).join(".dscode/shell-supervisor")
}

fn shell_supervisor_manifest_path(cwd: &str) -> PathBuf {
    shell_supervisor_state_dir(cwd).join("manifest.json")
}

fn shell_supervisor_default_socket_path(cwd: &str) -> PathBuf {
    shell_supervisor_state_dir(cwd).join("supervisor.sock")
}

fn durable_shell_job_exists(cwd: &str, task_id: &str) -> bool {
    shell_job_manifest_path(cwd, task_id).is_file()
}

fn detached_or_unknown_shell_task_error(cwd: &str, task_id: &str, action: &str) -> Box<dyn Error> {
    if durable_shell_job_exists(cwd, task_id) {
        app_error(format!(
            "background shell task {task_id} is detached; durable metadata and logs are available with exec_shell_show cwd={cwd}, but {action} control requires the original attached DeepSeekCode process"
        ))
    } else {
        app_error(format!("unknown background shell task: {task_id}"))
    }
}

fn interact_detached_shell_job(
    cwd: &str,
    task_id: &str,
    data: &str,
    close_stdin: bool,
    timeout_ms: u64,
) -> AppResult<String> {
    let manifest = shell_job_manifest_path(cwd, task_id);
    if !manifest.is_file() {
        return Err(app_error(format!(
            "unknown background shell task: {task_id}"
        )));
    }
    let mut record = read_durable_shell_job_manifest(&manifest)?;
    refresh_durable_running_status(cwd, &mut record)?;
    if record.status != "running" {
        return Err(app_error(format!(
            "background shell task {task_id} is detached but is {}",
            record.status
        )));
    }
    let Some(stdin_path) = record.stdin_path.clone() else {
        return Err(detached_or_unknown_shell_task_error(cwd, task_id, "stdin"));
    };
    if record.stdin_closed {
        return Err(app_error(format!(
            "stdin is closed for detached background shell task {task_id}"
        )));
    }
    if !data.is_empty() {
        write_fifo_stdin(Path::new(&stdin_path), data)?;
    }
    if close_stdin {
        if let Some(keeper_pid) = record.stdin_keeper_pid {
            kill_process(keeper_pid)?;
        }
        record.stdin_keeper_pid = None;
        record.stdin_closed = true;
        record.updated_at = epoch_label();
        write_durable_shell_job_manifest(cwd, &record)?;
    }

    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    loop {
        let mut current = read_durable_shell_job_manifest(&manifest)?;
        refresh_durable_running_status(cwd, &mut current)?;
        if current.status != "running" || Instant::now() >= deadline {
            let snapshot = render_durable_snapshot(cwd, task_id)?;
            return Ok(format!(
                "meta.detached_stdin=true\nmeta.stdin_closed={}\n{}",
                close_stdin, snapshot
            ));
        }
        thread::sleep(Duration::from_millis(25));
    }
}

fn resize_detached_shell_job(cwd: &str, task_id: &str, size: ShellTtySize) -> AppResult<String> {
    let manifest = shell_job_manifest_path(cwd, task_id);
    if !manifest.is_file() {
        return Err(app_error(format!(
            "unknown background shell task: {task_id}"
        )));
    }
    let mut record = read_durable_shell_job_manifest(&manifest)?;
    refresh_durable_running_status(cwd, &mut record)?;
    if !record.tty {
        return Err(app_error(format!(
            "detached background shell task {task_id} was not started with tty=true"
        )));
    }
    let mut live_control = "metadata_only";
    if record.status == "running" {
        live_control = if let Some(stdin_path) = record.stdin_path.as_deref() {
            if record.stdin_closed {
                "metadata_only_stdin_closed"
            } else {
                write_fifo_stdin(Path::new(stdin_path), &resize_stty_command(size))?;
                "detached_fifo_stty"
            }
        } else {
            "metadata_only_no_stdin"
        };
    }
    record.tty_size = Some(size);
    record.updated_at = epoch_label();
    write_durable_shell_job_manifest(cwd, &record)?;
    let snapshot = render_durable_snapshot(cwd, task_id)?;
    Ok(format!(
        "meta.detached_resize=true\nmeta.live_resize={live_control}\nmeta.tty_size={}x{}\n{}",
        size.rows, size.cols, snapshot
    ))
}

fn cancel_detached_shell_job(cwd: &str, task_id: &str) -> AppResult<String> {
    let manifest = shell_job_manifest_path(cwd, task_id);
    if !manifest.is_file() {
        return Err(app_error(format!(
            "unknown background shell task: {task_id}"
        )));
    }
    let mut record = read_durable_shell_job_manifest(&manifest)?;
    refresh_durable_running_status(cwd, &mut record)?;
    if record.status != "running" {
        return Ok(format!(
            "Detached background shell job is not running: {task_id}\nmanaged: false\nstatus: {}",
            record.status
        ));
    }
    if record.pid == 0 {
        return Err(app_error(format!(
            "detached background shell task {task_id} has no recorded pid for cancellation"
        )));
    }
    let process_group = record.process_group.unwrap_or(record.pid);
    kill_detached_process_group(process_group, record.pid)?;
    if let Some(keeper_pid) = record.stdin_keeper_pid {
        let _ = kill_process(keeper_pid);
    }
    record.status = "killed".to_string();
    record.exit_code = None;
    record.stdin_closed = true;
    record.stdin_keeper_pid = None;
    record.updated_at = epoch_label();
    write_durable_shell_job_manifest(cwd, &record)?;
    Ok(format!(
        "Canceled detached background shell job: {task_id}\nmanaged: false\npid: {}\nstatus: killed",
        record.pid
    ))
}

fn persist_job_snapshot(job: &BackgroundShellJob) -> AppResult<()> {
    fs::create_dir_all(&job.record_dir)?;
    let stdout_total = durable_log_bytes(&job.record_dir, "stdout.log", 0);
    let stderr_total = durable_log_bytes(&job.record_dir, "stderr.log", 0);
    let exit_code = job
        .exit_code
        .map(|code| JsonValue::Number(code.to_string()))
        .unwrap_or(JsonValue::Null);
    let (stdin_path, stdin_keeper_pid, stdin_closed) = job
        .stdin
        .as_ref()
        .map(shell_stdin_manifest_fields)
        .unwrap_or((JsonValue::Null, JsonValue::Null, JsonValue::Bool(true)));
    let manifest = JsonValue::Object(BTreeMap::from([
        (
            "kind".to_string(),
            JsonValue::String("deepseek.exec_shell.job.v1".to_string()),
        ),
        ("id".to_string(), JsonValue::String(job.id.clone())),
        (
            "command".to_string(),
            JsonValue::String(job.command.clone()),
        ),
        ("cwd".to_string(), JsonValue::String(job.cwd.clone())),
        ("tty".to_string(), JsonValue::Bool(job.tty_options.enabled)),
        (
            "pty_backend".to_string(),
            if job.tty_options.enabled {
                JsonValue::String(PTY_BACKEND_SCRIPT.to_string())
            } else {
                JsonValue::Null
            },
        ),
        ("attachable".to_string(), JsonValue::Bool(false)),
        ("resizable".to_string(), JsonValue::Bool(false)),
        ("supervisor_pid".to_string(), JsonValue::Null),
        ("supervisor_socket".to_string(), JsonValue::Null),
        ("supervisor_epoch".to_string(), JsonValue::Null),
        ("terminal_event_log".to_string(), JsonValue::Null),
        ("terminal_event_seq".to_string(), JsonValue::Null),
        ("control_token_hash".to_string(), JsonValue::Null),
        (
            "tty_rows".to_string(),
            job.tty_options
                .size
                .map(|size| JsonValue::Number(size.rows.to_string()))
                .unwrap_or(JsonValue::Null),
        ),
        (
            "tty_cols".to_string(),
            job.tty_options
                .size
                .map(|size| JsonValue::Number(size.cols.to_string()))
                .unwrap_or(JsonValue::Null),
        ),
        (
            "status".to_string(),
            JsonValue::String(job.status.as_str().to_string()),
        ),
        ("exit_code".to_string(), exit_code),
        (
            "pid".to_string(),
            JsonValue::Number(job.child_pid.to_string()),
        ),
        (
            "owner_pid".to_string(),
            JsonValue::Number(job.owner_pid.to_string()),
        ),
        (
            "process_group".to_string(),
            JsonValue::Number(job.process_group.to_string()),
        ),
        ("stdin_path".to_string(), stdin_path),
        ("stdin_keeper_pid".to_string(), stdin_keeper_pid),
        ("stdin_closed".to_string(), stdin_closed),
        (
            "started_at".to_string(),
            JsonValue::String(job.started_at.clone()),
        ),
        (
            "updated_at".to_string(),
            JsonValue::String(job.updated_at.clone()),
        ),
        (
            "stdout_total_bytes".to_string(),
            JsonValue::Number(stdout_total.to_string()),
        ),
        (
            "stderr_total_bytes".to_string(),
            JsonValue::Number(stderr_total.to_string()),
        ),
    ]));
    fs::write(
        job.record_dir.join("manifest.json"),
        json_value_to_string(&manifest),
    )?;
    Ok(())
}

fn write_durable_shell_job_manifest(cwd: &str, record: &DurableShellJobRecord) -> AppResult<()> {
    let record_dir = shell_job_record_dir(cwd, &record.id);
    fs::create_dir_all(&record_dir)?;
    let exit_code = record
        .exit_code
        .map(|code| JsonValue::Number(code.to_string()))
        .unwrap_or(JsonValue::Null);
    let stdin_path = record
        .stdin_path
        .as_ref()
        .map(|value| JsonValue::String(value.clone()))
        .unwrap_or(JsonValue::Null);
    let stdin_keeper_pid = record
        .stdin_keeper_pid
        .map(|pid| JsonValue::Number(pid.to_string()))
        .unwrap_or(JsonValue::Null);
    let stdout_total = durable_log_bytes(&record_dir, "stdout.log", record.stdout_total_bytes);
    let stderr_total = durable_log_bytes(&record_dir, "stderr.log", record.stderr_total_bytes);
    let manifest = JsonValue::Object(BTreeMap::from([
        (
            "kind".to_string(),
            JsonValue::String("deepseek.exec_shell.job.v1".to_string()),
        ),
        ("id".to_string(), JsonValue::String(record.id.clone())),
        (
            "command".to_string(),
            JsonValue::String(record.command.clone()),
        ),
        ("cwd".to_string(), JsonValue::String(record.cwd.clone())),
        ("tty".to_string(), JsonValue::Bool(record.tty)),
        (
            "pty_backend".to_string(),
            record
                .pty_backend
                .as_ref()
                .map(|backend| JsonValue::String(backend.clone()))
                .unwrap_or(JsonValue::Null),
        ),
        ("attachable".to_string(), JsonValue::Bool(record.attachable)),
        ("resizable".to_string(), JsonValue::Bool(record.resizable)),
        (
            "supervisor_pid".to_string(),
            record
                .supervisor_pid
                .map(|pid| JsonValue::Number(pid.to_string()))
                .unwrap_or(JsonValue::Null),
        ),
        (
            "supervisor_socket".to_string(),
            record
                .supervisor_socket
                .as_ref()
                .map(|value| JsonValue::String(value.clone()))
                .unwrap_or(JsonValue::Null),
        ),
        (
            "supervisor_epoch".to_string(),
            record
                .supervisor_epoch
                .as_ref()
                .map(|value| JsonValue::String(value.clone()))
                .unwrap_or(JsonValue::Null),
        ),
        (
            "terminal_event_log".to_string(),
            record
                .terminal_event_log
                .as_ref()
                .map(|value| JsonValue::String(value.clone()))
                .unwrap_or(JsonValue::Null),
        ),
        (
            "terminal_event_seq".to_string(),
            record
                .terminal_event_seq
                .map(|seq| JsonValue::Number(seq.to_string()))
                .unwrap_or(JsonValue::Null),
        ),
        (
            "control_token_hash".to_string(),
            record
                .control_token_hash
                .as_ref()
                .map(|value| JsonValue::String(value.clone()))
                .unwrap_or(JsonValue::Null),
        ),
        (
            "tty_rows".to_string(),
            record
                .tty_size
                .map(|size| JsonValue::Number(size.rows.to_string()))
                .unwrap_or(JsonValue::Null),
        ),
        (
            "tty_cols".to_string(),
            record
                .tty_size
                .map(|size| JsonValue::Number(size.cols.to_string()))
                .unwrap_or(JsonValue::Null),
        ),
        (
            "status".to_string(),
            JsonValue::String(record.status.clone()),
        ),
        ("exit_code".to_string(), exit_code),
        ("pid".to_string(), JsonValue::Number(record.pid.to_string())),
        (
            "owner_pid".to_string(),
            record
                .owner_pid
                .map(|pid| JsonValue::Number(pid.to_string()))
                .unwrap_or(JsonValue::Null),
        ),
        (
            "process_group".to_string(),
            record
                .process_group
                .map(|pid| JsonValue::Number(pid.to_string()))
                .unwrap_or(JsonValue::Null),
        ),
        ("stdin_path".to_string(), stdin_path),
        ("stdin_keeper_pid".to_string(), stdin_keeper_pid),
        (
            "stdin_closed".to_string(),
            JsonValue::Bool(record.stdin_closed),
        ),
        (
            "started_at".to_string(),
            JsonValue::String(record.started_at.clone()),
        ),
        (
            "updated_at".to_string(),
            JsonValue::String(record.updated_at.clone()),
        ),
        (
            "stdout_total_bytes".to_string(),
            JsonValue::Number(stdout_total.to_string()),
        ),
        (
            "stderr_total_bytes".to_string(),
            JsonValue::Number(stderr_total.to_string()),
        ),
    ]));
    fs::write(
        record_dir.join("manifest.json"),
        json_value_to_string(&manifest),
    )?;
    Ok(())
}

fn list_durable_shell_jobs(cwd: &str) -> AppResult<Vec<DurableShellJobRecord>> {
    let dir = Path::new(cwd).join(".dscode/shell-jobs");
    if !dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut records = Vec::new();
    for entry in fs::read_dir(dir)? {
        let path = entry?.path().join("manifest.json");
        if !path.is_file() {
            continue;
        }
        if let Ok(record) = read_durable_shell_job_manifest(&path) {
            let mut record = record;
            refresh_durable_running_status(cwd, &mut record)?;
            records.push(record);
        }
    }
    records.sort_by(|a, b| {
        b.updated_at
            .cmp(&a.updated_at)
            .then_with(|| b.id.cmp(&a.id))
    });
    Ok(records)
}

fn render_shell_supervisor_status(cwd: &str) -> AppResult<String> {
    let state_dir = shell_supervisor_state_dir(cwd);
    let manifest_path = shell_supervisor_manifest_path(cwd);
    let default_socket = shell_supervisor_default_socket_path(cwd);
    let manifest = if manifest_path.is_file() {
        Some(read_shell_supervisor_manifest(&manifest_path)?)
    } else {
        None
    };
    let socket_path = manifest
        .as_ref()
        .and_then(|manifest| manifest.supervisor_socket.as_deref())
        .map(PathBuf::from)
        .unwrap_or(default_socket);
    let socket_kind = shell_supervisor_socket_kind(&socket_path);
    let protocol_health = shell_supervisor_protocol_health(&socket_path, socket_kind == "socket");
    let (protocol_status, protocol_status_active_jobs) =
        shell_supervisor_protocol_status(&socket_path, socket_kind == "socket", &protocol_health);
    let (protocol_show, protocol_job_inventory) =
        shell_supervisor_protocol_show(&socket_path, socket_kind == "socket", &protocol_health);
    let supervisor_alive = manifest
        .as_ref()
        .and_then(|manifest| manifest.supervisor_pid)
        .map(process_is_alive);
    let status = shell_supervisor_status_label(
        manifest.is_some(),
        socket_kind == "socket",
        supervisor_alive,
        &protocol_health,
    );
    Ok(format!(
        "kind: deepseek.exec_shell.supervisor_status.v1\nstatus: {status}\nplatform: {}\ncwd: {}\nstate_dir: {}\nmanifest: {}\nmanifest_exists: {}\nmanifest_kind: {}\nsocket: {}\nsocket_kind: {socket_kind}\nprotocol_health: {protocol_health}\nprotocol_status: {protocol_status}\nprotocol_status_active_jobs: {}\nprotocol_show: {protocol_show}\nsupervisor_pid: {}\nsupervisor_alive: {}\nsupervisor_epoch: {}\nprotocol: {}\nmethods: {}\nunsupported_methods: {}\nactive_jobs: {}\nstarted_at: {}\nupdated_at: {}\nprotocol_job_inventory:\n{}\nnote: this is the shell supervisor protocol/status skeleton; native PTY ownership, live attach, and TIOCSWINSZ resize are not implemented until a real supervisor process writes this state.\n",
        shell_supervisor_platform_label(),
        cwd,
        state_dir.display(),
        manifest_path.display(),
        manifest.is_some(),
        shell_optional_string_label(manifest.as_ref().and_then(|manifest| manifest.kind.as_deref())),
        socket_path.display(),
        protocol_status_active_jobs.unwrap_or_else(|| "not_checked".to_string()),
        shell_optional_pid_label(manifest.as_ref().and_then(|manifest| manifest.supervisor_pid)),
        supervisor_alive
            .map(|alive| alive.to_string())
            .unwrap_or_else(|| "unknown".to_string()),
        shell_optional_string_label(
            manifest
                .as_ref()
                .and_then(|manifest| manifest.supervisor_epoch.as_deref())
        ),
        shell_optional_string_label(
            manifest
                .as_ref()
                .and_then(|manifest| manifest.protocol.as_deref())
        ),
        shell_supervisor_method_label(
            manifest.as_ref().map(|manifest| manifest.methods.as_slice()),
            SHELL_SUPERVISOR_SUPPORTED_METHODS,
        ),
        shell_supervisor_method_label(
            manifest
                .as_ref()
                .map(|manifest| manifest.unsupported_methods.as_slice()),
            SHELL_SUPERVISOR_UNSUPPORTED_PTY_METHODS,
        ),
        shell_optional_u64_label(manifest.as_ref().and_then(|manifest| manifest.active_jobs)),
        shell_optional_string_label(
            manifest
                .as_ref()
                .and_then(|manifest| manifest.started_at.as_deref())
        ),
        shell_optional_string_label(
            manifest
                .as_ref()
                .and_then(|manifest| manifest.updated_at.as_deref())
        ),
        protocol_job_inventory.unwrap_or_else(|| "not_checked".to_string())
    )
    .trim_end()
    .to_string())
}

fn shell_supervisor_status_label(
    manifest_exists: bool,
    socket_ready: bool,
    supervisor_alive: Option<bool>,
    protocol_health: &str,
) -> &'static str {
    if manifest_exists && socket_ready && supervisor_alive == Some(true) && protocol_health != "ok"
    {
        return "protocol_unhealthy";
    }
    match (manifest_exists, socket_ready, supervisor_alive) {
        (false, false, _) => "not_configured",
        (false, true, _) => "socket_without_manifest",
        (true, true, Some(true)) => "ready",
        (true, _, Some(false)) => "stale",
        (true, false, Some(true)) => "socket_missing",
        (true, true, None) => "socket_without_pid",
        (true, false, None) => "manifest_only",
    }
}

fn shell_supervisor_protocol_health(socket_path: &Path, socket_ready: bool) -> String {
    if !socket_ready {
        return "not_checked".to_string();
    }
    #[cfg(unix)]
    {
        match shell_supervisor_protocol_health_unix(socket_path) {
            Ok(label) => label,
            Err(error) => format!("error: {}", shell_compact_error_label(&error.to_string())),
        }
    }
    #[cfg(not(unix))]
    {
        let _ = socket_path;
        "unsupported".to_string()
    }
}

#[cfg(unix)]
fn shell_supervisor_protocol_health_unix(socket_path: &Path) -> AppResult<String> {
    let root = shell_supervisor_protocol_request_unix(socket_path, "health")?;
    if root.get("method").and_then(json_as_string) == Some("health")
        && root.get("status").and_then(json_as_string) == Some("ok")
    {
        Ok("ok".to_string())
    } else {
        Ok(format!(
            "unexpected_response: {}",
            shell_compact_error_label(&json_value_to_string(&JsonValue::Object(root)))
        ))
    }
}

fn shell_supervisor_protocol_status(
    socket_path: &Path,
    socket_ready: bool,
    protocol_health: &str,
) -> (String, Option<String>) {
    if !socket_ready {
        return ("not_checked".to_string(), None);
    }
    if protocol_health != "ok" {
        return ("not_checked".to_string(), None);
    }
    #[cfg(unix)]
    {
        match shell_supervisor_protocol_status_unix(socket_path) {
            Ok((label, active_jobs)) => (label, active_jobs),
            Err(error) => (
                format!("error: {}", shell_compact_error_label(&error.to_string())),
                None,
            ),
        }
    }
    #[cfg(not(unix))]
    {
        let _ = socket_path;
        ("unsupported".to_string(), None)
    }
}

#[cfg(unix)]
fn shell_supervisor_protocol_status_unix(
    socket_path: &Path,
) -> AppResult<(String, Option<String>)> {
    let root = shell_supervisor_protocol_request_unix(socket_path, "status")?;
    if root.get("method").and_then(json_as_string) == Some("status")
        && root.get("status").and_then(json_as_string) == Some("ok")
    {
        let active_jobs = root
            .get("active_jobs")
            .and_then(json_as_u64)
            .map(|count| count.to_string());
        return Ok(("ok".to_string(), active_jobs));
    }
    Ok((
        format!(
            "unexpected_response: {}",
            shell_compact_error_label(&json_value_to_string(&JsonValue::Object(root)))
        ),
        None,
    ))
}

fn shell_supervisor_protocol_show(
    socket_path: &Path,
    socket_ready: bool,
    protocol_health: &str,
) -> (String, Option<String>) {
    if !socket_ready {
        return ("not_checked".to_string(), None);
    }
    if protocol_health != "ok" {
        return ("not_checked".to_string(), None);
    }
    #[cfg(unix)]
    {
        match shell_supervisor_protocol_show_unix(socket_path) {
            Ok((label, inventory)) => (label, inventory),
            Err(error) => (
                format!("error: {}", shell_compact_error_label(&error.to_string())),
                None,
            ),
        }
    }
    #[cfg(not(unix))]
    {
        let _ = socket_path;
        ("unsupported".to_string(), None)
    }
}

#[cfg(unix)]
fn shell_supervisor_protocol_show_unix(socket_path: &Path) -> AppResult<(String, Option<String>)> {
    let root = shell_supervisor_protocol_request_unix(socket_path, "show")?;
    if root.get("method").and_then(json_as_string) == Some("show")
        && root.get("status").and_then(json_as_string) == Some("ok")
    {
        let inventory = root
            .get("job_inventory")
            .and_then(json_as_string)
            .map(str::to_string);
        if let Some(error) = root.get("job_inventory_error").and_then(json_as_string) {
            return Ok((
                format!("inventory_error: {}", shell_compact_error_label(error)),
                inventory,
            ));
        }
        return Ok(("ok".to_string(), inventory));
    }
    Ok((
        format!(
            "unexpected_response: {}",
            shell_compact_error_label(&json_value_to_string(&JsonValue::Object(root)))
        ),
        None,
    ))
}

#[cfg(unix)]
fn shell_supervisor_protocol_request_unix(
    socket_path: &Path,
    method: &str,
) -> AppResult<BTreeMap<String, JsonValue>> {
    use std::io::{BufRead, BufReader, ErrorKind};
    use std::os::unix::net::UnixStream;

    let mut stream = UnixStream::connect(socket_path)
        .map_err(|error| app_error(format!("{method} connect failed: {error}")))?;
    stream
        .set_read_timeout(Some(Duration::from_millis(250)))
        .map_err(|error| app_error(format!("{method} read timeout setup failed: {error}")))?;
    stream
        .set_write_timeout(Some(Duration::from_millis(250)))
        .map_err(|error| app_error(format!("{method} write timeout setup failed: {error}")))?;
    stream
        .write_all(format!("{{\"method\":\"{method}\"}}\n").as_bytes())
        .map_err(|error| app_error(format!("{method} request write failed: {error}")))?;

    let mut response = String::new();
    let mut reader = BufReader::new(stream);
    match reader.read_line(&mut response) {
        Ok(0) => return Err(app_error(format!("{method} response was empty"))),
        Ok(_) => {}
        Err(error) if matches!(error.kind(), ErrorKind::TimedOut | ErrorKind::WouldBlock) => {
            return Err(app_error(format!("{method} response timed out")));
        }
        Err(error) => return Err(app_error(format!("{method} response read failed: {error}"))),
    }

    parse_root_object(response.trim()).map_err(|error| {
        app_error(format!(
            "{method} response was not valid JSON: {}; response={}",
            error,
            shell_compact_error_label(response.trim())
        ))
    })
}

fn shell_compact_error_label(value: &str) -> String {
    let compact = value.split_whitespace().collect::<Vec<_>>().join(" ");
    let clipped = compact.chars().take(160).collect::<String>();
    if clipped.len() < compact.len() {
        format!("{clipped}...")
    } else {
        compact
    }
}

fn shell_supervisor_method_label(methods: Option<&[String]>, fallback: &[&str]) -> String {
    match methods.filter(|methods| !methods.is_empty()) {
        Some(methods) => methods.join(","),
        None => fallback.join(","),
    }
}

fn shell_supervisor_platform_label() -> &'static str {
    #[cfg(unix)]
    {
        "unix"
    }
    #[cfg(not(unix))]
    {
        "unsupported"
    }
}

fn read_shell_supervisor_manifest(path: &Path) -> AppResult<ShellSupervisorManifest> {
    let content = fs::read_to_string(path)?;
    let root = parse_root_object(&content)?;
    Ok(ShellSupervisorManifest {
        kind: root
            .get("kind")
            .and_then(json_as_string)
            .map(str::to_string),
        supervisor_pid: root
            .get("supervisor_pid")
            .and_then(json_as_u64)
            .map(|pid| pid as u32),
        supervisor_socket: root
            .get("supervisor_socket")
            .and_then(json_as_string)
            .map(str::to_string),
        supervisor_epoch: root
            .get("supervisor_epoch")
            .and_then(json_as_string)
            .map(str::to_string),
        protocol: root
            .get("protocol")
            .and_then(json_as_string)
            .map(str::to_string),
        methods: json_string_array(root.get("methods")),
        unsupported_methods: json_string_array(root.get("unsupported_methods")),
        started_at: root
            .get("started_at")
            .and_then(json_as_string)
            .map(str::to_string),
        updated_at: root
            .get("updated_at")
            .and_then(json_as_string)
            .map(str::to_string),
        active_jobs: root.get("active_jobs").and_then(json_as_u64),
    })
}

fn json_string_array(value: Option<&JsonValue>) -> Vec<String> {
    let Some(JsonValue::Array(items)) = value else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(json_as_string)
        .map(str::to_string)
        .collect()
}

fn render_durable_snapshot(cwd: &str, task_id: &str) -> AppResult<String> {
    let manifest = shell_job_manifest_path(cwd, task_id);
    if !manifest.is_file() {
        return Err(app_error(format!(
            "unknown background shell task: {task_id}"
        )));
    }
    let mut record = read_durable_shell_job_manifest(&manifest)?;
    refresh_durable_running_status(cwd, &mut record)?;
    let record_dir = shell_job_record_dir(cwd, task_id);
    let stdout = read_durable_log(&record_dir, "stdout.log");
    let stderr = read_durable_log(&record_dir, "stderr.log");
    let stdin_control = if record.stdin_path.is_some() && !record.stdin_closed {
        "detached_fifo"
    } else {
        "unavailable"
    };
    let mut out = format!(
        "task_id: {}\nstatus: {}\nmanaged: false\nexit_code: {}\npid: {}\nowner_pid: {}\nowner_alive: {}\nprocess_group: {}\ncommand: {}\ncwd: {}\ntty: {}\npty_backend: {}\nattachable: {}\nresizable: {}\nsupervisor_pid: {}\nsupervisor_alive: {}\nsupervisor_socket: {}\nsupervisor_epoch: {}\nterminal_event_log: {}\nterminal_event_seq: {}\ntty_rows: {}\ntty_cols: {}\nstarted_at: {}\nupdated_at: {}\nstdout_total_bytes: {}\nstderr_total_bytes: {}\nstdin_control: {}\nnote: durable metadata and logs are available; detached cancel is best-effort and detached stdin is available only when stdin_control=detached_fifo. attachable/resizable are true only for future native supervisor-owned PTY sessions.\n",
        record.id,
        record.status,
        record
            .exit_code
            .map(|code| code.to_string())
            .unwrap_or_else(|| "null".to_string()),
        record.pid,
        shell_optional_pid_label(record.owner_pid),
        owner_alive_label(record.owner_pid),
        shell_optional_pid_label(record.process_group),
        record.command,
        record.cwd,
        record.tty,
        durable_pty_backend_label(&record),
        record.attachable,
        record.resizable,
        shell_optional_pid_label(record.supervisor_pid),
        owner_alive_label(record.supervisor_pid),
        shell_optional_string_label(record.supervisor_socket.as_deref()),
        shell_optional_string_label(record.supervisor_epoch.as_deref()),
        shell_optional_string_label(record.terminal_event_log.as_deref()),
        shell_optional_u64_label(record.terminal_event_seq),
        tty_rows_label(record.tty_size),
        tty_cols_label(record.tty_size),
        record.started_at,
        record.updated_at,
        record.stdout_total_bytes,
        record.stderr_total_bytes,
        stdin_control
    );
    if !stdout.trim().is_empty() {
        out.push_str("stdout:\n");
        out.push_str(&clip_shell_snapshot(&stdout));
        out.push('\n');
    }
    if !stderr.trim().is_empty() {
        out.push_str("stderr:\n");
        out.push_str(&clip_shell_snapshot(&stderr));
        out.push('\n');
    }
    Ok(out.trim_end().to_string())
}

fn render_shell_replay(
    cwd: &str,
    task_id: &str,
    stream: &str,
    offset: usize,
    limit_bytes: usize,
    tail: bool,
) -> AppResult<String> {
    let manifest = shell_job_manifest_path(cwd, task_id);
    if !manifest.is_file() {
        return Err(app_error(format!(
            "unknown background shell task: {task_id}"
        )));
    }
    let mut record = read_durable_shell_job_manifest(&manifest)?;
    refresh_durable_running_status(cwd, &mut record)?;
    let record_dir = shell_job_record_dir(cwd, task_id);
    match stream {
        "stdout" | "out" => Ok(render_shell_replay_stream(
            &record,
            &record_dir,
            "stdout",
            "stdout.log",
            offset,
            limit_bytes,
            tail,
        )),
        "stderr" | "err" => Ok(render_shell_replay_stream(
            &record,
            &record_dir,
            "stderr",
            "stderr.log",
            offset,
            limit_bytes,
            tail,
        )),
        "all" | "both" => {
            let stdout = render_shell_replay_stream(
                &record,
                &record_dir,
                "stdout",
                "stdout.log",
                offset,
                limit_bytes,
                tail,
            );
            let stderr = render_shell_replay_stream(
                &record,
                &record_dir,
                "stderr",
                "stderr.log",
                offset,
                limit_bytes,
                tail,
            );
            Ok(format!("{stdout}\n---\n{stderr}"))
        }
        _ => Err(app_error(
            "exec_shell_replay stream must be stdout, stderr, or all",
        )),
    }
}

fn render_shell_attach(
    cwd: &str,
    task_id: &str,
    offset: usize,
    limit_bytes: usize,
    tail: bool,
    wait_ms: u64,
) -> AppResult<String> {
    let deadline = Instant::now() + Duration::from_millis(wait_ms);
    loop {
        let manifest = shell_job_manifest_path(cwd, task_id);
        if !manifest.is_file() {
            return Err(app_error(format!(
                "unknown background shell task: {task_id}"
            )));
        }
        let mut record = read_durable_shell_job_manifest(&manifest)?;
        refresh_durable_running_status(cwd, &mut record)?;
        let record_dir = shell_job_record_dir(cwd, task_id);
        let total = durable_log_bytes(&record_dir, "stdout.log", record.stdout_total_bytes);
        let should_return = tail
            || wait_ms == 0
            || total > offset
            || record.status != "running"
            || Instant::now() >= deadline;
        if should_return {
            let timed_out = wait_ms > 0 && !tail && total <= offset && record.status == "running";
            return Ok(render_shell_attach_snapshot(
                &record,
                &record_dir,
                offset,
                limit_bytes,
                tail,
                wait_ms,
                timed_out,
            ));
        }
        thread::sleep(Duration::from_millis(25));
    }
}

fn render_shell_attach_snapshot(
    record: &DurableShellJobRecord,
    record_dir: &Path,
    offset: usize,
    limit_bytes: usize,
    tail: bool,
    wait_ms: u64,
    timed_out: bool,
) -> String {
    let bytes = fs::read(record_dir.join("stdout.log")).unwrap_or_default();
    let total = bytes.len();
    let start = if tail {
        total.saturating_sub(limit_bytes)
    } else {
        offset.min(total)
    };
    let end = start.saturating_add(limit_bytes).min(total);
    let data = String::from_utf8_lossy(&bytes[start..end]);
    let mut out = format!(
        "task_id: {}\nstatus: {}\nmode: terminal_attach_replay\ncommand: {}\ncwd: {}\ntty: {}\npty_backend: {}\nattachable: {}\nresizable: {}\nsupervisor_pid: {}\nsupervisor_alive: {}\nsupervisor_socket: {}\nsupervisor_epoch: {}\nterminal_event_log: {}\nterminal_event_seq: {}\ntty_rows: {}\ntty_cols: {}\nterminal_stream: stdout\noffset: {start}\nnext_offset: {end}\ntotal_bytes: {total}\ntail: {tail}\nwait_ms: {wait_ms}\ntimed_out: {timed_out}\nnote: attach replay is backed by durable stdout PTY/log bytes, not a resident PTY takeover; attachable=true is reserved for future native supervisor-owned PTY sessions; use exec_shell_replay stream=stderr for stderr-only logs.\n",
        record.id,
        record.status,
        record.command,
        record.cwd,
        record.tty,
        durable_pty_backend_label(record),
        record.attachable,
        record.resizable,
        shell_optional_pid_label(record.supervisor_pid),
        owner_alive_label(record.supervisor_pid),
        shell_optional_string_label(record.supervisor_socket.as_deref()),
        shell_optional_string_label(record.supervisor_epoch.as_deref()),
        shell_optional_string_label(record.terminal_event_log.as_deref()),
        shell_optional_u64_label(record.terminal_event_seq),
        tty_rows_label(record.tty_size),
        tty_cols_label(record.tty_size)
    );
    if !data.is_empty() {
        out.push_str("terminal:\n");
        out.push_str(data.trim_end_matches('\n'));
        out.push('\n');
    }
    out.trim_end().to_string()
}

fn render_shell_replay_stream(
    record: &DurableShellJobRecord,
    record_dir: &Path,
    stream: &str,
    log_name: &str,
    offset: usize,
    limit_bytes: usize,
    tail: bool,
) -> String {
    let bytes = fs::read(record_dir.join(log_name)).unwrap_or_default();
    let total = bytes.len();
    let start = if tail {
        total.saturating_sub(limit_bytes)
    } else {
        offset.min(total)
    };
    let end = start.saturating_add(limit_bytes).min(total);
    let data = String::from_utf8_lossy(&bytes[start..end]);
    let mut out = format!(
        "task_id: {}\nstatus: {}\nstream: {stream}\noffset: {start}\nnext_offset: {end}\ntotal_bytes: {total}\ntail: {tail}\n",
        record.id, record.status
    );
    if !data.is_empty() {
        out.push_str("data:\n");
        out.push_str(data.trim_end_matches('\n'));
        out.push('\n');
    }
    out.trim_end().to_string()
}

fn read_durable_shell_job_manifest(path: &Path) -> AppResult<DurableShellJobRecord> {
    let content = fs::read_to_string(path)?;
    let root = parse_root_object(&content)?;
    let record_dir = path.parent().unwrap_or_else(|| Path::new("."));
    let manifest_stdout_total = root
        .get("stdout_total_bytes")
        .and_then(json_as_u64)
        .unwrap_or(0) as usize;
    let manifest_stderr_total = root
        .get("stderr_total_bytes")
        .and_then(json_as_u64)
        .unwrap_or(0) as usize;
    let tty = matches!(root.get("tty"), Some(JsonValue::Bool(true)));
    let pty_backend = root
        .get("pty_backend")
        .and_then(json_as_string)
        .map(str::to_string)
        .or_else(|| tty.then(|| PTY_BACKEND_SCRIPT.to_string()));
    Ok(DurableShellJobRecord {
        id: required_manifest_string(&root, "id")?,
        command: required_manifest_string(&root, "command")?,
        cwd: required_manifest_string(&root, "cwd")?,
        tty,
        pty_backend,
        tty_size: manifest_tty_size(&root)?,
        status: required_manifest_string(&root, "status")?,
        exit_code: match root.get("exit_code") {
            Some(JsonValue::Null) | None => None,
            Some(value) => json_as_u64(value).map(|value| value as i32),
        },
        pid: root.get("pid").and_then(json_as_u64).unwrap_or(0) as u32,
        owner_pid: root
            .get("owner_pid")
            .and_then(json_as_u64)
            .map(|pid| pid as u32),
        process_group: root
            .get("process_group")
            .and_then(json_as_u64)
            .map(|pid| pid as u32),
        supervisor_pid: root
            .get("supervisor_pid")
            .and_then(json_as_u64)
            .map(|pid| pid as u32),
        supervisor_socket: root
            .get("supervisor_socket")
            .and_then(json_as_string)
            .map(str::to_string),
        supervisor_epoch: root
            .get("supervisor_epoch")
            .and_then(json_as_string)
            .map(str::to_string),
        terminal_event_log: root
            .get("terminal_event_log")
            .and_then(json_as_string)
            .map(str::to_string),
        terminal_event_seq: root.get("terminal_event_seq").and_then(json_as_u64),
        control_token_hash: root
            .get("control_token_hash")
            .and_then(json_as_string)
            .map(str::to_string),
        attachable: matches!(root.get("attachable"), Some(JsonValue::Bool(true))),
        resizable: matches!(root.get("resizable"), Some(JsonValue::Bool(true))),
        stdin_path: root
            .get("stdin_path")
            .and_then(json_as_string)
            .map(str::to_string),
        stdin_keeper_pid: root
            .get("stdin_keeper_pid")
            .and_then(json_as_u64)
            .map(|pid| pid as u32),
        stdin_closed: matches!(root.get("stdin_closed"), Some(JsonValue::Bool(true))),
        started_at: required_manifest_string(&root, "started_at")?,
        updated_at: required_manifest_string(&root, "updated_at")?,
        stdout_total_bytes: durable_log_bytes(record_dir, "stdout.log", manifest_stdout_total),
        stderr_total_bytes: durable_log_bytes(record_dir, "stderr.log", manifest_stderr_total),
    })
}

fn manifest_tty_size(root: &BTreeMap<String, JsonValue>) -> AppResult<Option<ShellTtySize>> {
    let rows = manifest_tty_dimension(root, "tty_rows")?;
    let cols = manifest_tty_dimension(root, "tty_cols")?;
    match (rows, cols) {
        (Some(rows), Some(cols)) => Ok(Some(ShellTtySize { rows, cols })),
        (None, None) => Ok(None),
        _ => Err(app_error(
            "exec_shell manifest tty_rows and tty_cols must both be present",
        )),
    }
}

fn manifest_tty_dimension(root: &BTreeMap<String, JsonValue>, key: &str) -> AppResult<Option<u16>> {
    let Some(value) = root.get(key).and_then(json_as_u64) else {
        return Ok(None);
    };
    if value == 0 || value > 2000 {
        return Err(app_error(format!(
            "exec_shell manifest {key} must be between 1 and 2000"
        )));
    }
    Ok(Some(value as u16))
}

fn refresh_durable_running_status(cwd: &str, record: &mut DurableShellJobRecord) -> AppResult<()> {
    if record.status == "running" && record.pid > 0 && !detached_process_is_alive(record.pid) {
        if let Some(keeper_pid) = record.stdin_keeper_pid {
            let _ = kill_process(keeper_pid);
        }
        record.status = "exited".to_string();
        record.stdin_closed = true;
        record.stdin_keeper_pid = None;
        record.updated_at = epoch_label();
        write_durable_shell_job_manifest(cwd, record)?;
    }
    Ok(())
}

fn required_manifest_string(root: &BTreeMap<String, JsonValue>, key: &str) -> AppResult<String> {
    root.get(key)
        .and_then(json_as_string)
        .map(str::to_string)
        .ok_or_else(|| app_error(format!("exec_shell manifest missing string `{key}`")))
}

fn durable_log_bytes(record_dir: &Path, name: &str, fallback: usize) -> usize {
    fs::metadata(record_dir.join(name))
        .map(|metadata| metadata.len() as usize)
        .unwrap_or(fallback)
}

fn read_log_delta(record_dir: &Path, name: &str, cursor: &mut usize) -> String {
    let bytes = fs::read(record_dir.join(name)).unwrap_or_default();
    let start = (*cursor).min(bytes.len());
    let delta = String::from_utf8_lossy(&bytes[start..]).to_string();
    *cursor = bytes.len();
    delta
}

fn read_durable_log(record_dir: &Path, name: &str) -> String {
    fs::read(record_dir.join(name))
        .map(|bytes| String::from_utf8_lossy(&bytes).to_string())
        .unwrap_or_default()
}

fn shell_supervisor_socket_kind(path: &Path) -> &'static str {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return "missing";
    };
    let file_type = metadata.file_type();
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileTypeExt;
        if file_type.is_socket() {
            return "socket";
        }
    }
    if file_type.is_file() {
        "file"
    } else if file_type.is_dir() {
        "directory"
    } else if file_type.is_symlink() {
        "symlink"
    } else {
        "other"
    }
}

fn write_fifo_stdin(path: &Path, data: &str) -> AppResult<()> {
    let mut writer = OpenOptions::new().write(true).open(path)?;
    writer.write_all(data.as_bytes())?;
    writer.flush()?;
    Ok(())
}

#[cfg(unix)]
fn create_fifo(path: &Path) -> AppResult<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    if path.exists() {
        let _ = fs::remove_file(path);
    }
    let c_path = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| app_error("exec_shell fifo path contains nul byte"))?;
    unsafe extern "C" {
        fn mkfifo(path: *const std::os::raw::c_char, mode: u32) -> i32;
    }
    let result = unsafe { mkfifo(c_path.as_ptr(), 0o600) };
    if result == 0 {
        Ok(())
    } else {
        Err(app_error(format!(
            "failed to create exec_shell stdin fifo: {}",
            std::io::Error::last_os_error()
        )))
    }
}

#[cfg(unix)]
fn configure_process_group(process: &mut Command) {
    use std::os::unix::process::CommandExt;
    process.process_group(0);
}

#[cfg(not(unix))]
fn configure_process_group(_process: &mut Command) {}

#[cfg(unix)]
fn configure_process_group_id(process: &mut Command, process_group: u32) {
    use std::os::unix::process::CommandExt;
    process.process_group(process_group as i32);
}

fn kill_child_process_group(child: &mut Child) {
    #[cfg(unix)]
    {
        const SIGKILL: i32 = 9;
        extern "C" {
            fn kill(pid: i32, sig: i32) -> i32;
        }
        let process_group = -(child.id() as i32);
        unsafe {
            let _ = kill(process_group, SIGKILL);
        }
    }
    let _ = child.kill();
}

fn kill_process(pid: u32) -> AppResult<()> {
    if pid <= 1 || pid == std::process::id() {
        return Err(app_error(format!(
            "refusing to kill unsafe shell helper pid {pid}"
        )));
    }
    #[cfg(unix)]
    {
        const SIGKILL: i32 = 9;
        unsafe extern "C" {
            fn kill(pid: i32, sig: i32) -> i32;
        }
        let result = unsafe { kill(pid as i32, SIGKILL) };
        if result == 0 {
            return Ok(());
        }
        return Err(app_error(format!(
            "failed to kill shell helper process {pid}: {}",
            std::io::Error::last_os_error()
        )));
    }
    #[cfg(not(unix))]
    {
        Err(app_error(
            "shell helper process control is not supported on this platform",
        ))
    }
}

fn kill_detached_process_group(process_group: u32, pid: u32) -> AppResult<()> {
    if process_group <= 1
        || process_group > i32::MAX as u32
        || process_group == std::process::id()
        || pid <= 1
        || pid > i32::MAX as u32
        || pid == std::process::id()
    {
        return Err(app_error(format!(
            "refusing to cancel detached shell job with unsafe process group {process_group} and pid {pid}"
        )));
    }
    #[cfg(unix)]
    {
        const SIGKILL: i32 = 9;
        unsafe extern "C" {
            fn kill(pid: i32, sig: i32) -> i32;
        }
        let group_result = unsafe { kill(-(process_group as i32), SIGKILL) };
        if group_result == 0 {
            return Ok(());
        }
        let process_result = unsafe { kill(pid as i32, SIGKILL) };
        if process_result == 0 {
            return Ok(());
        }
        return Err(app_error(format!(
            "failed to cancel detached shell process {pid}: {}",
            std::io::Error::last_os_error()
        )));
    }
    #[cfg(not(unix))]
    {
        Err(app_error(
            "detached shell cancellation is not supported on this platform",
        ))
    }
}

fn detached_process_is_alive(pid: u32) -> bool {
    process_is_alive(pid)
}

fn process_is_alive(pid: u32) -> bool {
    if pid <= 1 || pid > i32::MAX as u32 {
        return false;
    }
    #[cfg(unix)]
    {
        if detached_process_is_zombie(pid) {
            reap_process_if_child(pid);
            return false;
        }
        unsafe extern "C" {
            fn kill(pid: i32, sig: i32) -> i32;
        }
        let result = unsafe { kill(pid as i32, 0) };
        if result == 0 {
            return true;
        }
        matches!(std::io::Error::last_os_error().raw_os_error(), Some(1))
    }
    #[cfg(not(unix))]
    {
        true
    }
}

#[cfg(unix)]
fn detached_process_is_zombie(pid: u32) -> bool {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).unwrap_or_default();
    let Some(after_name) = stat.rsplit_once(") ") else {
        return false;
    };
    after_name.1.starts_with("Z ")
}

#[cfg(unix)]
fn reap_process_if_child(pid: u32) {
    const WNOHANG: i32 = 1;
    unsafe extern "C" {
        fn waitpid(pid: i32, status: *mut i32, options: i32) -> i32;
    }
    let mut status = 0;
    unsafe {
        let _ = waitpid(pid as i32, &mut status, WNOHANG);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "deepseek-exec-shell-{label}-{}-{nanos}",
            std::process::id()
        ))
    }

    fn task_id_from(summary: &str) -> String {
        summary
            .lines()
            .find_map(|line| line.strip_prefix("task_id: "))
            .expect("task_id line")
            .to_string()
    }

    #[test]
    fn exec_shell_foreground_delegates_to_run_shell() {
        let output = ExecShellTool
            .execute(ToolInput::new().with_arg("command", "echo hello"))
            .unwrap();
        assert!(output.summary.contains("meta.result=ok"));
        assert!(output.summary.contains("hello"));
    }

    #[test]
    fn exec_shell_foreground_timeout_returns_completed_managed_snapshot() {
        let root = temp_root("foreground-complete");
        fs::create_dir_all(&root).unwrap();
        let cwd = root.display().to_string();
        let output = ExecShellTool
            .execute(
                ToolInput::new()
                    .with_arg("command", "echo managed")
                    .with_arg("cwd", cwd.clone())
                    .with_arg("timeout_ms", "1000"),
            )
            .unwrap();

        assert!(output.summary.contains("meta.foreground_managed=true"));
        assert!(output.summary.contains("meta.backgrounded=false"));
        assert!(output.summary.contains("status: completed"));
        assert!(output.summary.contains("managed"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn exec_shell_foreground_timeout_detaches_running_command() {
        let root = temp_root("foreground-detach");
        fs::create_dir_all(&root).unwrap();
        let cwd = root.display().to_string();
        let output = ExecShellTool
            .execute(
                ToolInput::new()
                    .with_arg("command", "tail -f /dev/null")
                    .with_arg("cwd", cwd.clone())
                    .with_arg("timeout_ms", "25"),
            )
            .unwrap();
        let task_id = task_id_from(&output.summary);

        assert!(output.summary.contains("meta.foreground_managed=true"));
        assert!(output.summary.contains("meta.backgrounded=true"));
        assert!(output.summary.contains("status: running"));
        assert!(output.summary.contains("Poll with exec_shell_wait"));

        ExecShellCancelTool
            .execute(
                ToolInput::new()
                    .with_arg("task_id", task_id)
                    .with_arg("cwd", cwd.clone()),
            )
            .unwrap();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn exec_shell_background_wait_reports_completion() {
        let root = temp_root("wait");
        fs::create_dir_all(&root).unwrap();
        let cwd = root.display().to_string();
        let started = ExecShellTool
            .execute(
                ToolInput::new()
                    .with_arg("command", "echo ready")
                    .with_arg("background", "true")
                    .with_arg("cwd", cwd.clone()),
            )
            .unwrap();
        let task_id = task_id_from(&started.summary);
        let waited = ExecShellWaitTool {
            tool_name: "exec_shell_wait",
        }
        .execute(
            ToolInput::new()
                .with_arg("task_id", task_id)
                .with_arg("cwd", cwd)
                .with_arg("timeout_ms", "1000"),
        )
        .unwrap();
        assert!(waited.summary.contains("status: completed"));
        assert!(waited.summary.contains("stdout_delta:"));
        assert!(waited.summary.contains("ready"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn exec_shell_background_applies_hidden_env_args() {
        let root = temp_root("env");
        fs::create_dir_all(&root).unwrap();
        let cwd = root.display().to_string();
        let started = ExecShellTool
            .execute(
                ToolInput::new()
                    .with_arg("command", "echo $DSCODE_EXEC_SHELL_ENV_TEST")
                    .with_arg("background", "true")
                    .with_arg("cwd", cwd.clone())
                    .with_arg("env.DSCODE_EXEC_SHELL_ENV_TEST", "from-exec-env"),
            )
            .unwrap();
        let task_id = task_id_from(&started.summary);
        let waited = ExecShellWaitTool {
            tool_name: "exec_shell_wait",
        }
        .execute(
            ToolInput::new()
                .with_arg("task_id", task_id)
                .with_arg("cwd", cwd.clone())
                .with_arg("timeout_ms", "1000"),
        )
        .unwrap();

        assert!(
            waited.summary.contains("from-exec-env"),
            "{}",
            waited.summary
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn exec_shell_list_and_show_report_jobs() {
        let root = temp_root("list-show");
        fs::create_dir_all(&root).unwrap();
        let cwd = root.display().to_string();
        let started = ExecShellTool
            .execute(
                ToolInput::new()
                    .with_arg("command", "echo listed")
                    .with_arg("background", "true")
                    .with_arg("cwd", cwd.clone()),
            )
            .unwrap();
        let task_id = task_id_from(&started.summary).to_string();
        let _ = ExecShellWaitTool {
            tool_name: "exec_shell_wait",
        }
        .execute(
            ToolInput::new()
                .with_arg("task_id", task_id.clone())
                .with_arg("cwd", cwd.clone())
                .with_arg("timeout_ms", "1000"),
        )
        .unwrap();

        let listed = ExecShellListTool
            .execute(ToolInput::new().with_arg("cwd", cwd.clone()))
            .unwrap();
        assert!(listed.summary.contains(&task_id), "{}", listed.summary);
        assert!(listed.summary.contains("echo listed"), "{}", listed.summary);

        let shown = ExecShellShowTool
            .execute(
                ToolInput::new()
                    .with_arg("task_id", task_id)
                    .with_arg("cwd", cwd),
            )
            .unwrap();
        assert!(shown.summary.contains("stdout:"), "{}", shown.summary);
        assert!(shown.summary.contains("listed"), "{}", shown.summary);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn exec_shell_background_writes_durable_record_for_detached_show() {
        let root = temp_root("durable");
        fs::create_dir_all(&root).unwrap();
        let cwd = root.display().to_string();
        let started = ExecShellTool
            .execute(
                ToolInput::new()
                    .with_arg("command", "echo durable")
                    .with_arg("background", "true")
                    .with_arg("cwd", cwd.clone()),
            )
            .unwrap();
        let task_id = task_id_from(&started.summary);
        let _ = ExecShellWaitTool {
            tool_name: "exec_shell_wait",
        }
        .execute(
            ToolInput::new()
                .with_arg("task_id", task_id.clone())
                .with_arg("cwd", cwd.clone())
                .with_arg("timeout_ms", "1000"),
        )
        .unwrap();
        let manifest = root
            .join(".dscode/shell-jobs")
            .join(&task_id)
            .join("manifest.json");
        assert!(manifest.is_file());
        assert!(manifest.with_file_name("stdout.log").is_file());

        shell_manager().lock().unwrap().jobs.remove(&task_id);

        let listed = ExecShellListTool
            .execute(ToolInput::new().with_arg("cwd", cwd.clone()))
            .unwrap();
        assert!(listed.summary.contains(&task_id), "{}", listed.summary);
        assert!(listed.summary.contains("detached"), "{}", listed.summary);

        let shown = ExecShellShowTool
            .execute(
                ToolInput::new()
                    .with_arg("task_id", task_id.clone())
                    .with_arg("cwd", cwd.clone()),
            )
            .unwrap();
        assert!(
            shown.summary.contains("managed: false"),
            "{}",
            shown.summary
        );
        assert!(shown.summary.contains("durable"), "{}", shown.summary);

        let waited = ExecShellWaitTool {
            tool_name: "exec_shell_wait",
        }
        .execute(
            ToolInput::new()
                .with_arg("task_id", task_id.clone())
                .with_arg("cwd", cwd.clone())
                .with_arg("timeout_ms", "1000"),
        )
        .unwrap();
        assert!(
            waited.summary.contains("managed: false"),
            "{}",
            waited.summary
        );

        let stdin_error = ExecShellInteractTool {
            tool_name: "exec_shell_interact",
        }
        .execute(
            ToolInput::new()
                .with_arg("task_id", task_id.clone())
                .with_arg("cwd", cwd.clone())
                .with_arg("input", "late input\n"),
        )
        .unwrap_err()
        .to_string();
        assert!(stdin_error.contains("detached"), "{stdin_error}");
        assert!(stdin_error.contains("completed"), "{stdin_error}");

        let cancel_output = ExecShellCancelTool
            .execute(
                ToolInput::new()
                    .with_arg("task_id", task_id)
                    .with_arg("cwd", cwd),
            )
            .unwrap();
        assert!(
            cancel_output
                .summary
                .contains("Detached background shell job is not running"),
            "{}",
            cancel_output.summary
        );
        assert!(cancel_output.summary.contains("status: completed"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn exec_shell_manifest_keeps_child_pid_after_completion() {
        let root = temp_root("owner-metadata");
        fs::create_dir_all(&root).unwrap();
        let cwd = root.display().to_string();
        let started = ExecShellTool
            .execute(
                ToolInput::new()
                    .with_arg("command", "echo owner-meta")
                    .with_arg("background", "true")
                    .with_arg("cwd", cwd.clone()),
            )
            .unwrap();
        let task_id = task_id_from(&started.summary);
        let manifest_path = root
            .join(".dscode/shell-jobs")
            .join(&task_id)
            .join("manifest.json");
        let before = read_durable_shell_job_manifest(&manifest_path).unwrap();
        assert_eq!(before.owner_pid, Some(std::process::id()));
        assert_eq!(before.process_group, Some(before.pid));
        assert_ne!(before.pid, std::process::id());

        let waited = ExecShellWaitTool {
            tool_name: "exec_shell_wait",
        }
        .execute(
            ToolInput::new()
                .with_arg("task_id", task_id.clone())
                .with_arg("cwd", cwd.clone())
                .with_arg("timeout_ms", "1000"),
        )
        .unwrap();
        assert!(waited.summary.contains("owner_pid:"), "{}", waited.summary);
        assert!(
            waited.summary.contains("owner_alive: true"),
            "{}",
            waited.summary
        );
        assert!(
            waited.summary.contains("process_group:"),
            "{}",
            waited.summary
        );

        let after = read_durable_shell_job_manifest(&manifest_path).unwrap();
        assert_eq!(after.pid, before.pid);
        assert_eq!(after.process_group, Some(before.pid));
        assert_eq!(after.owner_pid, Some(std::process::id()));
        shell_manager().lock().unwrap().jobs.remove(&task_id);

        let shown = ExecShellShowTool
            .execute(
                ToolInput::new()
                    .with_arg("task_id", task_id)
                    .with_arg("cwd", cwd.clone()),
            )
            .unwrap();
        assert!(
            shown.summary.contains("managed: false"),
            "{}",
            shown.summary
        );
        assert!(
            shown.summary.contains("owner_alive: true"),
            "{}",
            shown.summary
        );
        assert!(shown.summary.contains("owner-meta"), "{}", shown.summary);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn exec_shell_replay_reads_durable_log_offsets() {
        let root = temp_root("replay");
        fs::create_dir_all(&root).unwrap();
        let cwd = root.display().to_string();
        let task_id = shell_manager()
            .lock()
            .unwrap()
            .spawn(
                "printf 'alpha\\nbeta\\n'; printf 'warn\\n' >&2",
                &cwd,
                None,
                ShellTtyOptions::default(),
            )
            .unwrap();
        let _ = ExecShellWaitTool {
            tool_name: "exec_shell_wait",
        }
        .execute(
            ToolInput::new()
                .with_arg("task_id", task_id.clone())
                .with_arg("cwd", cwd.clone())
                .with_arg("timeout_ms", "1000"),
        )
        .unwrap();
        shell_manager().lock().unwrap().jobs.remove(&task_id);

        let first = ExecShellReplayTool
            .execute(
                ToolInput::new()
                    .with_arg("task_id", task_id.clone())
                    .with_arg("cwd", cwd.clone())
                    .with_arg("stream", "stdout")
                    .with_arg("offset", "0")
                    .with_arg("limit_bytes", "6"),
            )
            .unwrap();
        assert!(
            first.summary.contains("stream: stdout"),
            "{}",
            first.summary
        );
        assert!(first.summary.contains("offset: 0"), "{}", first.summary);
        assert!(
            first.summary.contains("next_offset: 6"),
            "{}",
            first.summary
        );
        assert!(first.summary.contains("alpha"), "{}", first.summary);
        assert!(!first.summary.contains("beta"), "{}", first.summary);

        let second = ExecShellReplayTool
            .execute(
                ToolInput::new()
                    .with_arg("task_id", task_id.clone())
                    .with_arg("cwd", cwd.clone())
                    .with_arg("stream", "stdout")
                    .with_arg("offset", "6")
                    .with_arg("limit_bytes", "20"),
            )
            .unwrap();
        assert!(second.summary.contains("offset: 6"), "{}", second.summary);
        assert!(second.summary.contains("beta"), "{}", second.summary);

        let stderr_tail = ExecShellReplayTool
            .execute(
                ToolInput::new()
                    .with_arg("task_id", task_id)
                    .with_arg("cwd", cwd.clone())
                    .with_arg("stream", "stderr")
                    .with_arg("tail", "true")
                    .with_arg("limit_bytes", "5"),
            )
            .unwrap();
        assert!(
            stderr_tail.summary.contains("stream: stderr"),
            "{}",
            stderr_tail.summary
        );
        assert!(
            stderr_tail.summary.contains("tail: true"),
            "{}",
            stderr_tail.summary
        );
        assert!(
            stderr_tail.summary.contains("warn"),
            "{}",
            stderr_tail.summary
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn exec_shell_attach_reads_terminal_replay_by_cursor() {
        let root = temp_root("attach");
        fs::create_dir_all(&root).unwrap();
        let cwd = root.display().to_string();
        let task_id = shell_manager()
            .lock()
            .unwrap()
            .spawn(
                "printf 'alpha\\nbeta\\n'; printf 'warn\\n' >&2",
                &cwd,
                None,
                ShellTtyOptions::default(),
            )
            .unwrap();
        let _ = ExecShellWaitTool {
            tool_name: "exec_shell_wait",
        }
        .execute(
            ToolInput::new()
                .with_arg("task_id", task_id.clone())
                .with_arg("cwd", cwd.clone())
                .with_arg("timeout_ms", "1000"),
        )
        .unwrap();
        shell_manager().lock().unwrap().jobs.remove(&task_id);

        let first = ExecShellAttachTool
            .execute(
                ToolInput::new()
                    .with_arg("task_id", task_id.clone())
                    .with_arg("cwd", cwd.clone())
                    .with_arg("cursor", "0")
                    .with_arg("limit_bytes", "6"),
            )
            .unwrap();
        assert!(
            first.summary.contains("mode: terminal_attach_replay"),
            "{}",
            first.summary
        );
        assert!(first.summary.contains("offset: 0"), "{}", first.summary);
        assert!(
            first.summary.contains("next_offset: 6"),
            "{}",
            first.summary
        );
        assert!(first.summary.contains("terminal:"), "{}", first.summary);
        let first_terminal = first
            .summary
            .split("terminal:\n")
            .nth(1)
            .unwrap_or_default();
        assert!(first_terminal.contains("alpha"), "{}", first.summary);
        assert!(!first_terminal.contains("beta"), "{}", first.summary);
        assert!(
            first
                .summary
                .contains("use exec_shell_replay stream=stderr"),
            "{}",
            first.summary
        );
        assert!(
            first.summary.contains("attachable: false"),
            "{}",
            first.summary
        );
        assert!(
            first.summary.contains("resizable: false"),
            "{}",
            first.summary
        );

        let second = ExecShellAttachTool
            .execute(
                ToolInput::new()
                    .with_arg("task_id", task_id)
                    .with_arg("cwd", cwd.clone())
                    .with_arg("cursor", "6")
                    .with_arg("limit_bytes", "20"),
            )
            .unwrap();
        assert!(second.summary.contains("offset: 6"), "{}", second.summary);
        let second_terminal = second
            .summary
            .split("terminal:\n")
            .nth(1)
            .unwrap_or_default();
        assert!(second_terminal.contains("beta"), "{}", second.summary);
        assert!(!second_terminal.contains("warn"), "{}", second.summary);

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn exec_shell_show_preserves_supervisor_manifest_capabilities() {
        let root = temp_root("supervisor-manifest");
        fs::create_dir_all(&root).unwrap();
        let cwd = root.display().to_string();
        let task_id = generated_job_id();
        let record_dir = shell_job_record_dir(&cwd, &task_id);
        fs::create_dir_all(&record_dir).unwrap();
        fs::write(record_dir.join("stdout.log"), b"supervised output\n").unwrap();
        fs::write(record_dir.join("stderr.log"), b"").unwrap();
        let record = DurableShellJobRecord {
            id: task_id.clone(),
            command: "bash".to_string(),
            cwd: cwd.clone(),
            tty: true,
            pty_backend: Some("native-supervisor".to_string()),
            tty_size: Some(ShellTtySize {
                rows: 44,
                cols: 132,
            }),
            status: "running".to_string(),
            exit_code: None,
            pid: 9_999_999,
            owner_pid: Some(std::process::id()),
            process_group: Some(9_999_999),
            supervisor_pid: Some(std::process::id()),
            supervisor_socket: Some(".dscode/shell-supervisor/supervisor.sock".to_string()),
            supervisor_epoch: Some("epoch+42".to_string()),
            terminal_event_log: Some("terminal-events.jsonl".to_string()),
            terminal_event_seq: Some(7),
            control_token_hash: Some("sha256:secret-token-hash".to_string()),
            attachable: true,
            resizable: true,
            stdin_path: None,
            stdin_keeper_pid: None,
            stdin_closed: true,
            started_at: epoch_label(),
            updated_at: epoch_label(),
            stdout_total_bytes: 0,
            stderr_total_bytes: 0,
        };
        write_durable_shell_job_manifest(&cwd, &record).unwrap();

        let shown = ExecShellShowTool
            .execute(
                ToolInput::new()
                    .with_arg("task_id", task_id.clone())
                    .with_arg("cwd", cwd.clone()),
            )
            .unwrap();
        assert!(
            shown.summary.contains("pty_backend: native-supervisor"),
            "{}",
            shown.summary
        );
        assert!(
            shown.summary.contains("status: exited"),
            "{}",
            shown.summary
        );
        assert!(
            shown.summary.contains("attachable: true"),
            "{}",
            shown.summary
        );
        assert!(
            shown.summary.contains("resizable: true"),
            "{}",
            shown.summary
        );
        assert!(
            shown.summary.contains("supervisor_alive: true"),
            "{}",
            shown.summary
        );
        assert!(
            shown
                .summary
                .contains("supervisor_socket: .dscode/shell-supervisor/supervisor.sock"),
            "{}",
            shown.summary
        );
        assert!(
            shown.summary.contains("terminal_event_seq: 7"),
            "{}",
            shown.summary
        );
        assert!(!shown.summary.contains("control_token_hash"));
        assert!(!shown.summary.contains("secret-token-hash"));
        assert!(
            shown.summary.contains("supervised output"),
            "{}",
            shown.summary
        );

        let listed = ExecShellListTool
            .execute(ToolInput::new().with_arg("cwd", cwd.clone()))
            .unwrap();
        assert!(
            listed.summary.contains("pty_backend=native-supervisor"),
            "{}",
            listed.summary
        );
        assert!(
            listed.summary.contains("attachable=true"),
            "{}",
            listed.summary
        );
        assert!(
            listed.summary.contains("resizable=true"),
            "{}",
            listed.summary
        );

        let loaded =
            read_durable_shell_job_manifest(&shell_job_manifest_path(&cwd, &task_id)).unwrap();
        assert_eq!(loaded.pty_backend.as_deref(), Some("native-supervisor"));
        assert_eq!(
            loaded.control_token_hash.as_deref(),
            Some("sha256:secret-token-hash")
        );
        assert!(loaded.attachable);
        assert!(loaded.resizable);

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn exec_shell_supervisor_status_reports_manifest_without_secret() {
        let root = temp_root("supervisor-status");
        fs::create_dir_all(&root).unwrap();
        let cwd = root.display().to_string();
        let absent = ExecShellSupervisorStatusTool
            .execute(ToolInput::new().with_arg("cwd", cwd.clone()))
            .unwrap();
        assert!(
            absent.summary.contains("status: not_configured"),
            "{}",
            absent.summary
        );
        assert!(
            absent
                .summary
                .contains(".dscode/shell-supervisor/supervisor.sock"),
            "{}",
            absent.summary
        );
        assert!(absent
            .summary
            .contains("methods: health,status,show,shutdown"));
        assert!(absent
            .summary
            .contains("unsupported_methods: start,wait,replay,attach,stdin,resize,cancel"));

        let state_dir = shell_supervisor_state_dir(&cwd);
        fs::create_dir_all(&state_dir).unwrap();
        fs::write(
            state_dir.join("manifest.json"),
            format!(
                "{{\"kind\":\"deepseek.exec_shell.supervisor.v1\",\"supervisor_pid\":{},\"supervisor_socket\":\"{}\",\"supervisor_epoch\":\"epoch+77\",\"protocol\":\"newline-json-v1\",\"methods\":[\"health\",\"status\",\"show\",\"shutdown\"],\"unsupported_methods\":[\"start\",\"attach\"],\"active_jobs\":2,\"started_at\":\"epoch+70\",\"updated_at\":\"epoch+76\",\"control_token_hash\":\"sha256:do-not-print\"}}",
                std::process::id(),
                state_dir.join("supervisor.sock").display()
            ),
        )
        .unwrap();

        let status = ExecShellSupervisorStatusTool
            .execute(ToolInput::new().with_arg("cwd", cwd.clone()))
            .unwrap();
        assert!(
            status.summary.contains("status: socket_missing"),
            "{}",
            status.summary
        );
        assert!(
            status.summary.contains("supervisor_alive: true"),
            "{}",
            status.summary
        );
        assert!(
            status.summary.contains("supervisor_epoch: epoch+77"),
            "{}",
            status.summary
        );
        assert!(
            status.summary.contains("protocol: newline-json-v1"),
            "{}",
            status.summary
        );
        assert!(
            status
                .summary
                .contains("methods: health,status,show,shutdown"),
            "{}",
            status.summary
        );
        assert!(
            status.summary.contains("unsupported_methods: start,attach"),
            "{}",
            status.summary
        );
        assert!(
            status.summary.contains("active_jobs: 2"),
            "{}",
            status.summary
        );
        assert!(!status.summary.contains("control_token_hash"));
        assert!(!status.summary.contains("do-not-print"));

        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn exec_shell_supervisor_status_probes_read_only_protocol_methods() {
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::net::UnixListener;

        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("dsh-{}-{nanos}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let cwd = root.display().to_string();
        let state_dir = shell_supervisor_state_dir(&cwd);
        fs::create_dir_all(&state_dir).unwrap();
        let socket = state_dir.join("supervisor.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        fs::write(
            state_dir.join("manifest.json"),
            format!(
                "{{\"kind\":\"deepseek.exec_shell.supervisor.v1\",\"supervisor_pid\":{},\"supervisor_socket\":\"{}\",\"supervisor_epoch\":\"epoch+health\",\"protocol\":\"newline-json-v1\",\"methods\":[\"health\",\"status\",\"show\",\"shutdown\"],\"unsupported_methods\":[\"start\",\"attach\"],\"active_jobs\":0,\"started_at\":\"epoch+70\",\"updated_at\":\"epoch+76\"}}",
                std::process::id(),
                socket.display()
            ),
        )
        .unwrap();

        let handle = std::thread::spawn(move || {
            for (expected_method, response) in [
                (
                    "health",
                    br#"{"kind":"deepseek.exec_shell.supervisor.response.v1","method":"health","status":"ok"}"#.as_slice(),
                ),
                (
                    "status",
                    br#"{"kind":"deepseek.exec_shell.supervisor.response.v1","method":"status","status":"ok","active_jobs":7}"#.as_slice(),
                ),
                (
                    "show",
                    br#"{"kind":"deepseek.exec_shell.supervisor.response.v1","method":"show","status":"ok","job_inventory":"No background shell jobs."}"#.as_slice(),
                ),
            ] {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = String::new();
                {
                    let mut reader = BufReader::new(&mut stream);
                    reader.read_line(&mut request).unwrap();
                }
                assert!(request.contains(&format!(r#""method":"{expected_method}""#)));
                stream.write_all(response).unwrap();
                stream.write_all(b"\n").unwrap();
            }
        });

        let status = ExecShellSupervisorStatusTool
            .execute(ToolInput::new().with_arg("cwd", cwd.clone()))
            .unwrap();

        handle.join().unwrap();
        assert!(
            status.summary.contains("status: ready"),
            "{}",
            status.summary
        );
        assert!(
            status.summary.contains("protocol_health: ok"),
            "{}",
            status.summary
        );
        assert!(
            status.summary.contains("protocol_status: ok"),
            "{}",
            status.summary
        );
        assert!(
            status.summary.contains("protocol_status_active_jobs: 7"),
            "{}",
            status.summary
        );
        assert!(
            status.summary.contains("protocol_show: ok"),
            "{}",
            status.summary
        );
        assert!(
            status.summary.contains("protocol_job_inventory:"),
            "{}",
            status.summary
        );
        assert!(
            status.summary.contains("No background shell jobs."),
            "{}",
            status.summary
        );

        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn exec_shell_interact_writes_detached_fifo_stdin() {
        let root = temp_root("detached-stdin");
        fs::create_dir_all(&root).unwrap();
        let cwd = root.display().to_string();
        let started = ExecShellTool
            .execute(
                ToolInput::new()
                    .with_arg("command", "cat -")
                    .with_arg("background", "true")
                    .with_arg("cwd", cwd.clone()),
            )
            .unwrap();
        let task_id = task_id_from(&started.summary);
        shell_manager().lock().unwrap().jobs.remove(&task_id);

        let interacted = ExecShellInteractTool {
            tool_name: "exec_shell_interact",
        }
        .execute(
            ToolInput::new()
                .with_arg("task_id", task_id.clone())
                .with_arg("cwd", cwd.clone())
                .with_arg("input", "detached stdin\n")
                .with_arg("close_stdin", "true")
                .with_arg("timeout_ms", "1000"),
        )
        .unwrap();
        assert!(
            interacted.summary.contains("meta.detached_stdin=true"),
            "{}",
            interacted.summary
        );
        assert!(
            interacted.summary.contains("detached stdin"),
            "{}",
            interacted.summary
        );
        assert!(
            interacted.summary.contains("stdin_control: unavailable"),
            "{}",
            interacted.summary
        );

        let shown = ExecShellShowTool
            .execute(
                ToolInput::new()
                    .with_arg("task_id", task_id)
                    .with_arg("cwd", cwd.clone()),
            )
            .unwrap();
        assert!(
            shown.summary.contains("detached stdin"),
            "{}",
            shown.summary
        );
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn exec_shell_cancel_kills_running_detached_durable_job() {
        let root = temp_root("detached-cancel");
        fs::create_dir_all(&root).unwrap();
        let cwd = root.display().to_string();

        let mut process = Command::new("sh");
        process
            .args(["-lc", "sleep 30"])
            .current_dir(&root)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        configure_process_group(&mut process);
        let mut child = process.spawn().unwrap();
        let task_id = generated_job_id();
        let record_dir = shell_job_record_dir(&cwd, &task_id);
        fs::create_dir_all(&record_dir).unwrap();
        fs::write(record_dir.join("stdout.log"), []).unwrap();
        fs::write(record_dir.join("stderr.log"), []).unwrap();
        let record = DurableShellJobRecord {
            id: task_id.clone(),
            command: "sleep 30".to_string(),
            cwd: cwd.clone(),
            tty: false,
            pty_backend: None,
            tty_size: None,
            status: "running".to_string(),
            exit_code: None,
            pid: child.id(),
            owner_pid: Some(999_998),
            process_group: Some(child.id()),
            supervisor_pid: None,
            supervisor_socket: None,
            supervisor_epoch: None,
            terminal_event_log: None,
            terminal_event_seq: None,
            control_token_hash: None,
            attachable: false,
            resizable: false,
            stdin_path: None,
            stdin_keeper_pid: None,
            stdin_closed: true,
            started_at: epoch_label(),
            updated_at: epoch_label(),
            stdout_total_bytes: 0,
            stderr_total_bytes: 0,
        };
        write_durable_shell_job_manifest(&cwd, &record).unwrap();

        let cancelled = ExecShellCancelTool
            .execute(
                ToolInput::new()
                    .with_arg("task_id", task_id.clone())
                    .with_arg("cwd", cwd.clone()),
            )
            .unwrap();
        assert!(
            cancelled
                .summary
                .contains("Canceled detached background shell job"),
            "{}",
            cancelled.summary
        );
        assert!(cancelled.summary.contains("managed: false"));

        let status = child.wait().unwrap();
        assert!(!status.success());

        let shown = ExecShellShowTool
            .execute(
                ToolInput::new()
                    .with_arg("task_id", task_id)
                    .with_arg("cwd", cwd),
            )
            .unwrap();
        assert!(
            shown.summary.contains("status: killed"),
            "{}",
            shown.summary
        );
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn exec_shell_cancel_all_kills_running_detached_durable_jobs() {
        let root = temp_root("detached-cancel-all");
        fs::create_dir_all(&root).unwrap();
        let cwd = root.display().to_string();

        let mut children = Vec::new();
        let mut task_ids = Vec::new();
        for index in 0..2 {
            let mut process = Command::new("sh");
            process
                .args(["-lc", "sleep 30"])
                .current_dir(&root)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            configure_process_group(&mut process);
            let child = process.spawn().unwrap();
            let task_id = generated_job_id();
            let record_dir = shell_job_record_dir(&cwd, &task_id);
            fs::create_dir_all(&record_dir).unwrap();
            fs::write(record_dir.join("stdout.log"), []).unwrap();
            fs::write(record_dir.join("stderr.log"), []).unwrap();
            let record = DurableShellJobRecord {
                id: task_id.clone(),
                command: format!("sleep 30 #{index}"),
                cwd: cwd.clone(),
                tty: false,
                pty_backend: None,
                tty_size: None,
                status: "running".to_string(),
                exit_code: None,
                pid: child.id(),
                owner_pid: Some(999_998),
                process_group: Some(child.id()),
                supervisor_pid: None,
                supervisor_socket: None,
                supervisor_epoch: None,
                terminal_event_log: None,
                terminal_event_seq: None,
                control_token_hash: None,
                attachable: false,
                resizable: false,
                stdin_path: None,
                stdin_keeper_pid: None,
                stdin_closed: true,
                started_at: epoch_label(),
                updated_at: epoch_label(),
                stdout_total_bytes: 0,
                stderr_total_bytes: 0,
            };
            write_durable_shell_job_manifest(&cwd, &record).unwrap();
            task_ids.push(task_id);
            children.push(child);
        }

        let cancelled = ExecShellCancelTool
            .execute(
                ToolInput::new()
                    .with_arg("all", "true")
                    .with_arg("cwd", cwd.clone()),
            )
            .unwrap();
        assert!(
            cancelled
                .summary
                .contains("Canceled 2 background shell jobs."),
            "{}",
            cancelled.summary
        );
        assert!(
            cancelled.summary.contains("detached:"),
            "{}",
            cancelled.summary
        );

        for mut child in children {
            let status = child.wait().unwrap();
            assert!(!status.success());
        }
        for task_id in task_ids {
            let shown = ExecShellShowTool
                .execute(
                    ToolInput::new()
                        .with_arg("task_id", task_id)
                        .with_arg("cwd", cwd.clone()),
                )
                .unwrap();
            assert!(
                shown.summary.contains("status: killed"),
                "{}",
                shown.summary
            );
        }

        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn exec_shell_show_marks_stale_running_detached_job_exited() {
        let root = temp_root("detached-stale");
        fs::create_dir_all(&root).unwrap();
        let cwd = root.display().to_string();
        let task_id = generated_job_id();
        let record_dir = shell_job_record_dir(&cwd, &task_id);
        fs::create_dir_all(&record_dir).unwrap();
        fs::write(record_dir.join("stdout.log"), b"stale output\n").unwrap();
        fs::write(record_dir.join("stderr.log"), b"").unwrap();
        let record = DurableShellJobRecord {
            id: task_id.clone(),
            command: "sleep 30".to_string(),
            cwd: cwd.clone(),
            tty: false,
            pty_backend: None,
            tty_size: None,
            status: "running".to_string(),
            exit_code: None,
            pid: 9_999_999,
            owner_pid: Some(999_998),
            process_group: Some(9_999_999),
            supervisor_pid: None,
            supervisor_socket: None,
            supervisor_epoch: None,
            terminal_event_log: None,
            terminal_event_seq: None,
            control_token_hash: None,
            attachable: false,
            resizable: false,
            stdin_path: None,
            stdin_keeper_pid: None,
            stdin_closed: true,
            started_at: epoch_label(),
            updated_at: epoch_label(),
            stdout_total_bytes: 0,
            stderr_total_bytes: 0,
        };
        write_durable_shell_job_manifest(&cwd, &record).unwrap();

        let shown = ExecShellShowTool
            .execute(
                ToolInput::new()
                    .with_arg("task_id", task_id.clone())
                    .with_arg("cwd", cwd.clone()),
            )
            .unwrap();
        assert!(
            shown.summary.contains("status: exited"),
            "{}",
            shown.summary
        );
        assert!(shown.summary.contains("stale output"), "{}", shown.summary);

        let listed = ExecShellListTool
            .execute(ToolInput::new().with_arg("cwd", cwd))
            .unwrap();
        assert!(listed.summary.contains(&task_id), "{}", listed.summary);
        assert!(
            listed.summary.contains("[exited detached]"),
            "{}",
            listed.summary
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn exec_shell_durable_log_helpers_handle_non_utf8_output() {
        let root = temp_root("non-utf8");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("stdout.log"), [0xff, b'o', b'k', b'\n']).unwrap();

        let rendered = read_durable_log(&root, "stdout.log");
        assert!(rendered.contains("ok"), "{rendered:?}");
        assert_eq!(durable_log_bytes(&root, "stdout.log", 0), 4);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn task_shell_start_and_wait_alias_background_shell() {
        let root = temp_root("task-shell");
        fs::create_dir_all(&root).unwrap();
        let cwd = root.display().to_string();
        let started = TaskShellStartTool
            .execute(
                ToolInput::new()
                    .with_arg("command", "echo task-ready")
                    .with_arg("cwd", cwd.clone()),
            )
            .unwrap();
        assert!(started.summary.contains("Poll with task_shell_wait"));
        assert!(started.summary.contains("meta.task_shell=true"));
        let task_id = task_id_from(&started.summary);
        let waited = TaskShellWaitTool
            .execute(
                ToolInput::new()
                    .with_arg("task_id", task_id)
                    .with_arg("cwd", cwd)
                    .with_arg("timeout_ms", "1000")
                    .with_arg("gate", "test")
                    .with_arg("command", "echo task-ready"),
            )
            .unwrap();
        assert!(waited.summary.contains("meta.gate=test"));
        assert!(waited.summary.contains("meta.command=echo task-ready"));
        assert!(waited.summary.contains("status: completed"));
        assert!(waited.summary.contains("task-ready"));
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn task_shell_start_tty_uses_script_pty_backend() {
        if !script_pty_backend_available() {
            return;
        }
        let root = temp_root("task-shell-tty");
        fs::create_dir_all(&root).unwrap();
        let cwd = root.display().to_string();

        let task_id = shell_manager()
            .lock()
            .unwrap()
            .spawn(
                "test -t 1 && echo tty-ready || echo not-tty",
                &cwd,
                None,
                ShellTtyOptions {
                    enabled: true,
                    size: None,
                },
            )
            .unwrap();
        let waited = ExecShellWaitTool {
            tool_name: "task_shell_wait",
        }
        .execute(
            ToolInput::new()
                .with_arg("task_id", task_id.clone())
                .with_arg("cwd", cwd.clone())
                .with_arg("timeout_ms", "1000"),
        )
        .unwrap();
        assert!(waited.summary.contains("tty: true"), "{}", waited.summary);
        assert!(
            waited.summary.contains("pty_backend: script"),
            "{}",
            waited.summary
        );
        let stdout_delta = waited
            .summary
            .split("stdout_delta:\n")
            .nth(1)
            .unwrap_or_default();
        assert!(
            stdout_delta.lines().any(|line| line.trim() == "tty-ready"),
            "{}",
            waited.summary
        );
        assert!(
            !stdout_delta.lines().any(|line| line.trim() == "not-tty"),
            "{}",
            waited.summary
        );

        let manifest = fs::read_to_string(
            root.join(".dscode/shell-jobs")
                .join(&task_id)
                .join("manifest.json"),
        )
        .unwrap();
        assert!(manifest.contains(r#""tty":true"#), "{manifest}");
        assert!(manifest.contains(r#""pty_backend":"script""#), "{manifest}");

        let started = TaskShellStartTool
            .execute(
                ToolInput::new()
                    .with_arg("command", "echo task-tty")
                    .with_arg("cwd", cwd.clone())
                    .with_arg("tty", "true"),
            )
            .unwrap();
        assert!(started.summary.contains("tty: true"), "{}", started.summary);
        assert!(
            started.summary.contains("meta.tty=true"),
            "{}",
            started.summary
        );
        let started_id = task_id_from(&started.summary);
        let _ = TaskShellWaitTool.execute(
            ToolInput::new()
                .with_arg("task_id", started_id)
                .with_arg("cwd", cwd.clone())
                .with_arg("timeout_ms", "1000"),
        );
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn task_shell_start_tty_size_sets_script_geometry() {
        if !script_pty_backend_available() {
            return;
        }
        let root = temp_root("task-shell-tty-size");
        fs::create_dir_all(&root).unwrap();
        let cwd = root.display().to_string();

        let geometry_task_id = shell_manager()
            .lock()
            .unwrap()
            .spawn(
                "stty size",
                &cwd,
                None,
                ShellTtyOptions {
                    enabled: true,
                    size: Some(ShellTtySize {
                        rows: 33,
                        cols: 111,
                    }),
                },
            )
            .unwrap();
        let geometry_waited = TaskShellWaitTool
            .execute(
                ToolInput::new()
                    .with_arg("task_id", geometry_task_id.clone())
                    .with_arg("cwd", cwd.clone())
                    .with_arg("timeout_ms", "1000"),
            )
            .unwrap();
        assert!(
            geometry_waited.summary.contains("tty: true"),
            "{}",
            geometry_waited.summary
        );
        assert!(
            geometry_waited.summary.contains("tty_rows: 33"),
            "{}",
            geometry_waited.summary
        );
        assert!(
            geometry_waited.summary.contains("tty_cols: 111"),
            "{}",
            geometry_waited.summary
        );
        assert!(
            geometry_waited.summary.contains("33 111"),
            "{}",
            geometry_waited.summary
        );

        let manifest = fs::read_to_string(
            root.join(".dscode/shell-jobs")
                .join(&geometry_task_id)
                .join("manifest.json"),
        )
        .unwrap();
        assert!(manifest.contains(r#""tty_rows":33"#), "{manifest}");
        assert!(manifest.contains(r#""tty_cols":111"#), "{manifest}");

        let started = TaskShellStartTool
            .execute(
                ToolInput::new()
                    .with_arg("command", "echo task-tty-size")
                    .with_arg("cwd", cwd.clone())
                    .with_arg("tty", "true")
                    .with_arg("tty_rows", "33")
                    .with_arg("tty_cols", "111"),
            )
            .unwrap();
        assert!(
            started.summary.contains("tty_rows: 33"),
            "{}",
            started.summary
        );
        assert!(
            started.summary.contains("tty_cols: 111"),
            "{}",
            started.summary
        );
        assert!(
            started.summary.contains("meta.tty=true"),
            "{}",
            started.summary
        );
        let started_id = task_id_from(&started.summary);
        let _ = TaskShellWaitTool.execute(
            ToolInput::new()
                .with_arg("task_id", started_id)
                .with_arg("cwd", cwd.clone())
                .with_arg("timeout_ms", "1000"),
        );
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn exec_shell_resize_updates_running_tty_geometry() {
        if !script_pty_backend_available() {
            return;
        }
        let root = temp_root("resize-tty");
        fs::create_dir_all(&root).unwrap();
        let cwd = root.display().to_string();

        let started = ExecShellTool
            .execute(
                ToolInput::new()
                    .with_arg("command", "tail -f /dev/null")
                    .with_arg("background", "true")
                    .with_arg("cwd", cwd.clone())
                    .with_arg("tty", "true")
                    .with_arg("tty_rows", "24")
                    .with_arg("tty_cols", "80"),
            )
            .unwrap();
        let task_id = task_id_from(&started.summary);

        let resized = ExecShellResizeTool
            .execute(
                ToolInput::new()
                    .with_arg("task_id", task_id.clone())
                    .with_arg("cwd", cwd.clone())
                    .with_arg("tty_rows", "40")
                    .with_arg("tty_cols", "120"),
            )
            .unwrap();
        assert!(
            resized.summary.contains("meta.live_resize=stdin_stty"),
            "{}",
            resized.summary
        );
        assert!(
            resized.summary.contains("tty_rows: 40"),
            "{}",
            resized.summary
        );
        assert!(
            resized.summary.contains("tty_cols: 120"),
            "{}",
            resized.summary
        );

        let manifest = fs::read_to_string(
            root.join(".dscode/shell-jobs")
                .join(&task_id)
                .join("manifest.json"),
        )
        .unwrap();
        assert!(manifest.contains(r#""tty_rows":40"#), "{manifest}");
        assert!(manifest.contains(r#""tty_cols":120"#), "{manifest}");

        let _ = ExecShellCancelTool.execute(ToolInput::new().with_arg("task_id", task_id));
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn exec_shell_resize_updates_detached_tty_geometry() {
        if !script_pty_backend_available() {
            return;
        }
        let root = temp_root("resize-detached-tty");
        fs::create_dir_all(&root).unwrap();
        let cwd = root.display().to_string();

        let started = ExecShellTool
            .execute(
                ToolInput::new()
                    .with_arg("command", "tail -f /dev/null")
                    .with_arg("background", "true")
                    .with_arg("cwd", cwd.clone())
                    .with_arg("tty", "true")
                    .with_arg("tty_rows", "24")
                    .with_arg("tty_cols", "80"),
            )
            .unwrap();
        let task_id = task_id_from(&started.summary);
        shell_manager().lock().unwrap().jobs.remove(&task_id);

        let resized = ExecShellResizeTool
            .execute(
                ToolInput::new()
                    .with_arg("task_id", task_id.clone())
                    .with_arg("cwd", cwd.clone())
                    .with_arg("rows", "41")
                    .with_arg("cols", "121"),
            )
            .unwrap();
        assert!(
            resized.summary.contains("meta.detached_resize=true"),
            "{}",
            resized.summary
        );
        assert!(
            resized
                .summary
                .contains("meta.live_resize=detached_fifo_stty"),
            "{}",
            resized.summary
        );
        assert!(
            resized.summary.contains("tty_rows: 41"),
            "{}",
            resized.summary
        );
        assert!(
            resized.summary.contains("tty_cols: 121"),
            "{}",
            resized.summary
        );

        let manifest = fs::read_to_string(
            root.join(".dscode/shell-jobs")
                .join(&task_id)
                .join("manifest.json"),
        )
        .unwrap();
        assert!(manifest.contains(r#""tty_rows":41"#), "{manifest}");
        assert!(manifest.contains(r#""tty_cols":121"#), "{manifest}");

        let _ = ExecShellCancelTool.execute(
            ToolInput::new()
                .with_arg("task_id", task_id)
                .with_arg("cwd", cwd.clone()),
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn exec_shell_interact_sends_stdin_and_closes_it() {
        let root = temp_root("interact");
        fs::create_dir_all(&root).unwrap();
        let cwd = root.display().to_string();
        let started = ExecShellTool
            .execute(
                ToolInput::new()
                    .with_arg("command", "cat -")
                    .with_arg("background", "true")
                    .with_arg("cwd", cwd),
            )
            .unwrap();
        let task_id = task_id_from(&started.summary);
        let interacted = ExecShellInteractTool {
            tool_name: "exec_shell_interact",
        }
        .execute(
            ToolInput::new()
                .with_arg("task_id", task_id)
                .with_arg("input", "hello stdin\n")
                .with_arg("close_stdin", "true")
                .with_arg("timeout_ms", "1000"),
        )
        .unwrap();
        assert!(interacted.summary.contains("status: completed"));
        assert!(interacted.summary.contains("hello stdin"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn exec_shell_cancel_kills_running_job() {
        let root = temp_root("cancel");
        fs::create_dir_all(&root).unwrap();
        let cwd = root.display().to_string();
        let started = ExecShellTool
            .execute(
                ToolInput::new()
                    .with_arg("command", "tail -f /dev/null")
                    .with_arg("background", "true")
                    .with_arg("cwd", cwd),
            )
            .unwrap();
        let task_id = task_id_from(&started.summary);
        let cancelled = ExecShellCancelTool
            .execute(ToolInput::new().with_arg("task_id", task_id))
            .unwrap();
        assert!(cancelled.summary.contains("Canceled background shell job"));
        let _ = fs::remove_dir_all(root);
    }
}
