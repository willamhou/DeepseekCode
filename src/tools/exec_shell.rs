use crate::error::{app_error, AppResult};
use crate::tools::run_shell::{is_safe_shell_command, RunShellTool};
use crate::tools::types::{Tool, ToolInput, ToolOutput};
use crate::util::json::{
    json_as_string, json_as_u64, json_value_to_string, parse_root_object, JsonValue,
};
use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const DEFAULT_WAIT_MS: u64 = 5_000;
const MAX_TIMEOUT_MS: u64 = 600_000;

static JOB_COUNTER: AtomicU64 = AtomicU64::new(0);
static SHELL_JOBS: OnceLock<Mutex<BackgroundShellManager>> = OnceLock::new();

pub struct ExecShellTool;

pub struct ExecShellWaitTool {
    pub tool_name: &'static str,
}

pub struct ExecShellListTool;

pub struct ExecShellShowTool;

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
    let task_id = shell_manager().lock().unwrap().spawn(command, cwd, None)?;
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
        let background = truthy(input.get("background"));
        if !background {
            let mut shell_input = ToolInput::new().with_arg("command", command.to_string());
            if let Some(cwd) = input.get("cwd") {
                shell_input = shell_input.with_arg("cwd", cwd.to_string());
            }
            return RunShellTool.execute(shell_input);
        }

        let cwd = input.get("cwd").unwrap_or(".");
        let stdin = input
            .get("stdin")
            .or_else(|| input.get("input"))
            .or_else(|| input.get("data"));
        let task_id = shell_manager().lock().unwrap().spawn(command, cwd, stdin)?;
        Ok(ToolOutput {
            summary: format!(
                "task_id: {task_id}\nstatus: running\ncommand: {command}\ncwd: {cwd}\nPoll with exec_shell_wait task_id={task_id} or cancel with exec_shell_cancel task_id={task_id}."
            ),
        })
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
        let mut output = ExecShellTool.execute(shell_input)?;
        output.summary = output
            .summary
            .replace("Poll with exec_shell_wait", "Poll with task_shell_wait");
        output.summary.push_str("\nmeta.task_shell=true");
        if input.get("tty").is_some() {
            output.summary.push_str("\nmeta.tty_compat=accepted");
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

impl Tool for ExecShellInteractTool {
    fn name(&self) -> &str {
        self.tool_name
    }

    fn execute(&self, input: ToolInput) -> AppResult<ToolOutput> {
        let task_id = required_task_id(&input)?;
        let data = input
            .get("input")
            .or_else(|| input.get("stdin"))
            .or_else(|| input.get("data"))
            .unwrap_or("");
        let close_stdin = truthy(input.get("close_stdin"));
        let timeout_ms = input_u64(&input, "timeout_ms", 1_000).min(MAX_TIMEOUT_MS);
        {
            let mut manager = shell_manager().lock().unwrap();
            manager.write_stdin(task_id, data, close_stdin)?;
        }
        ExecShellWaitTool {
            tool_name: self.tool_name,
        }
        .execute(
            ToolInput::new()
                .with_arg("task_id", task_id.to_string())
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
            let cancelled = shell_manager().lock().unwrap().cancel_all()?;
            return Ok(ToolOutput {
                summary: if cancelled.is_empty() {
                    "No running background shell jobs.".to_string()
                } else {
                    format!(
                        "Canceled {} background shell job{}: {}",
                        cancelled.len(),
                        if cancelled.len() == 1 { "" } else { "s" },
                        cancelled.join(", ")
                    )
                },
            });
        }
        let task_id = required_task_id(&input)?;
        shell_manager().lock().unwrap().cancel(task_id)?;
        Ok(ToolOutput {
            summary: format!("Canceled background shell job: {task_id}"),
        })
    }
}

struct BackgroundShellManager {
    jobs: BTreeMap<String, BackgroundShellJob>,
}

struct BackgroundShellJob {
    id: String,
    command: String,
    cwd: String,
    child: Option<Child>,
    stdin: Option<ChildStdin>,
    stdout: Arc<Mutex<Vec<u8>>>,
    stderr: Arc<Mutex<Vec<u8>>>,
    stdout_cursor: usize,
    stderr_cursor: usize,
    stdout_reader: Option<thread::JoinHandle<std::io::Result<()>>>,
    stderr_reader: Option<thread::JoinHandle<std::io::Result<()>>>,
    status: ShellJobStatus,
    exit_code: Option<i32>,
    started_at: String,
    updated_at: String,
    record_dir: PathBuf,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ShellJobStatus {
    Running,
    Completed,
    Failed,
    Killed,
}

impl BackgroundShellManager {
    fn spawn(&mut self, command: &str, cwd: &str, stdin_data: Option<&str>) -> AppResult<String> {
        let id = generated_job_id();
        let record_dir = shell_job_record_dir(cwd, &id);
        fs::create_dir_all(&record_dir)?;
        let mut process = Command::new("sh");
        process
            .args(["-lc", command])
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        configure_process_group(&mut process);
        let mut child = process.spawn()?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| app_error("exec_shell child produced no stdout pipe"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| app_error("exec_shell child produced no stderr pipe"))?;
        let stdout_buffer = Arc::new(Mutex::new(Vec::new()));
        let stderr_buffer = Arc::new(Mutex::new(Vec::new()));
        let stdout_reader = spawn_reader(
            stdout,
            stdout_buffer.clone(),
            Some(record_dir.join("stdout.log")),
        );
        let stderr_reader = spawn_reader(
            stderr,
            stderr_buffer.clone(),
            Some(record_dir.join("stderr.log")),
        );
        let mut stdin = child.stdin.take();
        if let Some(data) = stdin_data {
            if let Some(handle) = stdin.as_mut() {
                handle.write_all(data.as_bytes())?;
                handle.flush()?;
            }
        }

        let now = epoch_label();
        self.jobs.insert(
            id.clone(),
            BackgroundShellJob {
                id: id.clone(),
                command: command.to_string(),
                cwd: cwd.to_string(),
                child: Some(child),
                stdin,
                stdout: stdout_buffer,
                stderr: stderr_buffer,
                stdout_cursor: 0,
                stderr_cursor: 0,
                stdout_reader: Some(stdout_reader),
                stderr_reader: Some(stderr_reader),
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
            job.stdin = None;
            join_reader(job.stdout_reader.take(), "stdout")?;
            join_reader(job.stderr_reader.take(), "stderr")?;
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
            let stdout_total = job.stdout.lock().unwrap().len();
            let stderr_total = job.stderr.lock().unwrap().len();
            lines.push(format!(
                "- {} [{}] exit={} stdout={} stderr={} cwd={}",
                job.id,
                job.status.as_str(),
                job.exit_code
                    .map(|code| code.to_string())
                    .unwrap_or_else(|| "null".to_string()),
                stdout_total,
                stderr_total,
                job.cwd
            ));
            lines.push(format!("  command: {}", job.command));
        }
        for record in durable {
            if self.jobs.contains_key(&record.id) {
                continue;
            }
            lines.push(format!(
                "- {} [{} detached] exit={} stdout={} stderr={} cwd={}",
                record.id,
                record.status,
                record
                    .exit_code
                    .map(|code| code.to_string())
                    .unwrap_or_else(|| "null".to_string()),
                record.stdout_total_bytes,
                record.stderr_total_bytes,
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
        let stdout_delta = read_delta(&job.stdout, &mut job.stdout_cursor)?;
        let stderr_delta = read_delta(&job.stderr, &mut job.stderr_cursor)?;
        let stdout_total = job.stdout.lock().unwrap().len();
        let stderr_total = job.stderr.lock().unwrap().len();
        let mut out = format!(
            "task_id: {}\nstatus: {}\nexit_code: {}\ncommand: {}\ncwd: {}\nstdout_total_bytes: {stdout_total}\nstderr_total_bytes: {stderr_total}\n",
            job.id,
            job.status.as_str(),
            job.exit_code
                .map(|code| code.to_string())
                .unwrap_or_else(|| "null".to_string()),
            job.command,
            job.cwd
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
        let stdout = String::from_utf8_lossy(&job.stdout.lock().unwrap()).to_string();
        let stderr = String::from_utf8_lossy(&job.stderr.lock().unwrap()).to_string();
        let stdout_total = stdout.len();
        let stderr_total = stderr.len();
        let mut out = format!(
            "task_id: {}\nstatus: {}\nexit_code: {}\ncommand: {}\ncwd: {}\nstdout_total_bytes: {stdout_total}\nstderr_total_bytes: {stderr_total}\n",
            job.id,
            job.status.as_str(),
            job.exit_code
                .map(|code| code.to_string())
                .unwrap_or_else(|| "null".to_string()),
            job.command,
            job.cwd
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
        let Some(stdin) = job.stdin.as_mut() else {
            return Err(app_error(format!(
                "stdin is not available for background shell task {task_id}"
            )));
        };
        if !data.is_empty() {
            stdin.write_all(data.as_bytes())?;
            stdin.flush()?;
        }
        if close_stdin {
            job.stdin = None;
        }
        job.updated_at = epoch_label();
        persist_job_snapshot(job)?;
        Ok(())
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
        job.stdin = None;
        job.status = ShellJobStatus::Killed;
        join_reader(job.stdout_reader.take(), "stdout")?;
        join_reader(job.stderr_reader.take(), "stderr")?;
        job.updated_at = epoch_label();
        persist_job_snapshot(job)?;
        Ok(())
    }

    fn cancel_all(&mut self) -> AppResult<Vec<String>> {
        let ids = self
            .jobs
            .iter()
            .filter_map(|(id, job)| {
                if job.status == ShellJobStatus::Running {
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

fn required_task_id(input: &ToolInput) -> AppResult<&str> {
    input
        .get("task_id")
        .or_else(|| input.get("id"))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| app_error("background shell task_id is required"))
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
    status: String,
    exit_code: Option<i32>,
    pid: u32,
    started_at: String,
    updated_at: String,
    stdout_total_bytes: usize,
    stderr_total_bytes: usize,
}

fn shell_job_record_dir(cwd: &str, task_id: &str) -> PathBuf {
    Path::new(cwd).join(".dscode/shell-jobs").join(task_id)
}

fn shell_job_manifest_path(cwd: &str, task_id: &str) -> PathBuf {
    shell_job_record_dir(cwd, task_id).join("manifest.json")
}

fn persist_job_snapshot(job: &BackgroundShellJob) -> AppResult<()> {
    fs::create_dir_all(&job.record_dir)?;
    let stdout_total = job.stdout.lock().unwrap().len();
    let stderr_total = job.stderr.lock().unwrap().len();
    let exit_code = job
        .exit_code
        .map(|code| JsonValue::Number(code.to_string()))
        .unwrap_or(JsonValue::Null);
    let pid = job
        .child
        .as_ref()
        .map(Child::id)
        .unwrap_or_else(|| parse_pid_from_task_id(&job.id).unwrap_or(0));
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
        (
            "status".to_string(),
            JsonValue::String(job.status.as_str().to_string()),
        ),
        ("exit_code".to_string(), exit_code),
        ("pid".to_string(), JsonValue::Number(pid.to_string())),
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

fn render_durable_snapshot(cwd: &str, task_id: &str) -> AppResult<String> {
    let manifest = shell_job_manifest_path(cwd, task_id);
    if !manifest.is_file() {
        return Err(app_error(format!(
            "unknown background shell task: {task_id}"
        )));
    }
    let record = read_durable_shell_job_manifest(&manifest)?;
    let record_dir = shell_job_record_dir(cwd, task_id);
    let stdout = read_durable_log(&record_dir, "stdout.log");
    let stderr = read_durable_log(&record_dir, "stderr.log");
    let mut out = format!(
        "task_id: {}\nstatus: {}\nmanaged: false\nexit_code: {}\npid: {}\ncommand: {}\ncwd: {}\nstarted_at: {}\nupdated_at: {}\nstdout_total_bytes: {}\nstderr_total_bytes: {}\nnote: durable metadata is available, but this process is not attached to the shell job for stdin/cancel control.\n",
        record.id,
        record.status,
        record
            .exit_code
            .map(|code| code.to_string())
            .unwrap_or_else(|| "null".to_string()),
        record.pid,
        record.command,
        record.cwd,
        record.started_at,
        record.updated_at,
        record.stdout_total_bytes,
        record.stderr_total_bytes
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
    Ok(DurableShellJobRecord {
        id: required_manifest_string(&root, "id")?,
        command: required_manifest_string(&root, "command")?,
        cwd: required_manifest_string(&root, "cwd")?,
        status: required_manifest_string(&root, "status")?,
        exit_code: match root.get("exit_code") {
            Some(JsonValue::Null) | None => None,
            Some(value) => json_as_u64(value).map(|value| value as i32),
        },
        pid: root.get("pid").and_then(json_as_u64).unwrap_or(0) as u32,
        started_at: required_manifest_string(&root, "started_at")?,
        updated_at: required_manifest_string(&root, "updated_at")?,
        stdout_total_bytes: durable_log_bytes(record_dir, "stdout.log", manifest_stdout_total),
        stderr_total_bytes: durable_log_bytes(record_dir, "stderr.log", manifest_stderr_total),
    })
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

fn read_durable_log(record_dir: &Path, name: &str) -> String {
    fs::read(record_dir.join(name))
        .map(|bytes| String::from_utf8_lossy(&bytes).to_string())
        .unwrap_or_default()
}

fn parse_pid_from_task_id(task_id: &str) -> Option<u32> {
    task_id
        .strip_prefix("shell-")?
        .split('-')
        .next()?
        .parse::<u32>()
        .ok()
}

fn spawn_reader<R: Read + Send + 'static>(
    mut reader: R,
    buffer: Arc<Mutex<Vec<u8>>>,
    log_path: Option<PathBuf>,
) -> thread::JoinHandle<std::io::Result<()>> {
    thread::spawn(move || {
        let mut log_file = match log_path {
            Some(path) => Some(OpenOptions::new().create(true).append(true).open(path)?),
            None => None,
        };
        let mut chunk = [0u8; 4096];
        loop {
            let read = reader.read(&mut chunk)?;
            if read == 0 {
                return Ok(());
            }
            buffer.lock().unwrap().extend_from_slice(&chunk[..read]);
            if let Some(file) = log_file.as_mut() {
                file.write_all(&chunk[..read])?;
                file.flush()?;
            }
        }
    })
}

fn join_reader(
    handle: Option<thread::JoinHandle<std::io::Result<()>>>,
    stream_name: &str,
) -> AppResult<()> {
    let Some(handle) = handle else {
        return Ok(());
    };
    match handle.join() {
        Ok(Ok(())) => Ok(()),
        Ok(Err(error)) => Err(app_error(format!(
            "failed to read exec_shell {stream_name}: {error}"
        ))),
        Err(_) => Err(app_error(format!(
            "exec_shell {stream_name} reader panicked"
        ))),
    }
}

fn read_delta(buffer: &Arc<Mutex<Vec<u8>>>, cursor: &mut usize) -> AppResult<String> {
    let guard = buffer.lock().unwrap();
    let start = (*cursor).min(guard.len());
    let delta = String::from_utf8_lossy(&guard[start..]).to_string();
    *cursor = guard.len();
    Ok(delta)
}

#[cfg(unix)]
fn configure_process_group(process: &mut Command) {
    use std::os::unix::process::CommandExt;
    process.process_group(0);
}

#[cfg(not(unix))]
fn configure_process_group(_process: &mut Command) {}

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
                .with_arg("task_id", task_id)
                .with_arg("cwd", cwd)
                .with_arg("timeout_ms", "1000"),
        )
        .unwrap();
        assert!(
            waited.summary.contains("managed: false"),
            "{}",
            waited.summary
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
