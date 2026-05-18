use std::env;
use std::io::{self, IsTerminal};

#[derive(Debug)]
pub struct Cli {
    pub command: Option<Command>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TerminalContext {
    stdin_tty: bool,
    stdout_tty: bool,
}

impl TerminalContext {
    fn current() -> Self {
        let context = Self {
            stdin_tty: io::stdin().is_terminal(),
            stdout_tty: io::stdout().is_terminal(),
        };
        #[cfg(windows)]
        {
            if !context.supports_full_screen() && windows_console_devices_available() {
                return Self {
                    stdin_tty: true,
                    stdout_tty: true,
                };
            }
        }
        context
    }

    fn non_interactive() -> Self {
        Self {
            stdin_tty: false,
            stdout_tty: false,
        }
    }

    fn supports_full_screen(self) -> bool {
        self.stdin_tty && self.stdout_tty
    }
}

#[cfg(windows)]
fn windows_console_devices_available() -> bool {
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("CONIN$")
        .is_ok()
        && std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open("CONOUT$")
            .is_ok()
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct HelpArgs {
    pub topics: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompletionShell {
    Bash,
    Zsh,
    Fish,
}

#[derive(Debug)]
pub enum PrAction {
    Review {
        reference: String,
        post: bool,
        out: Option<String>,
    },
    LiveStatus {
        reference: String,
        require_write: bool,
        json: bool,
    },
    Fix {
        reference: String,
        job: Option<String>,
        benchmark_gate: bool,
    },
    Patch {
        reference: String,
        commit: bool,
        benchmark_gate: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DogfoodOutcome {
    Success,
    Failed,
    Stuck,
    Manual,
}

#[derive(Debug)]
pub enum DogfoodAction {
    Run(DogfoodRunArgs),
    ExternalFixture(DogfoodExternalFixtureArgs),
    ReplayBenchmark(DogfoodReplayArgs),
    LivePlan(DogfoodLivePlanArgs),
    LiveRun(DogfoodLiveRunArgs),
    Report(DogfoodReportArgs),
    ExportBenchmark(DogfoodExportArgs),
    PromoteBenchmark(DogfoodPromoteArgs),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RestoreAction {
    Snapshot { label: Option<String> },
    List { limit: usize },
    Show { id: String, patch: bool },
    RevertTurn { id: String, apply: bool },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpAction {
    List,
    Doctor,
    Tools {
        server: Option<String>,
    },
    Prompts {
        server: Option<String>,
    },
    Resources {
        server: Option<String>,
    },
    ResourceTemplates {
        server: Option<String>,
    },
    Call {
        server: String,
        tool: String,
        arguments_json: Option<String>,
    },
    Prompt {
        server: String,
        prompt: String,
        arguments_json: Option<String>,
    },
    Resource {
        server: String,
        uri: String,
    },
    Add {
        name: String,
        command: Option<String>,
        args: Vec<String>,
        url: Option<String>,
        transport: Option<String>,
        env: Vec<(String, String)>,
        headers: Vec<(String, String)>,
        disabled: bool,
        scope: McpConfigScope,
    },
    Get {
        name: String,
    },
    Remove {
        name: String,
        scope: McpConfigScope,
    },
    Enable {
        name: String,
        scope: McpConfigScope,
    },
    Disable {
        name: String,
        scope: McpConfigScope,
    },
    Validate,
    Init {
        force: bool,
    },
    AddSelf {
        name: String,
        workspace: Option<String>,
        scope: McpConfigScope,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpConfigScope {
    User,
    Project,
}

#[derive(Debug)]
pub struct DogfoodRunArgs {
    pub task: String,
    pub from_benchmark: Option<String>,
    pub benchmark_manifest: Option<String>,
    pub skill: Option<String>,
    pub budget: Option<usize>,
    pub workdir: Option<String>,
    pub isolate_workdir: bool,
    pub outcome: Option<DogfoodOutcome>,
    pub manual_intervention: bool,
    pub benchmark_gate: bool,
    pub notes: Option<String>,
}

#[derive(Debug)]
pub struct DogfoodExternalFixtureArgs {
    pub task: String,
    pub workdir: String,
    pub budget: Option<usize>,
    pub benchmark_gate: bool,
    pub notes: Option<String>,
    pub dry_run: bool,
}

#[derive(Debug, Default)]
pub struct DogfoodReplayArgs {
    pub manifest: Option<String>,
    pub category: Option<String>,
    pub limit: Option<usize>,
    pub benchmark_gate: bool,
}

#[derive(Debug, Default)]
pub struct DogfoodLivePlanArgs {
    pub manifest: Option<String>,
    pub target_live_runs: Option<usize>,
    pub target_live_success_rate: Option<f64>,
    pub target_categories: Vec<DogfoodCategoryRequirement>,
    pub limit: Option<usize>,
    pub json: bool,
}

#[derive(Debug, Default)]
pub struct DogfoodLiveRunArgs {
    pub manifest: Option<String>,
    pub target_live_runs: Option<usize>,
    pub target_live_success_rate: Option<f64>,
    pub target_categories: Vec<DogfoodCategoryRequirement>,
    pub categories: Vec<String>,
    pub limit: Option<usize>,
    pub execute: bool,
    pub benchmark_gate: bool,
}

#[derive(Debug, Default)]
pub struct DogfoodReportArgs {
    pub out: Option<String>,
    pub limit: Option<usize>,
    pub require_min_runs: Option<usize>,
    pub require_success_rate: Option<f64>,
    pub require_live_runs: Option<usize>,
    pub require_live_success_rate: Option<f64>,
    pub require_external_write_fixtures: Option<usize>,
    pub require_recent_clean: Option<usize>,
    pub require_categories: Vec<DogfoodCategoryRequirement>,
    pub require_live_categories: Vec<DogfoodCategoryRequirement>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DogfoodCategoryRequirement {
    pub category: String,
    pub min_runs: usize,
    pub min_success_percent: f64,
}

#[derive(Debug, Default)]
pub struct DogfoodExportArgs {
    pub out: Option<String>,
    pub limit: Option<usize>,
    pub outcome: Option<DogfoodOutcome>,
}

#[derive(Debug, Default)]
pub struct DogfoodPromoteArgs {
    pub manifest: Option<String>,
    pub limit: Option<usize>,
    pub outcome: Option<DogfoodOutcome>,
    pub dry_run: bool,
}

pub fn parse_pr_subcommand(args: Vec<String>) -> Result<PrAction, String> {
    let mut iter = args.into_iter();
    let action = iter
        .next()
        .ok_or_else(|| "pr requires a sub-action: review|live-status|fix|patch".to_string())?;
    let reference = iter
        .next()
        .ok_or_else(|| format!("pr {action} requires a PR reference"))?;
    let rest: Vec<String> = iter.collect();

    match action.as_str() {
        "review" => {
            let mut post = false;
            let mut out = None;
            let mut index = 0;
            while index < rest.len() {
                match rest[index].as_str() {
                    "--post" => {
                        post = true;
                        index += 1;
                    }
                    "--out" if index + 1 < rest.len() => {
                        out = Some(rest[index + 1].clone());
                        index += 2;
                    }
                    other => {
                        return Err(format!("unknown flag for `pr review`: {other}"));
                    }
                }
            }
            Ok(PrAction::Review {
                reference,
                post,
                out,
            })
        }
        "live-status" => {
            let mut require_write = false;
            let mut json = false;
            let mut index = 0;
            while index < rest.len() {
                match rest[index].as_str() {
                    "--require-write" => {
                        require_write = true;
                        index += 1;
                    }
                    "--json" => {
                        json = true;
                        index += 1;
                    }
                    other => {
                        return Err(format!(
                            "unknown flag for `pr live-status`: {other}; expected --require-write|--json"
                        ));
                    }
                }
            }
            Ok(PrAction::LiveStatus {
                reference,
                require_write,
                json,
            })
        }
        "fix" => {
            let mut job = None;
            let mut benchmark_gate = false;
            let mut index = 0;
            while index < rest.len() {
                match rest[index].as_str() {
                    "--job" if index + 1 < rest.len() => {
                        job = Some(rest[index + 1].clone());
                        index += 2;
                    }
                    "--benchmark-gate" => {
                        benchmark_gate = true;
                        index += 1;
                    }
                    other => {
                        return Err(format!("unknown flag for `pr fix`: {other}"));
                    }
                }
            }
            Ok(PrAction::Fix {
                reference,
                job,
                benchmark_gate,
            })
        }
        "patch" => {
            let mut commit = false;
            let mut benchmark_gate = false;
            let mut index = 0;
            while index < rest.len() {
                match rest[index].as_str() {
                    "--commit" => {
                        commit = true;
                        index += 1;
                    }
                    "--benchmark-gate" => {
                        benchmark_gate = true;
                        index += 1;
                    }
                    other => {
                        return Err(format!("unknown flag for `pr patch`: {other}"));
                    }
                }
            }
            Ok(PrAction::Patch {
                reference,
                commit,
                benchmark_gate,
            })
        }
        other => Err(format!(
            "unknown pr sub-action `{other}`; expected review|live-status|fix|patch"
        )),
    }
}

impl Cli {
    pub fn parse() -> Result<Self, String> {
        let argv = env::args().skip(1).collect::<Vec<_>>();
        Self::from_argv_with_terminal(argv, TerminalContext::current())
    }

    pub fn from_argv(args: Vec<String>) -> Result<Self, String> {
        Self::from_argv_with_terminal(args, TerminalContext::non_interactive())
    }

    fn from_argv_with_terminal(
        mut args: Vec<String>,
        terminal: TerminalContext,
    ) -> Result<Self, String> {
        if args.is_empty() {
            let command = if terminal.supports_full_screen() {
                Command::Tui(TuiArgs::default())
            } else {
                Command::Chat(ChatArgs::default())
            };
            return Ok(Self {
                command: Some(command),
            });
        }

        if args.len() == 1 && matches!(args[0].as_str(), "--version" | "-V") {
            return Ok(Self {
                command: Some(Command::Version),
            });
        }

        let first = args.remove(0);
        if is_help_flag(&first) {
            return Ok(Self {
                command: Some(Command::Help(HelpArgs::default())),
            });
        }
        if first == "help" {
            return Ok(Self {
                command: Some(Command::Help(HelpArgs { topics: args })),
            });
        }
        if args.iter().any(|arg| is_help_flag(arg)) {
            let mut topics = vec![first];
            topics.extend(args.into_iter().filter(|arg| !is_help_flag(arg)));
            return Ok(Self {
                command: Some(Command::Help(HelpArgs { topics })),
            });
        }
        let command = match first.as_str() {
            "version" => Command::Version,
            "completion" => Command::Completion(parse_completion_args(args)?),
            "chat" | "repl" | "interactive" => {
                let (skill, positional) = parse_common_flags(args);
                let task = positional.join(" ");
                let task = if task.is_empty() { None } else { Some(task) };
                Command::Chat(ChatArgs { task, skill })
            }
            "benchmark" => Command::Benchmark(parse_benchmark_args(args)),
            "dogfood" => Command::Dogfood(parse_dogfood_subcommand(args)?),
            "run" => Command::Run(parse_run_args(args)),
            "exec" => Command::Exec(parse_exec_subcommand(args)?),
            "agents" => Command::Agents(parse_agents_subcommand(args)?),
            "diagnostics" | "diag" => Command::Diagnostics(parse_diagnostics_args(args)?),
            "diff" => Command::Diff(DiffArgs {}),
            "resume" => Command::Resume(ResumeArgs { session: None }),
            "restore" => Command::Restore(parse_restore_subcommand(args)?),
            "config" => Command::Config(parse_config_args(args)?),
            "doctor" => Command::Doctor(parse_doctor_args(args)?),
            "serve" => Command::Serve(parse_serve_args(args)?),
            "tui" => Command::Tui(parse_tui_args(args)?),
            "update" => Command::Update(parse_update_args(args)?),
            "smoke" => Command::Smoke(parse_smoke_args(args)),
            "pr" => Command::Pr(parse_pr_subcommand(args)?),
            "mcp" => Command::Mcp(parse_mcp_subcommand(args)?),
            _ => {
                let mut combined = vec![first];
                combined.extend(args);
                let (skill, positional) = parse_common_flags(combined);
                let task = positional.join(" ");
                let task = if task.is_empty() { None } else { Some(task) };
                Command::Chat(ChatArgs { task, skill })
            }
        };

        Ok(Self {
            command: Some(command),
        })
    }
}

#[derive(Debug)]
pub enum Command {
    Benchmark(BenchmarkArgs),
    Dogfood(DogfoodAction),
    Chat(ChatArgs),
    Completion(CompletionShell),
    Run(RunArgs),
    Exec(ExecAction),
    Agents(AgentsAction),
    Diagnostics(DiagnosticsArgs),
    Diff(DiffArgs),
    Resume(ResumeArgs),
    Restore(RestoreAction),
    Config(ConfigArgs),
    Doctor(DoctorArgs),
    Serve(ServeArgs),
    Tui(TuiArgs),
    Update(UpdateArgs),
    Smoke(SmokeArgs),
    Pr(PrAction),
    Mcp(McpAction),
    Help(HelpArgs),
    Version,
}

impl Default for Command {
    fn default() -> Self {
        Self::Chat(ChatArgs::default())
    }
}

fn is_help_flag(value: &str) -> bool {
    matches!(value, "--help" | "-h")
}

fn parse_completion_args(args: Vec<String>) -> Result<CompletionShell, String> {
    let shell = args
        .first()
        .ok_or_else(|| "completion requires a shell: bash|zsh|fish".to_string())?;
    if args.len() > 1 {
        return Err("completion accepts exactly one shell argument".to_string());
    }
    match shell.as_str() {
        "bash" => Ok(CompletionShell::Bash),
        "zsh" => Ok(CompletionShell::Zsh),
        "fish" => Ok(CompletionShell::Fish),
        other => Err(format!(
            "unknown completion shell `{other}`; expected bash|zsh|fish"
        )),
    }
}

#[derive(Debug, Default)]
pub struct ChatArgs {
    #[allow(dead_code)]
    pub task: Option<String>,
    pub skill: Option<String>,
}

#[derive(Debug, Default)]
pub struct BenchmarkArgs {
    pub manifest: Option<String>,
    pub out: Option<String>,
    pub accept_live_baseline: bool,
}

#[derive(Debug)]
pub struct RunArgs {
    pub task: String,
    pub skill: Option<String>,
    pub budget: Option<usize>,
    pub benchmark_gate: bool,
}

#[derive(Debug)]
pub enum ExecAction {
    Run(ExecArgs),
    Resume(ExecResumeArgs),
}

#[derive(Debug)]
pub struct ExecArgs {
    pub task: String,
    pub skill: Option<String>,
    pub budget: Option<usize>,
    pub images: Vec<String>,
    pub json: bool,
}

#[derive(Debug)]
pub struct ExecResumeArgs {
    pub session: Option<String>,
    pub task: Option<String>,
    pub skill: Option<String>,
    pub budget: Option<usize>,
    pub images: Vec<String>,
    pub json: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentsAction {
    List,
    Show {
        name: String,
    },
    Validate {
        path: Option<String>,
    },
    RunTask {
        id: String,
        budget: Option<usize>,
        json: bool,
    },
    Daemon {
        budget: Option<usize>,
        interval_ms: u64,
        once: bool,
        json: bool,
    },
    RlmStatus(AgentsRlmStatusArgs),
    RlmEvents(AgentsRlmEventsArgs),
    RlmWait(AgentsRlmWaitArgs),
    RlmCancel(AgentsRlmCancelArgs),
    RlmRecover(AgentsRlmRecoverArgs),
    RlmStop(AgentsRlmStopArgs),
    RlmRunNext(AgentsRlmRunNextArgs),
    RlmDrain(AgentsRlmDrainArgs),
    Shell(AgentsShellArgs),
    ShellSupervisor(AgentsShellSupervisorArgs),
    Service(AgentsServiceArgs),
    ServiceDoctor(AgentsServiceDoctorArgs),
    ServiceSmoke(AgentsServiceSmokeArgs),
    Threads,
    ShowThread {
        id: String,
    },
    SwitchThread {
        id: String,
    },
    CurrentThread,
    ClearThread,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentsShellArgs {
    pub action: AgentsShellAction,
    pub json: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentsShellAction {
    Status,
    Show,
    Start {
        command: String,
        cwd: Option<String>,
        tty: bool,
        tty_rows: Option<u64>,
        tty_cols: Option<u64>,
    },
    Wait {
        task_id: String,
        timeout_ms: Option<u64>,
    },
    Replay {
        task_id: String,
        stream: Option<String>,
        cursor: Option<u64>,
        offset: Option<u64>,
        limit_bytes: Option<u64>,
        tail: bool,
    },
    Attach {
        task_id: String,
        cursor: Option<u64>,
        wait_ms: Option<u64>,
        limit_bytes: Option<u64>,
        tail: bool,
        follow: bool,
        interactive: bool,
        poll_ms: Option<u64>,
        max_ms: Option<u64>,
    },
    Stdin {
        task_id: String,
        input: Option<String>,
        close_stdin: bool,
        timeout_ms: Option<u64>,
    },
    Resize {
        task_id: String,
        tty_rows: u64,
        tty_cols: u64,
    },
    Cancel {
        task_id: Option<String>,
        all: bool,
    },
    Shutdown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentsShellSupervisorArgs {
    pub once: bool,
    pub json: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentsRlmStatusArgs {
    pub session_id: Option<String>,
    pub limit: Option<usize>,
    pub json: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentsRlmEventsArgs {
    pub session_id: String,
    pub cursor: Option<u64>,
    pub limit: Option<usize>,
    pub json: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentsRlmWaitArgs {
    pub session_id: String,
    pub cursor: Option<u64>,
    pub limit: Option<usize>,
    pub timeout_ms: Option<u64>,
    pub poll_interval_ms: Option<u64>,
    pub json: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentsRlmCancelArgs {
    pub session_id: String,
    pub task_id: Option<String>,
    pub all: bool,
    pub force: bool,
    pub reason: Option<String>,
    pub json: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentsRlmRecoverArgs {
    pub session_id: Option<String>,
    pub all: bool,
    pub mode: Option<String>,
    pub dry_run: bool,
    pub force: bool,
    pub limit: Option<usize>,
    pub reason: Option<String>,
    pub json: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentsRlmStopArgs {
    pub session_id: String,
    pub reason: Option<String>,
    pub json: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentsRlmRunNextArgs {
    pub session_id: String,
    pub task_id: Option<String>,
    pub dry_run: bool,
    pub json: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentsRlmDrainArgs {
    pub session_id: String,
    pub max_turns: Option<usize>,
    pub dry_run: bool,
    pub json: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentsServiceKind {
    Systemd,
    Launchd,
    All,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentsServiceArgs {
    pub kind: AgentsServiceKind,
    pub out: Option<String>,
    pub bin: Option<String>,
    pub workdir: Option<String>,
    pub addr: String,
    pub interval_ms: u64,
    pub budget: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentsServiceDoctorArgs {
    pub kind: AgentsServiceKind,
    pub out: Option<String>,
    pub bin: Option<String>,
    pub workdir: Option<String>,
    pub addr: String,
    pub interval_ms: u64,
    pub budget: Option<usize>,
    pub json: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentsServiceSmokeArgs {
    pub bin: Option<String>,
    pub workdir: Option<String>,
    pub addr: String,
    pub timeout_ms: u64,
    pub json: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiagnosticsArgs {
    pub changed: bool,
    pub watch: bool,
    pub once: bool,
    pub json: bool,
    pub interval_ms: u64,
    pub paths: Vec<String>,
}

#[derive(Debug)]
pub struct DiffArgs {}

#[derive(Debug)]
pub struct ResumeArgs {
    pub session: Option<String>,
}

#[derive(Debug)]
pub struct ConfigArgs {
    pub print_default: bool,
    pub init: bool,
    pub force: bool,
    pub network_allow: Option<String>,
    pub network_deny: Option<String>,
    pub auth_env: Option<String>,
    pub auth_stdin: bool,
}

#[derive(Debug, Default)]
pub struct DoctorArgs {
    pub json: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServeArgs {
    pub action: ServeAction,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServeAction {
    Http(ServeHttpArgs),
    Mcp(ServeMcpArgs),
    Acp(ServeAcpArgs),
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TuiArgs {
    pub demo: bool,
    pub once: bool,
    pub runtime_url: Option<String>,
    pub entrypoint_smoke: bool,
    pub smoke_bin: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServeHttpArgs {
    pub addr: String,
    pub once: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ServeMcpArgs {
    pub workspace: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ServeAcpArgs {
    pub workspace: Option<String>,
}

#[derive(Debug, Default)]
pub struct UpdateArgs {
    pub check: bool,
    pub print_command: bool,
    pub action: UpdateAction,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateAction {
    Status,
    Package(UpdatePackageArgs),
    VerifyInstall(UpdateVerifyInstallArgs),
    InstallPackage(UpdateInstallPackageArgs),
    Rollback(UpdateRollbackArgs),
    HomebrewFormula(UpdateHomebrewFormulaArgs),
    PublishStatus(UpdatePublishStatusArgs),
    DownloadPlan(UpdateDownloadPlanArgs),
}

impl Default for UpdateAction {
    fn default() -> Self {
        Self::Status
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UpdatePackageArgs {
    pub out: Option<String>,
    pub bin: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UpdateVerifyInstallArgs {
    pub bin: Option<String>,
    pub workdir: Option<String>,
    pub keep_workdir: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UpdateInstallPackageArgs {
    pub package: Option<String>,
    pub dest: Option<String>,
    pub backup_dir: Option<String>,
    pub dry_run: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UpdateRollbackArgs {
    pub backup: Option<String>,
    pub dest: Option<String>,
    pub dry_run: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateHomebrewFormulaArgs {
    pub version: String,
    pub repo: String,
    pub dist: String,
    pub formula: String,
    pub out: Option<String>,
}

impl Default for UpdateHomebrewFormulaArgs {
    fn default() -> Self {
        Self {
            version: env!("CARGO_PKG_VERSION").to_string(),
            repo: "willamhou/DeepSeekCode".to_string(),
            dist: "dist".to_string(),
            formula: "packaging/homebrew/deepseek.rb".to_string(),
            out: None,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UpdatePublishStatusArgs {
    pub dist: Option<String>,
    pub npm_dist: Option<String>,
    pub strict: bool,
    pub json: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UpdateDownloadPlanArgs {
    pub version: Option<String>,
    pub repo: Option<String>,
    pub base_url: Option<String>,
    pub platform: Option<String>,
    pub json: bool,
}

#[derive(Debug, Default)]
pub struct SmokeArgs {
    pub flavor: Option<SmokeFlavor>,
    pub prompt: Option<String>,
}

#[derive(Debug, Clone, Copy)]
pub enum SmokeFlavor {
    OpenAi,
    Anthropic,
}

fn parse_smoke_args(args: Vec<String>) -> SmokeArgs {
    let mut smoke = SmokeArgs::default();
    let mut index = 0;

    while index < args.len() {
        match args[index].as_str() {
            "--flavor" => {
                if index + 1 < args.len() {
                    smoke.flavor = match args[index + 1].as_str() {
                        "openai" | "openai-compatible" => Some(SmokeFlavor::OpenAi),
                        "anthropic" | "anthropic-compatible" => Some(SmokeFlavor::Anthropic),
                        _ => smoke.flavor,
                    };
                    index += 2;
                    continue;
                }
            }
            "--prompt" => {
                if index + 1 < args.len() {
                    smoke.prompt = Some(args[index + 1].clone());
                    index += 2;
                    continue;
                }
            }
            _ => {}
        }
        index += 1;
    }

    smoke
}

fn parse_mcp_subcommand(args: Vec<String>) -> Result<McpAction, String> {
    if args.is_empty() {
        return Ok(McpAction::List);
    }

    let action = &args[0];
    match action.as_str() {
        "list" => {
            if args.len() > 1 {
                return Err("mcp list accepts no arguments".to_string());
            }
            Ok(McpAction::List)
        }
        "doctor" => {
            if args.len() > 1 {
                return Err("mcp doctor accepts no arguments".to_string());
            }
            Ok(McpAction::Doctor)
        }
        "tools" => {
            if args.len() > 2 {
                return Err("mcp tools accepts at most one server name".to_string());
            }
            Ok(McpAction::Tools {
                server: args.get(1).cloned(),
            })
        }
        "prompts" => {
            if args.len() > 2 {
                return Err("mcp prompts accepts at most one server name".to_string());
            }
            Ok(McpAction::Prompts {
                server: args.get(1).cloned(),
            })
        }
        "resources" => {
            if args.len() > 2 {
                return Err("mcp resources accepts at most one server name".to_string());
            }
            Ok(McpAction::Resources {
                server: args.get(1).cloned(),
            })
        }
        "resource-templates" | "templates" => {
            if args.len() > 2 {
                return Err("mcp resource-templates accepts at most one server name".to_string());
            }
            Ok(McpAction::ResourceTemplates {
                server: args.get(1).cloned(),
            })
        }
        "call" => {
            if args.len() < 3 {
                return Err("mcp call requires <server> <tool> [json-args]".to_string());
            }
            if args.len() > 4 {
                return Err("mcp call accepts at most one JSON arguments object".to_string());
            }
            Ok(McpAction::Call {
                server: args[1].clone(),
                tool: args[2].clone(),
                arguments_json: args.get(3).cloned(),
            })
        }
        "prompt" => {
            if args.len() < 3 {
                return Err("mcp prompt requires <server> <prompt> [json-args]".to_string());
            }
            if args.len() > 4 {
                return Err("mcp prompt accepts at most one JSON arguments object".to_string());
            }
            Ok(McpAction::Prompt {
                server: args[1].clone(),
                prompt: args[2].clone(),
                arguments_json: args.get(3).cloned(),
            })
        }
        "resource" => {
            if args.len() != 3 {
                return Err("mcp resource requires <server> <uri>".to_string());
            }
            Ok(McpAction::Resource {
                server: args[1].clone(),
                uri: args[2].clone(),
            })
        }
        "add" => parse_mcp_add_args(&args[1..]),
        "get" => {
            if args.len() != 2 {
                return Err("mcp get requires exactly one server name".to_string());
            }
            Ok(McpAction::Get {
                name: args[1].clone(),
            })
        }
        "remove" => parse_mcp_scoped_name_action(&args[1..], "remove").map(|(name, scope)| {
            McpAction::Remove { name, scope }
        }),
        "enable" => parse_mcp_scoped_name_action(&args[1..], "enable").map(|(name, scope)| {
            McpAction::Enable { name, scope }
        }),
        "disable" => parse_mcp_scoped_name_action(&args[1..], "disable").map(|(name, scope)| {
            McpAction::Disable { name, scope }
        }),
        "validate" => {
            if args.len() > 1 {
                return Err("mcp validate accepts no arguments".to_string());
            }
            Ok(McpAction::Validate)
        }
        "init" => {
            let mut force = false;
            for flag in args.iter().skip(1) {
                match flag.as_str() {
                    "--force" => force = true,
                    other => return Err(format!("unknown flag for `mcp init`: {other}")),
                }
            }
            Ok(McpAction::Init { force })
        }
        "add-self" => {
            let mut name = "deepseek".to_string();
            let mut workspace = None;
            let mut scope = McpConfigScope::User;
            let mut index = 1;
            while index < args.len() {
                match args[index].as_str() {
                    "--name" => {
                        let Some(value) = args.get(index + 1) else {
                            return Err("mcp add-self --name requires a value".to_string());
                        };
                        name = value.clone();
                        index += 2;
                    }
                    "--workspace" => {
                        let Some(value) = args.get(index + 1) else {
                            return Err("mcp add-self --workspace requires a value".to_string());
                        };
                        workspace = Some(value.clone());
                        index += 2;
                    }
                    "--user" => {
                        scope = McpConfigScope::User;
                        index += 1;
                    }
                    "--project" => {
                        scope = McpConfigScope::Project;
                        index += 1;
                    }
                    other => return Err(format!("unknown flag for `mcp add-self`: {other}")),
                }
            }
            if name.trim().is_empty() {
                return Err("mcp add-self --name must not be empty".to_string());
            }
            if workspace
                .as_deref()
                .map(str::trim)
                .is_some_and(str::is_empty)
            {
                return Err("mcp add-self --workspace must not be empty".to_string());
            }
            Ok(McpAction::AddSelf {
                name,
                workspace,
                scope,
            })
        }
        other => Err(format!(
            "unknown mcp sub-action `{other}`; expected list|doctor|tools|prompts|call|prompt|add|get|remove|enable|disable|validate|init|add-self"
        )),
    }
}

fn parse_mcp_add_args(args: &[String]) -> Result<McpAction, String> {
    let Some(name) = args.first() else {
        return Err("mcp add requires <name>".to_string());
    };
    if name.trim().is_empty() {
        return Err("mcp add <name> must not be empty".to_string());
    }
    let mut command = None;
    let mut command_args = Vec::new();
    let mut url = None;
    let mut transport = None;
    let mut env = Vec::new();
    let mut headers = Vec::new();
    let mut disabled = false;
    let mut scope = McpConfigScope::User;
    let mut index = 1;
    while index < args.len() {
        match args[index].as_str() {
            "--command" => {
                let Some(value) = args.get(index + 1) else {
                    return Err("mcp add --command requires a value".to_string());
                };
                command = Some(value.clone());
                index += 2;
            }
            "--arg" => {
                let Some(value) = args.get(index + 1) else {
                    return Err("mcp add --arg requires a value".to_string());
                };
                command_args.push(value.clone());
                index += 2;
            }
            "--url" => {
                let Some(value) = args.get(index + 1) else {
                    return Err("mcp add --url requires a value".to_string());
                };
                url = Some(value.clone());
                index += 2;
            }
            "--transport" => {
                let Some(value) = args.get(index + 1) else {
                    return Err("mcp add --transport requires a value".to_string());
                };
                transport = Some(value.clone());
                index += 2;
            }
            "--env" => {
                let Some(value) = args.get(index + 1) else {
                    return Err("mcp add --env requires KEY=VALUE".to_string());
                };
                env.push(parse_key_value_arg(value, "mcp add --env")?);
                index += 2;
            }
            "--header" => {
                let Some(value) = args.get(index + 1) else {
                    return Err("mcp add --header requires KEY=VALUE".to_string());
                };
                headers.push(parse_key_value_arg(value, "mcp add --header")?);
                index += 2;
            }
            "--disabled" => {
                disabled = true;
                index += 1;
            }
            "--user" => {
                scope = McpConfigScope::User;
                index += 1;
            }
            "--project" => {
                scope = McpConfigScope::Project;
                index += 1;
            }
            other => return Err(format!("unknown flag for `mcp add`: {other}")),
        }
    }
    if command.is_some() == url.is_some() {
        return Err("mcp add requires exactly one of --command or --url".to_string());
    }
    if command.as_deref().map(str::trim).is_some_and(str::is_empty) {
        return Err("mcp add --command must not be empty".to_string());
    }
    if url.as_deref().map(str::trim).is_some_and(str::is_empty) {
        return Err("mcp add --url must not be empty".to_string());
    }
    Ok(McpAction::Add {
        name: name.clone(),
        command,
        args: command_args,
        url,
        transport,
        env,
        headers,
        disabled,
        scope,
    })
}

fn parse_mcp_scoped_name_action(
    args: &[String],
    action: &str,
) -> Result<(String, McpConfigScope), String> {
    let Some(name) = args.first() else {
        return Err(format!("mcp {action} requires <name>"));
    };
    if name.trim().is_empty() {
        return Err(format!("mcp {action} <name> must not be empty"));
    }
    let mut scope = McpConfigScope::User;
    for flag in args.iter().skip(1) {
        match flag.as_str() {
            "--user" => scope = McpConfigScope::User,
            "--project" => scope = McpConfigScope::Project,
            other => return Err(format!("unknown flag for `mcp {action}`: {other}")),
        }
    }
    Ok((name.clone(), scope))
}

fn parse_key_value_arg(value: &str, label: &str) -> Result<(String, String), String> {
    let Some((key, value)) = value.split_once('=') else {
        return Err(format!("{label} requires KEY=VALUE"));
    };
    if key.trim().is_empty() {
        return Err(format!("{label} key must not be empty"));
    }
    Ok((key.to_string(), value.to_string()))
}

fn parse_config_args(args: Vec<String>) -> Result<ConfigArgs, String> {
    let mut parsed = ConfigArgs {
        print_default: false,
        init: false,
        force: false,
        network_allow: None,
        network_deny: None,
        auth_env: None,
        auth_stdin: false,
    };

    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--print-default" => {
                parsed.print_default = true;
                index += 1;
            }
            "init" => {
                parsed.init = true;
                index += 1;
            }
            "--force" | "-f" => {
                parsed.force = true;
                index += 1;
            }
            "network" if index + 2 < args.len() && args[index + 1] == "allow" => {
                parsed.network_allow = Some(args[index + 2].clone());
                index += 3;
            }
            "network" if index + 2 < args.len() && args[index + 1] == "deny" => {
                parsed.network_deny = Some(args[index + 2].clone());
                index += 3;
            }
            "network" => {
                return Err("config network requires allow|deny <host>".to_string());
            }
            "auth" => {
                index += 1;
                while index < args.len() {
                    match args[index].as_str() {
                        "--stdin" => {
                            parsed.auth_stdin = true;
                            index += 1;
                        }
                        value if parsed.auth_env.is_none() && !value.starts_with('-') => {
                            parsed.auth_env = Some(value.to_string());
                            index += 1;
                        }
                        other => {
                            return Err(format!(
                                "unknown config auth argument `{other}`; expected [ENV] --stdin"
                            ));
                        }
                    }
                }
            }
            other => {
                return Err(format!(
                    "unknown config argument `{other}`; expected init|auth [ENV] --stdin|network allow|network deny|--force|--print-default"
                ));
            }
        }
    }

    let network_mutations =
        usize::from(parsed.network_allow.is_some()) + usize::from(parsed.network_deny.is_some());
    let auth_mutation = parsed.auth_env.is_some() || parsed.auth_stdin;
    if network_mutations > 1 {
        return Err("config accepts only one network allow/deny mutation at a time".to_string());
    }
    if network_mutations > 0 && (parsed.print_default || parsed.init || parsed.force) {
        return Err(
            "config network allow|deny cannot be combined with init, --force, or --print-default"
                .to_string(),
        );
    }
    if auth_mutation && !parsed.auth_stdin {
        return Err("config auth requires --stdin so secrets are not passed in argv".to_string());
    }
    if auth_mutation && network_mutations > 0 {
        return Err("config auth cannot be combined with config network mutations".to_string());
    }
    if auth_mutation && (parsed.print_default || parsed.init || parsed.force) {
        return Err(
            "config auth cannot be combined with init, --force, or --print-default".to_string(),
        );
    }
    if parsed.print_default && parsed.init {
        return Err("config init cannot be combined with --print-default".to_string());
    }
    if parsed.force && !parsed.init {
        return Err("config --force requires init".to_string());
    }

    Ok(parsed)
}

fn parse_tui_args(args: Vec<String>) -> Result<TuiArgs, String> {
    let mut parsed = TuiArgs::default();
    let mut iter = args.into_iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--demo" => parsed.demo = true,
            "--once" => parsed.once = true,
            "--entrypoint-smoke" => parsed.entrypoint_smoke = true,
            "--smoke-bin" => {
                let Some(value) = iter.next() else {
                    return Err("tui --smoke-bin requires a binary path".to_string());
                };
                if value.is_empty() {
                    return Err("tui --smoke-bin requires a binary path".to_string());
                }
                parsed.entrypoint_smoke = true;
                parsed.smoke_bin = Some(value);
            }
            "--runtime-url" => {
                let Some(value) = iter.next() else {
                    return Err("tui --runtime-url requires a URL".to_string());
                };
                parsed.runtime_url = Some(value);
            }
            other => {
                if let Some(value) = other.strip_prefix("--runtime-url=") {
                    if value.is_empty() {
                        return Err("tui --runtime-url requires a URL".to_string());
                    }
                    parsed.runtime_url = Some(value.to_string());
                    continue;
                }
                if let Some(value) = other.strip_prefix("--smoke-bin=") {
                    if value.is_empty() {
                        return Err("tui --smoke-bin requires a binary path".to_string());
                    }
                    parsed.entrypoint_smoke = true;
                    parsed.smoke_bin = Some(value.to_string());
                    continue;
                }
                return Err(format!(
                    "unknown tui argument `{other}`; expected --demo|--once|--runtime-url <url>|--entrypoint-smoke|--smoke-bin <path>"
                ));
            }
        }
    }
    if parsed.entrypoint_smoke && (parsed.demo || parsed.once || parsed.runtime_url.is_some()) {
        return Err(
            "tui --entrypoint-smoke cannot be combined with --demo, --once, or --runtime-url"
                .to_string(),
        );
    }
    Ok(parsed)
}

fn parse_doctor_args(args: Vec<String>) -> Result<DoctorArgs, String> {
    let mut parsed = DoctorArgs::default();
    for arg in args {
        match arg.as_str() {
            "--json" => parsed.json = true,
            other => {
                return Err(format!(
                    "unknown doctor argument `{other}`; expected --json"
                ));
            }
        }
    }
    Ok(parsed)
}

fn parse_serve_args(args: Vec<String>) -> Result<ServeArgs, String> {
    if args.is_empty() {
        return Err("serve requires one mode: --http|--mcp|--acp".to_string());
    }
    match args[0].as_str() {
        "--http" => Ok(ServeArgs {
            action: ServeAction::Http(parse_serve_http_args(args.into_iter().skip(1).collect())?),
        }),
        "--mcp" => Ok(ServeArgs {
            action: ServeAction::Mcp(parse_serve_mcp_args(args.into_iter().skip(1).collect())?),
        }),
        "--acp" => Ok(ServeArgs {
            action: ServeAction::Acp(parse_serve_acp_args(args.into_iter().skip(1).collect())?),
        }),
        other => Err(format!(
            "unknown serve mode `{other}`; expected --http|--mcp|--acp"
        )),
    }
}

fn parse_serve_acp_args(args: Vec<String>) -> Result<ServeAcpArgs, String> {
    let workspace = parse_optional_workspace_arg(args, "serve --acp")?;
    Ok(ServeAcpArgs { workspace })
}

fn parse_serve_mcp_args(args: Vec<String>) -> Result<ServeMcpArgs, String> {
    let workspace = parse_optional_workspace_arg(args, "serve --mcp")?;
    Ok(ServeMcpArgs { workspace })
}

fn parse_optional_workspace_arg(
    args: Vec<String>,
    command_name: &str,
) -> Result<Option<String>, String> {
    let mut workspace = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--workspace" => {
                let Some(value) = args.get(index + 1) else {
                    return Err(format!("{command_name} --workspace requires a value"));
                };
                if value.trim().is_empty() {
                    return Err(format!("{command_name} --workspace must not be empty"));
                }
                workspace = Some(value.clone());
                index += 2;
            }
            other => {
                return Err(format!(
                    "unknown {command_name} argument `{other}`; expected --workspace <path>"
                ));
            }
        }
    }
    Ok(workspace)
}

fn parse_serve_http_args(args: Vec<String>) -> Result<ServeHttpArgs, String> {
    let mut parsed = ServeHttpArgs {
        addr: "127.0.0.1:8765".to_string(),
        once: false,
    };
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--addr" if index + 1 < args.len() => {
                parsed.addr = args[index + 1].clone();
                index += 2;
            }
            "--addr" => return Err("serve --http --addr requires a value".to_string()),
            "--once" => {
                parsed.once = true;
                index += 1;
            }
            other => {
                return Err(format!(
                    "unknown flag for `serve --http`: {other}; expected --addr|--once"
                ));
            }
        }
    }
    Ok(parsed)
}

fn parse_update_args(args: Vec<String>) -> Result<UpdateArgs, String> {
    let mut parsed = UpdateArgs::default();
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--check" => {
                parsed.check = true;
                index += 1;
            }
            "--print-command" => {
                parsed.print_command = true;
                index += 1;
            }
            "package" => {
                parsed.action = UpdateAction::Package(parse_update_package_args(
                    args.into_iter().skip(index + 1).collect(),
                )?);
                return Ok(parsed);
            }
            "verify-install" => {
                parsed.action = UpdateAction::VerifyInstall(parse_update_verify_install_args(
                    args.into_iter().skip(index + 1).collect(),
                )?);
                return Ok(parsed);
            }
            "install-package" => {
                parsed.action = UpdateAction::InstallPackage(parse_update_install_package_args(
                    args.into_iter().skip(index + 1).collect(),
                )?);
                return Ok(parsed);
            }
            "rollback" => {
                parsed.action = UpdateAction::Rollback(parse_update_rollback_args(
                    args.into_iter().skip(index + 1).collect(),
                )?);
                return Ok(parsed);
            }
            "homebrew-formula" => {
                parsed.action = UpdateAction::HomebrewFormula(parse_update_homebrew_formula_args(
                    args.into_iter().skip(index + 1).collect(),
                )?);
                return Ok(parsed);
            }
            "publish-status" => {
                parsed.action = UpdateAction::PublishStatus(parse_update_publish_status_args(
                    args.into_iter().skip(index + 1).collect(),
                )?);
                return Ok(parsed);
            }
            "download-plan" => {
                parsed.action = UpdateAction::DownloadPlan(parse_update_download_plan_args(
                    args.into_iter().skip(index + 1).collect(),
                )?);
                return Ok(parsed);
            }
            other => {
                return Err(format!(
                    "unknown update argument `{other}`; expected --check|--print-command|package|verify-install|install-package|rollback|homebrew-formula|publish-status|download-plan"
                ));
            }
        }
    }
    Ok(parsed)
}

fn parse_update_package_args(args: Vec<String>) -> Result<UpdatePackageArgs, String> {
    let mut parsed = UpdatePackageArgs::default();
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--out" if index + 1 < args.len() => {
                parsed.out = Some(args[index + 1].clone());
                index += 2;
            }
            "--out" => return Err("update package --out requires a path".to_string()),
            "--bin" if index + 1 < args.len() => {
                parsed.bin = Some(args[index + 1].clone());
                index += 2;
            }
            "--bin" => return Err("update package --bin requires a path".to_string()),
            other => {
                return Err(format!(
                    "unknown flag for `update package`: {other}; expected --out|--bin"
                ));
            }
        }
    }
    Ok(parsed)
}

fn parse_update_verify_install_args(args: Vec<String>) -> Result<UpdateVerifyInstallArgs, String> {
    let mut parsed = UpdateVerifyInstallArgs::default();
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--bin" if index + 1 < args.len() => {
                parsed.bin = Some(args[index + 1].clone());
                index += 2;
            }
            "--bin" => return Err("update verify-install --bin requires a path".to_string()),
            "--workdir" if index + 1 < args.len() => {
                parsed.workdir = Some(args[index + 1].clone());
                index += 2;
            }
            "--workdir" => {
                return Err("update verify-install --workdir requires a path".to_string())
            }
            "--keep-workdir" => {
                parsed.keep_workdir = true;
                index += 1;
            }
            other => {
                return Err(format!(
                    "unknown flag for `update verify-install`: {other}; expected --bin|--workdir|--keep-workdir"
                ));
            }
        }
    }
    Ok(parsed)
}

fn parse_update_install_package_args(
    args: Vec<String>,
) -> Result<UpdateInstallPackageArgs, String> {
    let mut parsed = UpdateInstallPackageArgs::default();
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--package" if index + 1 < args.len() => {
                parsed.package = Some(args[index + 1].clone());
                index += 2;
            }
            "--package" => {
                return Err("update install-package --package requires a path".to_string())
            }
            "--dest" if index + 1 < args.len() => {
                parsed.dest = Some(args[index + 1].clone());
                index += 2;
            }
            "--dest" => return Err("update install-package --dest requires a path".to_string()),
            "--backup-dir" if index + 1 < args.len() => {
                parsed.backup_dir = Some(args[index + 1].clone());
                index += 2;
            }
            "--backup-dir" => {
                return Err("update install-package --backup-dir requires a path".to_string())
            }
            "--dry-run" => {
                parsed.dry_run = true;
                index += 1;
            }
            other => {
                return Err(format!(
                    "unknown flag for `update install-package`: {other}; expected --package|--dest|--backup-dir|--dry-run"
                ));
            }
        }
    }
    Ok(parsed)
}

fn parse_update_rollback_args(args: Vec<String>) -> Result<UpdateRollbackArgs, String> {
    let mut parsed = UpdateRollbackArgs::default();
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--backup" if index + 1 < args.len() => {
                parsed.backup = Some(args[index + 1].clone());
                index += 2;
            }
            "--backup" => return Err("update rollback --backup requires a path".to_string()),
            "--dest" if index + 1 < args.len() => {
                parsed.dest = Some(args[index + 1].clone());
                index += 2;
            }
            "--dest" => return Err("update rollback --dest requires a path".to_string()),
            "--dry-run" => {
                parsed.dry_run = true;
                index += 1;
            }
            other => {
                return Err(format!(
                    "unknown flag for `update rollback`: {other}; expected --backup|--dest|--dry-run"
                ));
            }
        }
    }
    Ok(parsed)
}

fn parse_update_homebrew_formula_args(
    args: Vec<String>,
) -> Result<UpdateHomebrewFormulaArgs, String> {
    let mut parsed = UpdateHomebrewFormulaArgs::default();
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--version" if index + 1 < args.len() => {
                parsed.version = args[index + 1].clone();
                index += 2;
            }
            "--version" => {
                return Err("update homebrew-formula --version requires a value".to_string())
            }
            "--repo" if index + 1 < args.len() => {
                parsed.repo = args[index + 1].clone();
                index += 2;
            }
            "--repo" => {
                return Err("update homebrew-formula --repo requires owner/name".to_string())
            }
            "--dist" if index + 1 < args.len() => {
                parsed.dist = args[index + 1].clone();
                index += 2;
            }
            "--dist" => return Err("update homebrew-formula --dist requires a path".to_string()),
            "--formula" if index + 1 < args.len() => {
                parsed.formula = args[index + 1].clone();
                index += 2;
            }
            "--formula" => {
                return Err("update homebrew-formula --formula requires a path".to_string())
            }
            "--out" if index + 1 < args.len() => {
                parsed.out = Some(args[index + 1].clone());
                index += 2;
            }
            "--out" => return Err("update homebrew-formula --out requires a path".to_string()),
            other => {
                return Err(format!(
                    "unknown flag for `update homebrew-formula`: {other}; expected --version|--repo|--dist|--formula|--out"
                ));
            }
        }
    }
    Ok(parsed)
}

fn parse_update_publish_status_args(args: Vec<String>) -> Result<UpdatePublishStatusArgs, String> {
    let mut parsed = UpdatePublishStatusArgs::default();
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--dist" if index + 1 < args.len() => {
                parsed.dist = Some(args[index + 1].clone());
                index += 2;
            }
            "--dist" => return Err("update publish-status --dist requires a path".to_string()),
            "--npm-dist" if index + 1 < args.len() => {
                parsed.npm_dist = Some(args[index + 1].clone());
                index += 2;
            }
            "--npm-dist" => {
                return Err("update publish-status --npm-dist requires a path".to_string())
            }
            "--strict" => {
                parsed.strict = true;
                index += 1;
            }
            "--json" => {
                parsed.json = true;
                index += 1;
            }
            other => {
                return Err(format!(
                    "unknown flag for `update publish-status`: {other}; expected --dist|--npm-dist|--strict|--json"
                ));
            }
        }
    }
    Ok(parsed)
}

fn parse_update_download_plan_args(args: Vec<String>) -> Result<UpdateDownloadPlanArgs, String> {
    let mut parsed = UpdateDownloadPlanArgs::default();
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--version" if index + 1 < args.len() => {
                parsed.version = Some(args[index + 1].clone());
                index += 2;
            }
            "--version" => {
                return Err("update download-plan --version requires a value".to_string())
            }
            "--repo" if index + 1 < args.len() => {
                parsed.repo = Some(args[index + 1].clone());
                index += 2;
            }
            "--repo" => return Err("update download-plan --repo requires owner/name".to_string()),
            "--base-url" if index + 1 < args.len() => {
                parsed.base_url = Some(args[index + 1].clone());
                index += 2;
            }
            "--base-url" => {
                return Err("update download-plan --base-url requires a URL".to_string())
            }
            "--platform" if index + 1 < args.len() => {
                parsed.platform = Some(args[index + 1].clone());
                index += 2;
            }
            "--platform" => {
                return Err("update download-plan --platform requires a value".to_string())
            }
            "--json" => {
                parsed.json = true;
                index += 1;
            }
            other => {
                return Err(format!(
                    "unknown flag for `update download-plan`: {other}; expected --version|--repo|--base-url|--platform|--json"
                ));
            }
        }
    }
    Ok(parsed)
}

fn parse_benchmark_args(args: Vec<String>) -> BenchmarkArgs {
    let mut benchmark = BenchmarkArgs::default();
    let mut index = 0;

    while index < args.len() {
        match args[index].as_str() {
            "--manifest" => {
                if index + 1 < args.len() {
                    benchmark.manifest = Some(args[index + 1].clone());
                    index += 2;
                    continue;
                }
            }
            "--out" => {
                if index + 1 < args.len() {
                    benchmark.out = Some(args[index + 1].clone());
                    index += 2;
                    continue;
                }
            }
            "--accept-live-baseline" => {
                benchmark.accept_live_baseline = true;
                index += 1;
                continue;
            }
            _ => {}
        }
        index += 1;
    }

    benchmark
}

fn parse_run_args(args: Vec<String>) -> RunArgs {
    let mut skill = None;
    let mut budget: Option<usize> = None;
    let mut benchmark_gate = false;
    let mut positional = Vec::new();
    let mut index = 0;

    while index < args.len() {
        match args[index].as_str() {
            "--skill" if index + 1 < args.len() => {
                skill = Some(args[index + 1].clone());
                index += 2;
                continue;
            }
            "--budget" if index + 1 < args.len() => {
                if let Ok(n) = args[index + 1].parse::<usize>() {
                    if (1..=200).contains(&n) {
                        budget = Some(n);
                    }
                }
                index += 2;
                continue;
            }
            "--benchmark-gate" => {
                benchmark_gate = true;
                index += 1;
                continue;
            }
            _ => {
                positional.push(args[index].clone());
                index += 1;
            }
        }
    }

    let task = positional
        .first()
        .cloned()
        .unwrap_or_else(|| "Run task".to_string());
    RunArgs {
        task,
        skill,
        budget,
        benchmark_gate,
    }
}

fn parse_exec_subcommand(args: Vec<String>) -> Result<ExecAction, String> {
    if args.first().map(|arg| arg.as_str()) == Some("resume") {
        parse_exec_resume_args(args.into_iter().skip(1).collect()).map(ExecAction::Resume)
    } else {
        parse_exec_args(args).map(ExecAction::Run)
    }
}

fn parse_diagnostics_args(args: Vec<String>) -> Result<DiagnosticsArgs, String> {
    let mut changed = false;
    let mut watch = false;
    let mut once = false;
    let mut json = false;
    let mut interval_ms = 1_000_u64;
    let mut paths = Vec::new();
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--changed" => {
                changed = true;
                index += 1;
            }
            "--watch" => {
                watch = true;
                index += 1;
            }
            "--once" => {
                once = true;
                index += 1;
            }
            "--json" => {
                json = true;
                index += 1;
            }
            "--interval-ms" if index + 1 < args.len() => {
                interval_ms = args[index + 1]
                    .parse::<u64>()
                    .map_err(|_| "diagnostics --interval-ms must be a number".to_string())?;
                index += 2;
            }
            "--interval-ms" => {
                return Err("diagnostics --interval-ms requires a value".to_string());
            }
            "--" => {
                paths.extend(args.into_iter().skip(index + 1));
                break;
            }
            other if other.starts_with('-') => {
                return Err(format!(
                    "unknown flag for `diagnostics`: {other}; expected --changed|--watch|--once|--json|--interval-ms"
                ));
            }
            value => {
                paths.push(value.to_string());
                index += 1;
            }
        }
    }
    Ok(DiagnosticsArgs {
        changed,
        watch,
        once,
        json,
        interval_ms,
        paths,
    })
}

fn parse_restore_subcommand(args: Vec<String>) -> Result<RestoreAction, String> {
    if args.is_empty() {
        return Ok(RestoreAction::List { limit: 20 });
    }

    match args[0].as_str() {
        "snapshot" => {
            let mut label = None;
            let mut label_parts = Vec::new();
            let mut index = 1;
            while index < args.len() {
                match args[index].as_str() {
                    "--label" if index + 1 < args.len() => {
                        if !label_parts.is_empty() {
                            return Err(
                                "restore snapshot accepts either --label or a positional label"
                                    .to_string(),
                            );
                        }
                        label = Some(args[index + 1].clone());
                        index += 2;
                    }
                    "--label" => {
                        return Err("restore snapshot --label requires a value".to_string())
                    }
                    other if other.starts_with('-') => {
                        return Err(format!("unknown flag for `restore snapshot`: {other}"));
                    }
                    value => {
                        if label.is_some() {
                            return Err(
                                "restore snapshot accepts either --label or a positional label"
                                    .to_string(),
                            );
                        }
                        label_parts.push(value.to_string());
                        index += 1;
                    }
                }
            }
            if label.is_none() && !label_parts.is_empty() {
                label = Some(label_parts.join(" "));
            }
            Ok(RestoreAction::Snapshot { label })
        }
        "list" => {
            let mut limit = 20;
            let mut index = 1;
            while index < args.len() {
                match args[index].as_str() {
                    "--limit" if index + 1 < args.len() => {
                        limit = args[index + 1]
                            .parse::<usize>()
                            .ok()
                            .filter(|value| (1..=200).contains(value))
                            .unwrap_or(20);
                        index += 2;
                    }
                    "--limit" => return Err("restore list --limit requires a value".to_string()),
                    other => return Err(format!("unknown flag for `restore list`: {other}")),
                }
            }
            Ok(RestoreAction::List { limit })
        }
        "show" => {
            let id = args
                .get(1)
                .cloned()
                .ok_or_else(|| "restore show requires a snapshot id".to_string())?;
            let mut patch = false;
            for arg in args.iter().skip(2) {
                match arg.as_str() {
                    "--patch" => patch = true,
                    other => return Err(format!("unknown flag for `restore show`: {other}")),
                }
            }
            Ok(RestoreAction::Show { id, patch })
        }
        "revert-turn" | "revert_turn" => {
            let id = args
                .get(1)
                .cloned()
                .ok_or_else(|| "restore revert-turn requires a snapshot id".to_string())?;
            let mut apply = false;
            for arg in args.iter().skip(2) {
                match arg.as_str() {
                    "--apply" => apply = true,
                    other => {
                        return Err(format!("unknown flag for `restore revert-turn`: {other}"))
                    }
                }
            }
            Ok(RestoreAction::RevertTurn { id, apply })
        }
        other => Err(format!(
            "unknown restore sub-action `{other}`; expected snapshot|list|show|revert-turn"
        )),
    }
}

fn parse_agents_subcommand(args: Vec<String>) -> Result<AgentsAction, String> {
    if args.is_empty() {
        return Ok(AgentsAction::List);
    }

    match args[0].as_str() {
        "list" => {
            if args.len() > 1 {
                return Err("agents list accepts no arguments".to_string());
            }
            Ok(AgentsAction::List)
        }
        "show" => {
            if args.len() != 2 {
                return Err("agents show requires exactly one agent name".to_string());
            }
            Ok(AgentsAction::Show {
                name: args[1].clone(),
            })
        }
        "validate" => {
            if args.len() > 2 {
                return Err("agents validate accepts at most one path".to_string());
            }
            Ok(AgentsAction::Validate {
                path: args.get(1).cloned(),
            })
        }
        "run-task" => parse_agents_run_task_args(args.into_iter().skip(1).collect()),
        "daemon" => parse_agents_daemon_args(args.into_iter().skip(1).collect()),
        "rlm-status" => parse_agents_rlm_status_args(args.into_iter().skip(1).collect()),
        "rlm-events" => parse_agents_rlm_events_args(args.into_iter().skip(1).collect()),
        "rlm-wait" => parse_agents_rlm_wait_args(args.into_iter().skip(1).collect()),
        "rlm-cancel" => parse_agents_rlm_cancel_args(args.into_iter().skip(1).collect()),
        "rlm-recover" => parse_agents_rlm_recover_args(args.into_iter().skip(1).collect()),
        "rlm-stop" => parse_agents_rlm_stop_args(args.into_iter().skip(1).collect()),
        "rlm-run-next" => parse_agents_rlm_run_next_args(args.into_iter().skip(1).collect()),
        "rlm-drain" => parse_agents_rlm_drain_args(args.into_iter().skip(1).collect()),
        "shell" => parse_agents_shell_args(args.into_iter().skip(1).collect()),
        "shell-supervisor" => {
            parse_agents_shell_supervisor_args(args.into_iter().skip(1).collect())
        }
        "service" => parse_agents_service_args(args.into_iter().skip(1).collect()),
        "service-doctor" => parse_agents_service_doctor_args(args.into_iter().skip(1).collect()),
        "service-smoke" => parse_agents_service_smoke_args(args.into_iter().skip(1).collect()),
        "threads" => {
            if args.len() > 1 {
                return Err("agents threads accepts no arguments".to_string());
            }
            Ok(AgentsAction::Threads)
        }
        "show-thread" => {
            if args.len() != 2 {
                return Err("agents show-thread requires exactly one thread id".to_string());
            }
            Ok(AgentsAction::ShowThread {
                id: args[1].clone(),
            })
        }
        "switch" => {
            if args.len() != 2 {
                return Err("agents switch requires exactly one thread id".to_string());
            }
            Ok(AgentsAction::SwitchThread {
                id: args[1].clone(),
            })
        }
        "current" => {
            if args.len() > 1 {
                return Err("agents current accepts no arguments".to_string());
            }
            Ok(AgentsAction::CurrentThread)
        }
        "clear-current" => {
            if args.len() > 1 {
                return Err("agents clear-current accepts no arguments".to_string());
            }
            Ok(AgentsAction::ClearThread)
        }
        other => Err(format!(
            "unknown agents sub-action `{other}`; expected list|show|validate|run-task|daemon|rlm-status|rlm-events|rlm-wait|rlm-cancel|rlm-recover|rlm-stop|rlm-run-next|rlm-drain|shell|shell-supervisor|service|service-doctor|service-smoke|threads|show-thread|switch|current|clear-current"
        )),
    }
}

fn parse_agents_shell_args(args: Vec<String>) -> Result<AgentsAction, String> {
    if args.is_empty() {
        return Err("agents shell requires an action: status|show|start|wait|replay|attach|stdin|send|resize|cancel|shutdown".to_string());
    }
    let action = args[0].clone();
    let rest = args.into_iter().skip(1).collect::<Vec<_>>();
    match action.as_str() {
        "status" | "health" => {
            parse_agents_shell_empty_args(&action, rest, AgentsShellAction::Status)
        }
        "show" => parse_agents_shell_empty_args(&action, rest, AgentsShellAction::Show),
        "start" => parse_agents_shell_start_args(rest),
        "wait" => parse_agents_shell_wait_args(rest),
        "replay" => parse_agents_shell_replay_args(rest),
        "attach" => parse_agents_shell_attach_args(rest),
        "stdin" | "send" => parse_agents_shell_stdin_args(&action, rest),
        "resize" => parse_agents_shell_resize_args(rest),
        "cancel" => parse_agents_shell_cancel_args(rest),
        "shutdown" => parse_agents_shell_empty_args(&action, rest, AgentsShellAction::Shutdown),
        other => Err(format!(
            "unknown agents shell action `{other}`; expected status|show|start|wait|replay|attach|stdin|send|resize|cancel|shutdown"
        )),
    }
}

fn parse_agents_shell_empty_args(
    action_name: &str,
    args: Vec<String>,
    action: AgentsShellAction,
) -> Result<AgentsAction, String> {
    let mut json = false;
    for arg in args {
        match arg.as_str() {
            "--json" => json = true,
            other => {
                return Err(format!(
                    "unknown flag for `agents shell {action_name}`: {other}; expected --json"
                ));
            }
        }
    }
    Ok(AgentsAction::Shell(AgentsShellArgs { action, json }))
}

fn parse_agents_shell_start_args(args: Vec<String>) -> Result<AgentsAction, String> {
    let mut json = false;
    let mut cwd = None;
    let mut tty = false;
    let mut tty_rows = None;
    let mut tty_cols = None;
    let mut command_parts = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--json" => json = true,
            "--tty" => tty = true,
            "--cwd" => {
                i += 1;
                cwd = Some(require_agents_shell_value(&args, i, "start", "--cwd")?);
            }
            "--rows" | "--tty-rows" => {
                i += 1;
                tty_rows = Some(parse_agents_shell_u64(&args, i, "start", "--rows")?);
            }
            "--cols" | "--tty-cols" => {
                i += 1;
                tty_cols = Some(parse_agents_shell_u64(&args, i, "start", "--cols")?);
            }
            "--" => {
                command_parts.extend(args.iter().skip(i + 1).cloned());
                break;
            }
            value if value.starts_with("--") => {
                return Err(format!(
                    "unknown flag for `agents shell start`: {value}; expected --tty|--cwd|--rows|--cols|--json|--"
                ));
            }
            value => command_parts.push(value.to_string()),
        }
        i += 1;
    }
    let command = command_parts.join(" ").trim().to_string();
    if command.is_empty() {
        return Err("agents shell start requires a command".to_string());
    }
    Ok(AgentsAction::Shell(AgentsShellArgs {
        action: AgentsShellAction::Start {
            command,
            cwd,
            tty,
            tty_rows,
            tty_cols,
        },
        json,
    }))
}

fn parse_agents_shell_wait_args(args: Vec<String>) -> Result<AgentsAction, String> {
    let mut json = false;
    let mut task_id = None;
    let mut timeout_ms = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--json" => json = true,
            "--timeout-ms" => {
                i += 1;
                timeout_ms = Some(parse_agents_shell_u64(&args, i, "wait", "--timeout-ms")?);
            }
            value if value.starts_with("--") => {
                return Err(format!(
                    "unknown flag for `agents shell wait`: {value}; expected --timeout-ms|--json"
                ));
            }
            value => set_agents_shell_task_id(&mut task_id, value, "wait")?,
        }
        i += 1;
    }
    let task_id = task_id.ok_or_else(|| "agents shell wait requires a task id".to_string())?;
    Ok(AgentsAction::Shell(AgentsShellArgs {
        action: AgentsShellAction::Wait {
            task_id,
            timeout_ms,
        },
        json,
    }))
}

fn parse_agents_shell_replay_args(args: Vec<String>) -> Result<AgentsAction, String> {
    let mut json = false;
    let mut task_id = None;
    let mut stream = None;
    let mut cursor = None;
    let mut offset = None;
    let mut limit_bytes = None;
    let mut tail = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--json" => json = true,
            "--tail" => tail = true,
            "--stream" => {
                i += 1;
                stream = Some(require_agents_shell_value(&args, i, "replay", "--stream")?);
            }
            "--cursor" => {
                i += 1;
                cursor = Some(parse_agents_shell_u64(&args, i, "replay", "--cursor")?);
            }
            "--offset" => {
                i += 1;
                offset = Some(parse_agents_shell_u64(&args, i, "replay", "--offset")?);
            }
            "--limit-bytes" => {
                i += 1;
                limit_bytes = Some(parse_agents_shell_u64(&args, i, "replay", "--limit-bytes")?);
            }
            value if value.starts_with("--") => {
                return Err(format!(
                    "unknown flag for `agents shell replay`: {value}; expected --stream|--cursor|--offset|--limit-bytes|--tail|--json"
                ));
            }
            value => set_agents_shell_task_id(&mut task_id, value, "replay")?,
        }
        i += 1;
    }
    let task_id = task_id.ok_or_else(|| "agents shell replay requires a task id".to_string())?;
    Ok(AgentsAction::Shell(AgentsShellArgs {
        action: AgentsShellAction::Replay {
            task_id,
            stream,
            cursor,
            offset,
            limit_bytes,
            tail,
        },
        json,
    }))
}

fn parse_agents_shell_attach_args(args: Vec<String>) -> Result<AgentsAction, String> {
    let mut json = false;
    let mut task_id = None;
    let mut cursor = None;
    let mut wait_ms = None;
    let mut limit_bytes = None;
    let mut tail = false;
    let mut follow = false;
    let mut interactive = false;
    let mut poll_ms = None;
    let mut max_ms = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--json" => json = true,
            "--tail" => tail = true,
            "--follow" => follow = true,
            "--interactive" | "--takeover" => interactive = true,
            "--cursor" => {
                i += 1;
                cursor = Some(parse_agents_shell_u64(&args, i, "attach", "--cursor")?);
            }
            "--wait-ms" => {
                i += 1;
                wait_ms = Some(parse_agents_shell_u64(&args, i, "attach", "--wait-ms")?);
            }
            "--poll-ms" => {
                i += 1;
                poll_ms = Some(parse_agents_shell_u64(&args, i, "attach", "--poll-ms")?);
            }
            "--max-ms" => {
                i += 1;
                max_ms = Some(parse_agents_shell_u64(&args, i, "attach", "--max-ms")?);
            }
            "--limit-bytes" => {
                i += 1;
                limit_bytes = Some(parse_agents_shell_u64(&args, i, "attach", "--limit-bytes")?);
            }
            value if value.starts_with("--") => {
                return Err(format!(
                    "unknown flag for `agents shell attach`: {value}; expected --cursor|--wait-ms|--poll-ms|--max-ms|--limit-bytes|--tail|--follow|--interactive|--takeover|--json"
                ));
            }
            value => set_agents_shell_task_id(&mut task_id, value, "attach")?,
        }
        i += 1;
    }
    let task_id = task_id.ok_or_else(|| "agents shell attach requires a task id".to_string())?;
    Ok(AgentsAction::Shell(AgentsShellArgs {
        action: AgentsShellAction::Attach {
            task_id,
            cursor,
            wait_ms,
            limit_bytes,
            tail,
            follow,
            interactive,
            poll_ms,
            max_ms,
        },
        json,
    }))
}

fn parse_agents_shell_stdin_args(
    action_name: &str,
    args: Vec<String>,
) -> Result<AgentsAction, String> {
    let mut json = false;
    let mut task_id = None;
    let mut input = None;
    let mut close_stdin = false;
    let mut timeout_ms = None;
    let mut positional_input = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--json" => json = true,
            "--close-stdin" => close_stdin = true,
            "--input" | "--stdin" | "--data" => {
                i += 1;
                input = Some(require_agents_shell_value(
                    &args,
                    i,
                    action_name,
                    "--input",
                )?);
            }
            "--timeout-ms" => {
                i += 1;
                timeout_ms = Some(parse_agents_shell_u64(
                    &args,
                    i,
                    action_name,
                    "--timeout-ms",
                )?);
            }
            "--" => {
                positional_input.extend(args.iter().skip(i + 1).cloned());
                break;
            }
            value if value.starts_with("--") => {
                return Err(format!(
                    "unknown flag for `agents shell {action_name}`: {value}; expected --input|--close-stdin|--timeout-ms|--json|--"
                ));
            }
            value if task_id.is_none() => task_id = Some(value.to_string()),
            value => positional_input.push(value.to_string()),
        }
        i += 1;
    }
    if input.is_none() && !positional_input.is_empty() {
        input = Some(positional_input.join(" "));
    }
    if input.is_none() && !close_stdin {
        return Err(format!(
            "agents shell {action_name} requires --input, positional input, or --close-stdin"
        ));
    }
    let task_id =
        task_id.ok_or_else(|| format!("agents shell {action_name} requires a task id"))?;
    Ok(AgentsAction::Shell(AgentsShellArgs {
        action: AgentsShellAction::Stdin {
            task_id,
            input,
            close_stdin,
            timeout_ms,
        },
        json,
    }))
}

fn parse_agents_shell_resize_args(args: Vec<String>) -> Result<AgentsAction, String> {
    let mut json = false;
    let mut task_id = None;
    let mut tty_rows = None;
    let mut tty_cols = None;
    let mut positional_sizes = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--json" => json = true,
            "--rows" | "--tty-rows" => {
                i += 1;
                tty_rows = Some(parse_agents_shell_u64(&args, i, "resize", "--rows")?);
            }
            "--cols" | "--tty-cols" => {
                i += 1;
                tty_cols = Some(parse_agents_shell_u64(&args, i, "resize", "--cols")?);
            }
            value if value.starts_with("--") => {
                return Err(format!(
                    "unknown flag for `agents shell resize`: {value}; expected --rows|--cols|--json"
                ));
            }
            value if task_id.is_none() => task_id = Some(value.to_string()),
            value => positional_sizes.push(value.to_string()),
        }
        i += 1;
    }
    if tty_rows.is_none() && !positional_sizes.is_empty() {
        tty_rows = Some(parse_agents_shell_u64_value(
            &positional_sizes[0],
            "resize",
            "rows",
        )?);
    }
    if tty_cols.is_none() && positional_sizes.len() > 1 {
        tty_cols = Some(parse_agents_shell_u64_value(
            &positional_sizes[1],
            "resize",
            "cols",
        )?);
    }
    if positional_sizes.len() > 2 {
        return Err("agents shell resize accepts at most rows and cols after task id".to_string());
    }
    let task_id = task_id.ok_or_else(|| "agents shell resize requires a task id".to_string())?;
    let tty_rows = tty_rows.ok_or_else(|| "agents shell resize requires --rows".to_string())?;
    let tty_cols = tty_cols.ok_or_else(|| "agents shell resize requires --cols".to_string())?;
    Ok(AgentsAction::Shell(AgentsShellArgs {
        action: AgentsShellAction::Resize {
            task_id,
            tty_rows,
            tty_cols,
        },
        json,
    }))
}

fn parse_agents_shell_cancel_args(args: Vec<String>) -> Result<AgentsAction, String> {
    let mut json = false;
    let mut all = false;
    let mut task_id = None;
    for arg in args {
        match arg.as_str() {
            "--json" => json = true,
            "--all" => all = true,
            value if value.starts_with("--") => {
                return Err(format!(
                    "unknown flag for `agents shell cancel`: {value}; expected --all|--json"
                ));
            }
            value => set_agents_shell_task_id(&mut task_id, value, "cancel")?,
        }
    }
    if !all && task_id.is_none() {
        return Err("agents shell cancel requires a task id or --all".to_string());
    }
    Ok(AgentsAction::Shell(AgentsShellArgs {
        action: AgentsShellAction::Cancel { task_id, all },
        json,
    }))
}

fn set_agents_shell_task_id(
    task_id: &mut Option<String>,
    value: &str,
    action: &str,
) -> Result<(), String> {
    if task_id.is_some() {
        return Err(format!("agents shell {action} accepts exactly one task id"));
    }
    *task_id = Some(value.to_string());
    Ok(())
}

fn require_agents_shell_value(
    args: &[String],
    index: usize,
    action: &str,
    flag: &str,
) -> Result<String, String> {
    args.get(index)
        .cloned()
        .ok_or_else(|| format!("agents shell {action} {flag} requires a value"))
}

fn parse_agents_shell_u64(
    args: &[String],
    index: usize,
    action: &str,
    flag: &str,
) -> Result<u64, String> {
    let value = require_agents_shell_value(args, index, action, flag)?;
    parse_agents_shell_u64_value(&value, action, flag)
}

fn parse_agents_shell_u64_value(value: &str, action: &str, name: &str) -> Result<u64, String> {
    value
        .parse::<u64>()
        .map_err(|_| format!("agents shell {action} {name} must be a number"))
}

fn parse_agents_shell_supervisor_args(args: Vec<String>) -> Result<AgentsAction, String> {
    let mut parsed = AgentsShellSupervisorArgs {
        once: false,
        json: false,
    };
    for arg in args {
        match arg.as_str() {
            "--once" => parsed.once = true,
            "--json" => parsed.json = true,
            other => {
                return Err(format!(
                    "unknown flag for `agents shell-supervisor`: {other}; expected --once|--json"
                ));
            }
        }
    }
    Ok(AgentsAction::ShellSupervisor(parsed))
}

fn parse_agents_run_task_args(args: Vec<String>) -> Result<AgentsAction, String> {
    let mut id = None;
    let mut budget = None;
    let mut json = false;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--budget" if index + 1 < args.len() => {
                budget = Some(
                    args[index + 1]
                        .parse::<usize>()
                        .map_err(|_| "agents run-task --budget must be a number".to_string())?,
                );
                index += 2;
            }
            "--budget" => return Err("agents run-task --budget requires a value".to_string()),
            "--json" => {
                json = true;
                index += 1;
            }
            value if value.starts_with('-') => {
                return Err(format!(
                    "unknown flag for `agents run-task`: {value}; expected --budget|--json"
                ));
            }
            value => {
                if id.is_some() {
                    return Err("agents run-task accepts exactly one task id".to_string());
                }
                id = Some(value.to_string());
                index += 1;
            }
        }
    }
    let id = id.ok_or_else(|| "agents run-task requires a task id".to_string())?;
    Ok(AgentsAction::RunTask { id, budget, json })
}

fn parse_agents_daemon_args(args: Vec<String>) -> Result<AgentsAction, String> {
    let mut budget = None;
    let mut interval_ms = 1_000_u64;
    let mut once = false;
    let mut json = false;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--budget" if index + 1 < args.len() => {
                budget = Some(
                    args[index + 1]
                        .parse::<usize>()
                        .map_err(|_| "agents daemon --budget must be a number".to_string())?,
                );
                index += 2;
            }
            "--budget" => return Err("agents daemon --budget requires a value".to_string()),
            "--interval-ms" if index + 1 < args.len() => {
                interval_ms = args[index + 1]
                    .parse::<u64>()
                    .map_err(|_| "agents daemon --interval-ms must be a number".to_string())?;
                index += 2;
            }
            "--interval-ms" => {
                return Err("agents daemon --interval-ms requires a value".to_string());
            }
            "--once" => {
                once = true;
                index += 1;
            }
            "--json" => {
                json = true;
                index += 1;
            }
            value => {
                return Err(format!(
                    "unknown flag for `agents daemon`: {value}; expected --budget|--interval-ms|--once|--json"
                ));
            }
        }
    }
    Ok(AgentsAction::Daemon {
        budget,
        interval_ms,
        once,
        json,
    })
}

fn parse_agents_rlm_status_args(args: Vec<String>) -> Result<AgentsAction, String> {
    let mut session_id = None;
    let mut limit = None;
    let mut json = false;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--limit" if index + 1 < args.len() => {
                limit = Some(
                    args[index + 1]
                        .parse::<usize>()
                        .map_err(|_| "agents rlm-status --limit must be a number".to_string())?,
                );
                index += 2;
            }
            "--limit" => return Err("agents rlm-status --limit requires a value".to_string()),
            "--json" => {
                json = true;
                index += 1;
            }
            value if value.starts_with('-') => {
                return Err(format!(
                    "unknown flag for `agents rlm-status`: {value}; expected --limit|--json"
                ));
            }
            value => {
                if session_id.is_some() {
                    return Err("agents rlm-status accepts at most one session id".to_string());
                }
                session_id = Some(value.to_string());
                index += 1;
            }
        }
    }
    Ok(AgentsAction::RlmStatus(AgentsRlmStatusArgs {
        session_id,
        limit,
        json,
    }))
}

fn parse_agents_rlm_events_args(args: Vec<String>) -> Result<AgentsAction, String> {
    let mut session_id = None;
    let mut cursor = None;
    let mut limit = None;
    let mut json = false;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--cursor" | "--since-seq" if index + 1 < args.len() => {
                cursor = Some(args[index + 1].parse::<u64>().map_err(|_| {
                    "agents rlm-events --cursor/--since-seq must be a number".to_string()
                })?);
                index += 2;
            }
            "--cursor" | "--since-seq" => {
                return Err("agents rlm-events --cursor/--since-seq requires a value".to_string());
            }
            "--limit" if index + 1 < args.len() => {
                limit = Some(
                    args[index + 1]
                        .parse::<usize>()
                        .map_err(|_| "agents rlm-events --limit must be a number".to_string())?,
                );
                index += 2;
            }
            "--limit" => return Err("agents rlm-events --limit requires a value".to_string()),
            "--json" => {
                json = true;
                index += 1;
            }
            value if value.starts_with('-') => {
                return Err(format!(
                    "unknown flag for `agents rlm-events`: {value}; expected --cursor|--since-seq|--limit|--json"
                ));
            }
            value => {
                if session_id.is_some() {
                    return Err("agents rlm-events accepts exactly one session id".to_string());
                }
                session_id = Some(value.to_string());
                index += 1;
            }
        }
    }
    let session_id =
        session_id.ok_or_else(|| "agents rlm-events requires a session id".to_string())?;
    Ok(AgentsAction::RlmEvents(AgentsRlmEventsArgs {
        session_id,
        cursor,
        limit,
        json,
    }))
}

fn parse_agents_rlm_wait_args(args: Vec<String>) -> Result<AgentsAction, String> {
    let mut session_id = None;
    let mut cursor = None;
    let mut limit = None;
    let mut timeout_ms = None;
    let mut poll_interval_ms = None;
    let mut json = false;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--cursor" | "--since-seq" if index + 1 < args.len() => {
                cursor = Some(args[index + 1].parse::<u64>().map_err(|_| {
                    "agents rlm-wait --cursor/--since-seq must be a number".to_string()
                })?);
                index += 2;
            }
            "--cursor" | "--since-seq" => {
                return Err("agents rlm-wait --cursor/--since-seq requires a value".to_string());
            }
            "--limit" if index + 1 < args.len() => {
                limit = Some(
                    args[index + 1]
                        .parse::<usize>()
                        .map_err(|_| "agents rlm-wait --limit must be a number".to_string())?,
                );
                index += 2;
            }
            "--limit" => return Err("agents rlm-wait --limit requires a value".to_string()),
            "--timeout-ms" if index + 1 < args.len() => {
                timeout_ms =
                    Some(args[index + 1].parse::<u64>().map_err(|_| {
                        "agents rlm-wait --timeout-ms must be a number".to_string()
                    })?);
                index += 2;
            }
            "--timeout-ms" => {
                return Err("agents rlm-wait --timeout-ms requires a value".to_string());
            }
            "--poll-interval-ms" if index + 1 < args.len() => {
                poll_interval_ms = Some(args[index + 1].parse::<u64>().map_err(|_| {
                    "agents rlm-wait --poll-interval-ms must be a number".to_string()
                })?);
                index += 2;
            }
            "--poll-interval-ms" => {
                return Err("agents rlm-wait --poll-interval-ms requires a value".to_string());
            }
            "--json" => {
                json = true;
                index += 1;
            }
            value if value.starts_with('-') => {
                return Err(format!(
                    "unknown flag for `agents rlm-wait`: {value}; expected --cursor|--since-seq|--limit|--timeout-ms|--poll-interval-ms|--json"
                ));
            }
            value => {
                if session_id.is_some() {
                    return Err("agents rlm-wait accepts exactly one session id".to_string());
                }
                session_id = Some(value.to_string());
                index += 1;
            }
        }
    }
    let session_id =
        session_id.ok_or_else(|| "agents rlm-wait requires a session id".to_string())?;
    Ok(AgentsAction::RlmWait(AgentsRlmWaitArgs {
        session_id,
        cursor,
        limit,
        timeout_ms,
        poll_interval_ms,
        json,
    }))
}

fn parse_agents_rlm_cancel_args(args: Vec<String>) -> Result<AgentsAction, String> {
    let mut positionals = Vec::new();
    let mut all = false;
    let mut force = false;
    let mut reason = None;
    let mut json = false;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--all" => {
                all = true;
                index += 1;
            }
            "--force" => {
                force = true;
                index += 1;
            }
            "--reason" if index + 1 < args.len() => {
                reason = Some(args[index + 1].clone());
                index += 2;
            }
            "--reason" => return Err("agents rlm-cancel --reason requires a value".to_string()),
            "--json" => {
                json = true;
                index += 1;
            }
            value if value.starts_with('-') => {
                return Err(format!(
                    "unknown flag for `agents rlm-cancel`: {value}; expected --all|--force|--reason|--json"
                ));
            }
            value => {
                if positionals.len() >= 2 {
                    return Err(
                        "agents rlm-cancel accepts a session id and optional task id".to_string(),
                    );
                }
                positionals.push(value.to_string());
                index += 1;
            }
        }
    }
    let session_id = positionals
        .first()
        .cloned()
        .ok_or_else(|| "agents rlm-cancel requires a session id".to_string())?;
    let task_id = positionals.get(1).cloned();
    if task_id.is_none() && !all {
        return Err("agents rlm-cancel requires a task id or --all".to_string());
    }
    Ok(AgentsAction::RlmCancel(AgentsRlmCancelArgs {
        session_id,
        task_id,
        all,
        force,
        reason,
        json,
    }))
}

fn parse_agents_rlm_recover_args(args: Vec<String>) -> Result<AgentsAction, String> {
    let mut session_id = None;
    let mut all = false;
    let mut mode = None;
    let mut dry_run = false;
    let mut force = false;
    let mut limit = None;
    let mut reason = None;
    let mut json = false;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--all" => {
                all = true;
                index += 1;
            }
            "--mode" if index + 1 < args.len() => {
                let value = args[index + 1].as_str();
                if !matches!(value, "requeue" | "fail") {
                    return Err("agents rlm-recover --mode must be `requeue` or `fail`".to_string());
                }
                mode = Some(value.to_string());
                index += 2;
            }
            "--mode" => return Err("agents rlm-recover --mode requires a value".to_string()),
            "--dry-run" => {
                dry_run = true;
                index += 1;
            }
            "--force" => {
                force = true;
                index += 1;
            }
            "--limit" if index + 1 < args.len() => {
                limit = Some(
                    args[index + 1]
                        .parse::<usize>()
                        .map_err(|_| "agents rlm-recover --limit must be a number".to_string())?,
                );
                index += 2;
            }
            "--limit" => return Err("agents rlm-recover --limit requires a value".to_string()),
            "--reason" if index + 1 < args.len() => {
                reason = Some(args[index + 1].clone());
                index += 2;
            }
            "--reason" => return Err("agents rlm-recover --reason requires a value".to_string()),
            "--json" => {
                json = true;
                index += 1;
            }
            value if value.starts_with('-') => {
                return Err(format!(
                    "unknown flag for `agents rlm-recover`: {value}; expected --all|--mode|--dry-run|--force|--limit|--reason|--json"
                ));
            }
            value => {
                if session_id.is_some() {
                    return Err("agents rlm-recover accepts at most one session id".to_string());
                }
                session_id = Some(value.to_string());
                index += 1;
            }
        }
    }
    if session_id.is_none() && !all {
        return Err("agents rlm-recover requires a session id or --all".to_string());
    }
    Ok(AgentsAction::RlmRecover(AgentsRlmRecoverArgs {
        session_id,
        all,
        mode,
        dry_run,
        force,
        limit,
        reason,
        json,
    }))
}

fn parse_agents_rlm_stop_args(args: Vec<String>) -> Result<AgentsAction, String> {
    let mut session_id = None;
    let mut reason = None;
    let mut json = false;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--reason" if index + 1 < args.len() => {
                reason = Some(args[index + 1].clone());
                index += 2;
            }
            "--reason" => return Err("agents rlm-stop --reason requires a value".to_string()),
            "--json" => {
                json = true;
                index += 1;
            }
            value if value.starts_with('-') => {
                return Err(format!(
                    "unknown flag for `agents rlm-stop`: {value}; expected --reason|--json"
                ));
            }
            value => {
                if session_id.is_some() {
                    return Err("agents rlm-stop accepts exactly one session id".to_string());
                }
                session_id = Some(value.to_string());
                index += 1;
            }
        }
    }
    let session_id =
        session_id.ok_or_else(|| "agents rlm-stop requires a session id".to_string())?;
    Ok(AgentsAction::RlmStop(AgentsRlmStopArgs {
        session_id,
        reason,
        json,
    }))
}

fn parse_agents_rlm_run_next_args(args: Vec<String>) -> Result<AgentsAction, String> {
    let mut positionals = Vec::new();
    let mut dry_run = false;
    let mut json = false;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--dry-run" => {
                dry_run = true;
                index += 1;
            }
            "--json" => {
                json = true;
                index += 1;
            }
            value if value.starts_with('-') => {
                return Err(format!(
                    "unknown flag for `agents rlm-run-next`: {value}; expected --dry-run|--json"
                ));
            }
            value => {
                if positionals.len() >= 2 {
                    return Err(
                        "agents rlm-run-next accepts a session id and optional task id".to_string(),
                    );
                }
                positionals.push(value.to_string());
                index += 1;
            }
        }
    }
    let session_id = positionals
        .first()
        .cloned()
        .ok_or_else(|| "agents rlm-run-next requires a session id".to_string())?;
    Ok(AgentsAction::RlmRunNext(AgentsRlmRunNextArgs {
        session_id,
        task_id: positionals.get(1).cloned(),
        dry_run,
        json,
    }))
}

fn parse_agents_rlm_drain_args(args: Vec<String>) -> Result<AgentsAction, String> {
    let mut session_id = None;
    let mut max_turns = None;
    let mut dry_run = false;
    let mut json = false;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--max-turns" if index + 1 < args.len() => {
                max_turns =
                    Some(args[index + 1].parse::<usize>().map_err(|_| {
                        "agents rlm-drain --max-turns must be a number".to_string()
                    })?);
                index += 2;
            }
            "--max-turns" => {
                return Err("agents rlm-drain --max-turns requires a value".to_string());
            }
            "--dry-run" => {
                dry_run = true;
                index += 1;
            }
            "--json" => {
                json = true;
                index += 1;
            }
            value if value.starts_with('-') => {
                return Err(format!(
                    "unknown flag for `agents rlm-drain`: {value}; expected --max-turns|--dry-run|--json"
                ));
            }
            value => {
                if session_id.is_some() {
                    return Err("agents rlm-drain accepts exactly one session id".to_string());
                }
                session_id = Some(value.to_string());
                index += 1;
            }
        }
    }
    let session_id =
        session_id.ok_or_else(|| "agents rlm-drain requires a session id".to_string())?;
    Ok(AgentsAction::RlmDrain(AgentsRlmDrainArgs {
        session_id,
        max_turns,
        dry_run,
        json,
    }))
}

fn parse_agents_service_args(args: Vec<String>) -> Result<AgentsAction, String> {
    let mut parsed = AgentsServiceArgs {
        kind: default_agents_service_kind(),
        out: None,
        bin: None,
        workdir: None,
        addr: "127.0.0.1:8765".to_string(),
        interval_ms: 1_000,
        budget: None,
    };
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--kind" if index + 1 < args.len() => {
                parsed.kind = match args[index + 1].as_str() {
                    "systemd" => AgentsServiceKind::Systemd,
                    "launchd" => AgentsServiceKind::Launchd,
                    "all" => AgentsServiceKind::All,
                    value => {
                        return Err(format!(
                            "agents service --kind must be systemd, launchd, or all; got {value}"
                        ));
                    }
                };
                index += 2;
            }
            "--kind" => return Err("agents service --kind requires a value".to_string()),
            "--out" if index + 1 < args.len() => {
                parsed.out = Some(args[index + 1].clone());
                index += 2;
            }
            "--out" => return Err("agents service --out requires a directory".to_string()),
            "--bin" if index + 1 < args.len() => {
                parsed.bin = Some(args[index + 1].clone());
                index += 2;
            }
            "--bin" => return Err("agents service --bin requires a path".to_string()),
            "--workdir" if index + 1 < args.len() => {
                parsed.workdir = Some(args[index + 1].clone());
                index += 2;
            }
            "--workdir" => return Err("agents service --workdir requires a path".to_string()),
            "--addr" if index + 1 < args.len() => {
                parsed.addr = args[index + 1].clone();
                index += 2;
            }
            "--addr" => return Err("agents service --addr requires a value".to_string()),
            "--interval-ms" if index + 1 < args.len() => {
                parsed.interval_ms = args[index + 1]
                    .parse::<u64>()
                    .map_err(|_| "agents service --interval-ms must be a number".to_string())?;
                index += 2;
            }
            "--interval-ms" => {
                return Err("agents service --interval-ms requires a value".to_string());
            }
            "--budget" if index + 1 < args.len() => {
                parsed.budget = Some(
                    args[index + 1]
                        .parse::<usize>()
                        .map_err(|_| "agents service --budget must be a number".to_string())?,
                );
                index += 2;
            }
            "--budget" => return Err("agents service --budget requires a value".to_string()),
            value => {
                return Err(format!(
                    "unknown flag for `agents service`: {value}; expected --kind|--out|--bin|--workdir|--addr|--interval-ms|--budget"
                ));
            }
        }
    }
    Ok(AgentsAction::Service(parsed))
}

fn parse_agents_service_doctor_args(args: Vec<String>) -> Result<AgentsAction, String> {
    let mut parsed = AgentsServiceDoctorArgs {
        kind: default_agents_service_kind(),
        out: None,
        bin: None,
        workdir: None,
        addr: "127.0.0.1:8765".to_string(),
        interval_ms: 1_000,
        budget: None,
        json: false,
    };
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--kind" if index + 1 < args.len() => {
                parsed.kind = match args[index + 1].as_str() {
                    "systemd" => AgentsServiceKind::Systemd,
                    "launchd" => AgentsServiceKind::Launchd,
                    "all" => AgentsServiceKind::All,
                    value => {
                        return Err(format!(
                            "agents service-doctor --kind must be systemd, launchd, or all; got {value}"
                        ));
                    }
                };
                index += 2;
            }
            "--kind" => return Err("agents service-doctor --kind requires a value".to_string()),
            "--out" if index + 1 < args.len() => {
                parsed.out = Some(args[index + 1].clone());
                index += 2;
            }
            "--out" => return Err("agents service-doctor --out requires a directory".to_string()),
            "--bin" if index + 1 < args.len() => {
                parsed.bin = Some(args[index + 1].clone());
                index += 2;
            }
            "--bin" => return Err("agents service-doctor --bin requires a path".to_string()),
            "--workdir" if index + 1 < args.len() => {
                parsed.workdir = Some(args[index + 1].clone());
                index += 2;
            }
            "--workdir" => {
                return Err("agents service-doctor --workdir requires a path".to_string())
            }
            "--addr" if index + 1 < args.len() => {
                parsed.addr = args[index + 1].clone();
                index += 2;
            }
            "--addr" => return Err("agents service-doctor --addr requires a value".to_string()),
            "--interval-ms" if index + 1 < args.len() => {
                parsed.interval_ms = args[index + 1].parse::<u64>().map_err(|_| {
                    "agents service-doctor --interval-ms must be a number".to_string()
                })?;
                index += 2;
            }
            "--interval-ms" => {
                return Err("agents service-doctor --interval-ms requires a value".to_string());
            }
            "--budget" if index + 1 < args.len() => {
                parsed.budget =
                    Some(args[index + 1].parse::<usize>().map_err(|_| {
                        "agents service-doctor --budget must be a number".to_string()
                    })?);
                index += 2;
            }
            "--budget" => return Err("agents service-doctor --budget requires a value".to_string()),
            "--json" => {
                parsed.json = true;
                index += 1;
            }
            value => {
                return Err(format!(
                    "unknown flag for `agents service-doctor`: {value}; expected --kind|--out|--bin|--workdir|--addr|--interval-ms|--budget|--json"
                ));
            }
        }
    }
    Ok(AgentsAction::ServiceDoctor(parsed))
}

fn parse_agents_service_smoke_args(args: Vec<String>) -> Result<AgentsAction, String> {
    let mut parsed = AgentsServiceSmokeArgs {
        bin: None,
        workdir: None,
        addr: "127.0.0.1:0".to_string(),
        timeout_ms: 5_000,
        json: false,
    };
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--bin" if index + 1 < args.len() => {
                parsed.bin = Some(args[index + 1].clone());
                index += 2;
            }
            "--bin" => return Err("agents service-smoke --bin requires a path".to_string()),
            "--workdir" if index + 1 < args.len() => {
                parsed.workdir = Some(args[index + 1].clone());
                index += 2;
            }
            "--workdir" => return Err("agents service-smoke --workdir requires a path".to_string()),
            "--addr" if index + 1 < args.len() => {
                parsed.addr = args[index + 1].clone();
                index += 2;
            }
            "--addr" => return Err("agents service-smoke --addr requires a value".to_string()),
            "--timeout-ms" if index + 1 < args.len() => {
                parsed.timeout_ms = args[index + 1].parse::<u64>().map_err(|_| {
                    "agents service-smoke --timeout-ms must be a number".to_string()
                })?;
                index += 2;
            }
            "--timeout-ms" => {
                return Err("agents service-smoke --timeout-ms requires a value".to_string());
            }
            "--json" => {
                parsed.json = true;
                index += 1;
            }
            value => {
                return Err(format!(
                    "unknown flag for `agents service-smoke`: {value}; expected --bin|--workdir|--addr|--timeout-ms|--json"
                ));
            }
        }
    }
    Ok(AgentsAction::ServiceSmoke(parsed))
}

fn default_agents_service_kind() -> AgentsServiceKind {
    if cfg!(target_os = "macos") {
        AgentsServiceKind::Launchd
    } else {
        AgentsServiceKind::Systemd
    }
}

fn parse_exec_args(args: Vec<String>) -> Result<ExecArgs, String> {
    let mut skill = None;
    let mut budget = None;
    let mut images = Vec::new();
    let mut json = false;
    let mut positional = Vec::new();
    let mut index = 0;

    while index < args.len() {
        match args[index].as_str() {
            "--" => {
                positional.extend(args.iter().skip(index + 1).cloned());
                break;
            }
            "--json" => {
                json = true;
                index += 1;
            }
            "--skill" if index + 1 < args.len() => {
                skill = Some(args[index + 1].clone());
                index += 2;
            }
            "--skill" => return Err("exec --skill requires a value".to_string()),
            "--budget" if index + 1 < args.len() => {
                budget = parse_budget_flag("exec", &args[index + 1])?;
                index += 2;
            }
            "--budget" => return Err("exec --budget requires a value".to_string()),
            "--image" | "-i" if index + 1 < args.len() => {
                images.extend(parse_image_flag_values(&args[index + 1]));
                index += 2;
            }
            "--image" | "-i" => return Err("exec --image requires a value".to_string()),
            other if other.starts_with("--") => {
                return Err(format!("unknown flag for `exec`: {other}"));
            }
            _ => {
                positional.push(args[index].clone());
                index += 1;
            }
        }
    }

    let task = positional.join(" ");
    if task.trim().is_empty() {
        return Err("exec requires a prompt or `-` to read stdin".to_string());
    }

    Ok(ExecArgs {
        task,
        skill,
        budget,
        images,
        json,
    })
}

fn parse_exec_resume_args(args: Vec<String>) -> Result<ExecResumeArgs, String> {
    let mut skill = None;
    let mut budget = None;
    let mut images = Vec::new();
    let mut json = false;
    let mut last = false;
    let mut positional = Vec::new();
    let mut index = 0;

    while index < args.len() {
        match args[index].as_str() {
            "--" => {
                positional.extend(args.iter().skip(index + 1).cloned());
                break;
            }
            "--json" => {
                json = true;
                index += 1;
            }
            "--last" => {
                last = true;
                index += 1;
            }
            "--skill" if index + 1 < args.len() => {
                skill = Some(args[index + 1].clone());
                index += 2;
            }
            "--skill" => return Err("exec resume --skill requires a value".to_string()),
            "--budget" if index + 1 < args.len() => {
                budget = parse_budget_flag("exec resume", &args[index + 1])?;
                index += 2;
            }
            "--budget" => return Err("exec resume --budget requires a value".to_string()),
            "--image" | "-i" if index + 1 < args.len() => {
                images.extend(parse_image_flag_values(&args[index + 1]));
                index += 2;
            }
            "--image" | "-i" => return Err("exec resume --image requires a value".to_string()),
            other if other.starts_with("--") => {
                return Err(format!("unknown flag for `exec resume`: {other}"));
            }
            _ => {
                positional.push(args[index].clone());
                index += 1;
            }
        }
    }

    let session = if positional
        .first()
        .map(|arg| looks_like_session_id(arg))
        .unwrap_or(false)
    {
        Some(positional.remove(0))
    } else {
        None
    };

    if last && session.is_some() {
        return Err("exec resume accepts either --last or SESSION_ID, not both".to_string());
    }

    let task = if positional.is_empty() {
        None
    } else {
        Some(positional.join(" "))
    };

    Ok(ExecResumeArgs {
        session,
        task,
        skill,
        budget,
        images,
        json,
    })
}

fn parse_image_flag_values(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .collect()
}

fn parse_budget_flag(context: &str, raw: &str) -> Result<Option<usize>, String> {
    let value = raw
        .parse::<usize>()
        .map_err(|_| format!("{context} --budget must be a number from 1 to 200"))?;
    if !(1..=200).contains(&value) {
        return Err(format!("{context} --budget must be between 1 and 200"));
    }
    Ok(Some(value))
}

fn looks_like_session_id(value: &str) -> bool {
    value.starts_with("session-")
}

fn parse_dogfood_subcommand(args: Vec<String>) -> Result<DogfoodAction, String> {
    let mut iter = args.into_iter();
    let action = iter
        .next()
        .ok_or_else(|| {
            "dogfood requires a sub-action: run|external-fixture|replay-benchmark|live-plan|live-run|report|export-benchmark|promote-benchmark"
                .to_string()
        })?;
    let rest: Vec<String> = iter.collect();
    match action.as_str() {
        "run" => parse_dogfood_run_args(rest).map(DogfoodAction::Run),
        "external-fixture" | "external-write-fixture" => {
            parse_dogfood_external_fixture_args(rest).map(DogfoodAction::ExternalFixture)
        }
        "replay-benchmark" | "replay-bench" => {
            Ok(DogfoodAction::ReplayBenchmark(parse_dogfood_replay_args(rest)))
        }
        "live-plan" | "plan-live" => {
            parse_dogfood_live_plan_args(rest).map(DogfoodAction::LivePlan)
        }
        "live-run" | "run-live" => parse_dogfood_live_run_args(rest).map(DogfoodAction::LiveRun),
        "report" => parse_dogfood_report_args(rest).map(DogfoodAction::Report),
        "export-benchmark" | "export-bench" => {
            Ok(DogfoodAction::ExportBenchmark(parse_dogfood_export_args(rest)))
        }
        "promote-benchmark" | "promote-bench" => {
            Ok(DogfoodAction::PromoteBenchmark(parse_dogfood_promote_args(
                rest,
            )))
        }
        other => Err(format!(
            "unknown dogfood sub-action `{other}`; expected run|external-fixture|replay-benchmark|live-plan|live-run|report|export-benchmark|promote-benchmark"
        )),
    }
}

fn parse_dogfood_run_args(args: Vec<String>) -> Result<DogfoodRunArgs, String> {
    let mut from_benchmark = None;
    let mut benchmark_manifest = None;
    let mut skill = None;
    let mut budget = None;
    let mut workdir = None;
    let mut isolate_workdir = false;
    let mut outcome = None;
    let mut manual_intervention = false;
    let mut benchmark_gate = false;
    let mut notes = None;
    let mut positional = Vec::new();
    let mut index = 0;

    while index < args.len() {
        match args[index].as_str() {
            "--from-benchmark" if index + 1 < args.len() => {
                from_benchmark = Some(args[index + 1].clone());
                index += 2;
                continue;
            }
            "--manifest" if index + 1 < args.len() => {
                benchmark_manifest = Some(args[index + 1].clone());
                index += 2;
                continue;
            }
            "--skill" if index + 1 < args.len() => {
                skill = Some(args[index + 1].clone());
                index += 2;
                continue;
            }
            "--budget" if index + 1 < args.len() => {
                if let Ok(n) = args[index + 1].parse::<usize>() {
                    if (1..=200).contains(&n) {
                        budget = Some(n);
                    }
                }
                index += 2;
                continue;
            }
            "--workdir" if index + 1 < args.len() => {
                workdir = Some(args[index + 1].clone());
                index += 2;
                continue;
            }
            "--isolate-workdir" => {
                isolate_workdir = true;
                index += 1;
                continue;
            }
            "--outcome" if index + 1 < args.len() => {
                outcome = parse_dogfood_outcome(&args[index + 1]);
                if outcome.is_none() {
                    return Err(format!(
                        "invalid dogfood outcome `{}`; expected success|failed|stuck|manual",
                        args[index + 1]
                    ));
                }
                index += 2;
                continue;
            }
            "--manual-intervention" => {
                manual_intervention = true;
                index += 1;
                continue;
            }
            "--benchmark-gate" => {
                benchmark_gate = true;
                index += 1;
                continue;
            }
            "--notes" if index + 1 < args.len() => {
                notes = Some(args[index + 1].clone());
                index += 2;
                continue;
            }
            _ => {}
        }

        positional.push(args[index].clone());
        index += 1;
    }

    let task = positional.join(" ");
    if task.trim().is_empty() && from_benchmark.is_none() {
        return Err("dogfood run requires a task or --from-benchmark <case>".to_string());
    }
    if !task.trim().is_empty() && from_benchmark.is_some() {
        return Err(
            "dogfood run does not accept a free-form task together with --from-benchmark"
                .to_string(),
        );
    }

    Ok(DogfoodRunArgs {
        task,
        from_benchmark,
        benchmark_manifest,
        skill,
        budget,
        workdir,
        isolate_workdir,
        outcome,
        manual_intervention,
        benchmark_gate,
        notes,
    })
}

fn parse_dogfood_external_fixture_args(
    args: Vec<String>,
) -> Result<DogfoodExternalFixtureArgs, String> {
    let mut workdir = None;
    let mut budget = None;
    let mut benchmark_gate = false;
    let mut notes = None;
    let mut dry_run = false;
    let mut positional = Vec::new();
    let mut index = 0;

    while index < args.len() {
        match args[index].as_str() {
            "--workdir" if index + 1 < args.len() => {
                workdir = Some(args[index + 1].clone());
                index += 2;
                continue;
            }
            "--budget" if index + 1 < args.len() => {
                if let Ok(n) = args[index + 1].parse::<usize>() {
                    if (1..=200).contains(&n) {
                        budget = Some(n);
                    }
                }
                index += 2;
                continue;
            }
            "--benchmark-gate" => {
                benchmark_gate = true;
                index += 1;
                continue;
            }
            "--notes" if index + 1 < args.len() => {
                notes = Some(args[index + 1].clone());
                index += 2;
                continue;
            }
            "--dry-run" => {
                dry_run = true;
                index += 1;
                continue;
            }
            _ => {}
        }

        positional.push(args[index].clone());
        index += 1;
    }

    let task = positional.join(" ");
    if task.trim().is_empty() {
        return Err("dogfood external-fixture requires a task".to_string());
    }
    let Some(workdir) = workdir else {
        return Err("dogfood external-fixture requires --workdir <path>".to_string());
    };

    Ok(DogfoodExternalFixtureArgs {
        task,
        workdir,
        budget,
        benchmark_gate,
        notes,
        dry_run,
    })
}

fn parse_dogfood_replay_args(args: Vec<String>) -> DogfoodReplayArgs {
    let mut replay = DogfoodReplayArgs::default();
    let mut index = 0;

    while index < args.len() {
        match args[index].as_str() {
            "--manifest" if index + 1 < args.len() => {
                replay.manifest = Some(args[index + 1].clone());
                index += 2;
                continue;
            }
            "--category" if index + 1 < args.len() => {
                replay.category = Some(args[index + 1].clone());
                index += 2;
                continue;
            }
            "--limit" if index + 1 < args.len() => {
                if let Ok(limit) = args[index + 1].parse::<usize>() {
                    if (1..=200).contains(&limit) {
                        replay.limit = Some(limit);
                    }
                }
                index += 2;
                continue;
            }
            "--benchmark-gate" => {
                replay.benchmark_gate = true;
                index += 1;
                continue;
            }
            _ => {}
        }
        index += 1;
    }

    replay
}

fn parse_dogfood_live_plan_args(args: Vec<String>) -> Result<DogfoodLivePlanArgs, String> {
    let mut plan = DogfoodLivePlanArgs::default();
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--manifest" if index + 1 < args.len() => {
                plan.manifest = Some(args[index + 1].clone());
                index += 2;
                continue;
            }
            "--target-live-runs" if index + 1 < args.len() => {
                plan.target_live_runs = Some(parse_required_usize(
                    "--target-live-runs",
                    &args[index + 1],
                    1,
                    100_000,
                )?);
                index += 2;
                continue;
            }
            "--target-live-success-rate" if index + 1 < args.len() => {
                plan.target_live_success_rate = Some(parse_percent_arg(
                    "--target-live-success-rate",
                    &args[index + 1],
                )?);
                index += 2;
                continue;
            }
            "--target-category" if index + 1 < args.len() => {
                plan.target_categories
                    .push(parse_dogfood_category_requirement(&args[index + 1])?);
                index += 2;
                continue;
            }
            "--limit" if index + 1 < args.len() => {
                plan.limit = Some(parse_required_usize(
                    "--limit",
                    &args[index + 1],
                    1,
                    10_000,
                )?);
                index += 2;
                continue;
            }
            "--json" => {
                plan.json = true;
                index += 1;
                continue;
            }
            "--manifest"
            | "--target-live-runs"
            | "--target-live-success-rate"
            | "--target-category"
            | "--limit" => {
                return Err(format!("{} requires a value", args[index]));
            }
            other => return Err(format!("unknown dogfood live-plan flag `{other}`")),
        }
    }
    Ok(plan)
}

fn parse_dogfood_live_run_args(args: Vec<String>) -> Result<DogfoodLiveRunArgs, String> {
    let mut run = DogfoodLiveRunArgs::default();
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--manifest" if index + 1 < args.len() => {
                run.manifest = Some(args[index + 1].clone());
                index += 2;
                continue;
            }
            "--target-live-runs" if index + 1 < args.len() => {
                run.target_live_runs = Some(parse_required_usize(
                    "--target-live-runs",
                    &args[index + 1],
                    1,
                    100_000,
                )?);
                index += 2;
                continue;
            }
            "--target-live-success-rate" if index + 1 < args.len() => {
                run.target_live_success_rate = Some(parse_percent_arg(
                    "--target-live-success-rate",
                    &args[index + 1],
                )?);
                index += 2;
                continue;
            }
            "--target-category" if index + 1 < args.len() => {
                run.target_categories
                    .push(parse_dogfood_category_requirement(&args[index + 1])?);
                index += 2;
                continue;
            }
            "--category" if index + 1 < args.len() => {
                run.categories.push(args[index + 1].clone());
                index += 2;
                continue;
            }
            "--limit" if index + 1 < args.len() => {
                run.limit = Some(parse_required_usize(
                    "--limit",
                    &args[index + 1],
                    1,
                    10_000,
                )?);
                index += 2;
                continue;
            }
            "--execute" => {
                run.execute = true;
                index += 1;
                continue;
            }
            "--dry-run" => {
                run.execute = false;
                index += 1;
                continue;
            }
            "--benchmark-gate" => {
                run.benchmark_gate = true;
                index += 1;
                continue;
            }
            "--manifest"
            | "--target-live-runs"
            | "--target-live-success-rate"
            | "--target-category"
            | "--category"
            | "--limit" => {
                return Err(format!("{} requires a value", args[index]));
            }
            other => return Err(format!("unknown dogfood live-run flag `{other}`")),
        }
    }
    Ok(run)
}

fn parse_dogfood_report_args(args: Vec<String>) -> Result<DogfoodReportArgs, String> {
    let mut report = DogfoodReportArgs::default();
    let mut index = 0;

    while index < args.len() {
        match args[index].as_str() {
            "--out" if index + 1 < args.len() => {
                report.out = Some(args[index + 1].clone());
                index += 2;
                continue;
            }
            "--limit" if index + 1 < args.len() => {
                if let Ok(limit) = args[index + 1].parse::<usize>() {
                    if (1..=500).contains(&limit) {
                        report.limit = Some(limit);
                    }
                }
                index += 2;
                continue;
            }
            "--require-min-runs" if index + 1 < args.len() => {
                report.require_min_runs = Some(parse_required_usize(
                    "--require-min-runs",
                    &args[index + 1],
                    1,
                    100_000,
                )?);
                index += 2;
                continue;
            }
            "--require-success-rate" if index + 1 < args.len() => {
                report.require_success_rate = Some(parse_percent_arg(
                    "--require-success-rate",
                    &args[index + 1],
                )?);
                index += 2;
                continue;
            }
            "--require-live-runs" if index + 1 < args.len() => {
                report.require_live_runs = Some(parse_required_usize(
                    "--require-live-runs",
                    &args[index + 1],
                    1,
                    100_000,
                )?);
                index += 2;
                continue;
            }
            "--require-live-success-rate" if index + 1 < args.len() => {
                report.require_live_success_rate = Some(parse_percent_arg(
                    "--require-live-success-rate",
                    &args[index + 1],
                )?);
                index += 2;
                continue;
            }
            "--require-external-write-fixtures" if index + 1 < args.len() => {
                report.require_external_write_fixtures = Some(parse_required_usize(
                    "--require-external-write-fixtures",
                    &args[index + 1],
                    1,
                    100_000,
                )?);
                index += 2;
                continue;
            }
            "--require-recent-clean" if index + 1 < args.len() => {
                report.require_recent_clean = Some(parse_required_usize(
                    "--require-recent-clean",
                    &args[index + 1],
                    1,
                    100_000,
                )?);
                index += 2;
                continue;
            }
            "--require-category" if index + 1 < args.len() => {
                report
                    .require_categories
                    .push(parse_dogfood_category_requirement(&args[index + 1])?);
                index += 2;
                continue;
            }
            "--require-live-category" if index + 1 < args.len() => {
                report
                    .require_live_categories
                    .push(parse_dogfood_category_requirement(&args[index + 1])?);
                index += 2;
                continue;
            }
            "--require-min-runs"
            | "--require-success-rate"
            | "--require-live-runs"
            | "--require-live-success-rate"
            | "--require-external-write-fixtures"
            | "--require-recent-clean"
            | "--require-category"
            | "--require-live-category" => {
                return Err(format!("{} requires a value", args[index]));
            }
            _ => {}
        }
        index += 1;
    }

    Ok(report)
}

fn parse_required_usize(flag: &str, raw: &str, min: usize, max: usize) -> Result<usize, String> {
    let value = raw
        .parse::<usize>()
        .map_err(|_| format!("{flag} requires an integer between {min} and {max}"))?;
    if (min..=max).contains(&value) {
        Ok(value)
    } else {
        Err(format!(
            "{flag} requires an integer between {min} and {max}"
        ))
    }
}

fn parse_percent_arg(flag: &str, raw: &str) -> Result<f64, String> {
    let trimmed = raw.trim().trim_end_matches('%');
    let value = trimmed
        .parse::<f64>()
        .map_err(|_| format!("{flag} requires a percentage between 0 and 100"))?;
    if (0.0..=100.0).contains(&value) {
        Ok(value)
    } else {
        Err(format!("{flag} requires a percentage between 0 and 100"))
    }
}

fn parse_dogfood_category_requirement(raw: &str) -> Result<DogfoodCategoryRequirement, String> {
    let parts = raw.split(':').collect::<Vec<_>>();
    if parts.len() != 3 {
        return Err(
            "--require-category expects <category>:<min-runs>:<min-success-percent>".to_string(),
        );
    }
    let category = parts[0].trim();
    if category.is_empty() {
        return Err("--require-category requires a non-empty category".to_string());
    }
    Ok(DogfoodCategoryRequirement {
        category: category.to_string(),
        min_runs: parse_required_usize("--require-category min-runs", parts[1], 1, 100_000)?,
        min_success_percent: parse_percent_arg("--require-category min-success-percent", parts[2])?,
    })
}

fn parse_dogfood_export_args(args: Vec<String>) -> DogfoodExportArgs {
    let mut export = DogfoodExportArgs::default();
    let mut index = 0;

    while index < args.len() {
        match args[index].as_str() {
            "--out" if index + 1 < args.len() => {
                export.out = Some(args[index + 1].clone());
                index += 2;
                continue;
            }
            "--limit" if index + 1 < args.len() => {
                if let Ok(limit) = args[index + 1].parse::<usize>() {
                    if (1..=500).contains(&limit) {
                        export.limit = Some(limit);
                    }
                }
                index += 2;
                continue;
            }
            "--outcome" if index + 1 < args.len() => {
                export.outcome = parse_dogfood_outcome(&args[index + 1]);
                index += 2;
                continue;
            }
            _ => {}
        }
        index += 1;
    }

    export
}

fn parse_dogfood_promote_args(args: Vec<String>) -> DogfoodPromoteArgs {
    let mut promote = DogfoodPromoteArgs::default();
    let mut index = 0;

    while index < args.len() {
        match args[index].as_str() {
            "--manifest" if index + 1 < args.len() => {
                promote.manifest = Some(args[index + 1].clone());
                index += 2;
                continue;
            }
            "--limit" if index + 1 < args.len() => {
                if let Ok(limit) = args[index + 1].parse::<usize>() {
                    if (1..=500).contains(&limit) {
                        promote.limit = Some(limit);
                    }
                }
                index += 2;
                continue;
            }
            "--outcome" if index + 1 < args.len() => {
                promote.outcome = parse_dogfood_outcome(&args[index + 1]);
                index += 2;
                continue;
            }
            "--dry-run" => {
                promote.dry_run = true;
                index += 1;
                continue;
            }
            _ => {}
        }
        index += 1;
    }

    promote
}

fn parse_dogfood_outcome(raw: &str) -> Option<DogfoodOutcome> {
    match raw {
        "success" => Some(DogfoodOutcome::Success),
        "failed" => Some(DogfoodOutcome::Failed),
        "stuck" => Some(DogfoodOutcome::Stuck),
        "manual" => Some(DogfoodOutcome::Manual),
        _ => None,
    }
}

fn parse_common_flags(args: Vec<String>) -> (Option<String>, Vec<String>) {
    let (skill, _budget, positional) = parse_common_flags_extended(args);
    (skill, positional)
}

pub fn parse_common_flags_extended(
    args: Vec<String>,
) -> (Option<String>, Option<usize>, Vec<String>) {
    let mut skill = None;
    let mut budget: Option<usize> = None;
    let mut positional = Vec::new();
    let mut index = 0;

    while index < args.len() {
        if args[index] == "--skill" {
            if index + 1 < args.len() {
                skill = Some(args[index + 1].clone());
                index += 2;
                continue;
            }
        }
        if args[index] == "--budget" {
            if index + 1 < args.len() {
                if let Ok(n) = args[index + 1].parse::<usize>() {
                    if (1..=200).contains(&n) {
                        budget = Some(n);
                    }
                }
                index += 2;
                continue;
            }
        }

        positional.push(args[index].clone());
        index += 1;
    }

    (skill, budget, positional)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_pr_review_with_post_flag() {
        let args = vec!["review".to_string(), "42".to_string(), "--post".to_string()];
        let parsed = parse_pr_subcommand(args).unwrap();
        assert!(matches!(
            parsed,
            PrAction::Review {
                ref reference,
                post: true,
                out: None,
            } if reference == "42"
        ));
    }

    #[test]
    fn parses_restore_subcommands() {
        let snapshot = Cli::from_argv(vec![
            "restore".to_string(),
            "snapshot".to_string(),
            "--label".to_string(),
            "before turn".to_string(),
        ])
        .unwrap();
        assert!(matches!(
            snapshot.command,
            Some(Command::Restore(RestoreAction::Snapshot {
                label: Some(ref value)
            })) if value == "before turn"
        ));

        let positional_snapshot = Cli::from_argv(vec![
            "restore".to_string(),
            "snapshot".to_string(),
            "before".to_string(),
            "risky".to_string(),
            "turn".to_string(),
        ])
        .unwrap();
        assert!(matches!(
            positional_snapshot.command,
            Some(Command::Restore(RestoreAction::Snapshot {
                label: Some(ref value)
            })) if value == "before risky turn"
        ));

        let list = Cli::from_argv(vec![
            "restore".to_string(),
            "list".to_string(),
            "--limit".to_string(),
            "5".to_string(),
        ])
        .unwrap();
        assert!(matches!(
            list.command,
            Some(Command::Restore(RestoreAction::List { limit: 5 }))
        ));

        let show = Cli::from_argv(vec![
            "restore".to_string(),
            "show".to_string(),
            "snapshot-123".to_string(),
            "--patch".to_string(),
        ])
        .unwrap();
        assert!(matches!(
            show.command,
            Some(Command::Restore(RestoreAction::Show {
                ref id,
                patch: true
            })) if id == "snapshot-123"
        ));

        let revert = Cli::from_argv(vec![
            "restore".to_string(),
            "revert-turn".to_string(),
            "snapshot-123".to_string(),
            "--apply".to_string(),
        ])
        .unwrap();
        assert!(matches!(
            revert.command,
            Some(Command::Restore(RestoreAction::RevertTurn {
                ref id,
                apply: true
            })) if id == "snapshot-123"
        ));
    }

    #[test]
    fn parses_diagnostics_args() {
        let parsed = Cli::from_argv(vec![
            "diagnostics".to_string(),
            "--changed".to_string(),
            "src/lib.rs".to_string(),
        ])
        .unwrap();

        assert!(matches!(
            parsed.command,
            Some(Command::Diagnostics(DiagnosticsArgs {
                changed: true,
                watch: false,
                once: false,
                json: false,
                interval_ms: 1000,
                ref paths,
            })) if paths == &vec!["src/lib.rs".to_string()]
        ));

        let alias = Cli::from_argv(vec![
            "diag".to_string(),
            "--watch".to_string(),
            "--once".to_string(),
            "--interval-ms".to_string(),
            "250".to_string(),
            "src/main.rs".to_string(),
        ])
        .unwrap();
        assert!(matches!(
            alias.command,
            Some(Command::Diagnostics(DiagnosticsArgs {
                changed: false,
                watch: true,
                once: true,
                json: false,
                interval_ms: 250,
                ref paths,
            })) if paths == &vec!["src/main.rs".to_string()]
        ));

        let json = Cli::from_argv(vec![
            "diagnostics".to_string(),
            "--json".to_string(),
            "--changed".to_string(),
        ])
        .unwrap();
        assert!(matches!(
            json.command,
            Some(Command::Diagnostics(DiagnosticsArgs {
                changed: true,
                watch: false,
                once: false,
                json: true,
                interval_ms: 1000,
                ref paths,
            })) if paths.is_empty()
        ));
    }

    #[test]
    fn parses_tui_args() {
        let parsed = Cli::from_argv(vec![
            "tui".to_string(),
            "--demo".to_string(),
            "--once".to_string(),
        ])
        .unwrap();

        assert!(matches!(
            parsed.command,
            Some(Command::Tui(TuiArgs {
                demo: true,
                once: true,
                runtime_url: None,
                entrypoint_smoke: false,
                smoke_bin: None
            }))
        ));

        let parsed = Cli::from_argv(vec![
            "tui".to_string(),
            "--runtime-url".to_string(),
            "http://127.0.0.1:13000".to_string(),
        ])
        .unwrap();
        assert!(matches!(
            parsed.command,
            Some(Command::Tui(TuiArgs {
                demo: false,
                once: false,
                runtime_url: Some(ref url),
                entrypoint_smoke: false,
                smoke_bin: None
            })) if url == "http://127.0.0.1:13000"
        ));

        let parsed = Cli::from_argv(vec![
            "tui".to_string(),
            "--entrypoint-smoke".to_string(),
            "--smoke-bin".to_string(),
            "./target/release/deepseek".to_string(),
        ])
        .unwrap();
        assert!(matches!(
            parsed.command,
            Some(Command::Tui(TuiArgs {
                demo: false,
                once: false,
                runtime_url: None,
                entrypoint_smoke: true,
                smoke_bin: Some(ref bin)
            })) if bin == "./target/release/deepseek"
        ));

        let error = Cli::from_argv(vec![
            "tui".to_string(),
            "--entrypoint-smoke".to_string(),
            "--once".to_string(),
        ])
        .unwrap_err();
        assert!(error.contains("cannot be combined"));

        let error = Cli::from_argv(vec!["tui".to_string(), "--bad".to_string()]).unwrap_err();
        assert!(error.contains("unknown tui argument"));
    }

    #[test]
    fn parses_mcp_subcommands() {
        let list = Cli::from_argv(vec!["mcp".to_string()]).unwrap();
        assert!(matches!(list.command, Some(Command::Mcp(McpAction::List))));

        let doctor = Cli::from_argv(vec!["mcp".to_string(), "doctor".to_string()]).unwrap();
        assert!(matches!(
            doctor.command,
            Some(Command::Mcp(McpAction::Doctor))
        ));

        let tools = Cli::from_argv(vec![
            "mcp".to_string(),
            "tools".to_string(),
            "filesystem".to_string(),
        ])
        .unwrap();
        assert!(matches!(
            tools.command,
            Some(Command::Mcp(McpAction::Tools {
                server: Some(ref name)
            })) if name == "filesystem"
        ));

        let prompts = Cli::from_argv(vec![
            "mcp".to_string(),
            "prompts".to_string(),
            "github".to_string(),
        ])
        .unwrap();
        assert!(matches!(
            prompts.command,
            Some(Command::Mcp(McpAction::Prompts {
                server: Some(ref name)
            })) if name == "github"
        ));

        let resources = Cli::from_argv(vec![
            "mcp".to_string(),
            "resources".to_string(),
            "filesystem".to_string(),
        ])
        .unwrap();
        assert!(matches!(
            resources.command,
            Some(Command::Mcp(McpAction::Resources {
                server: Some(ref name)
            })) if name == "filesystem"
        ));

        let templates = Cli::from_argv(vec![
            "mcp".to_string(),
            "resource-templates".to_string(),
            "filesystem".to_string(),
        ])
        .unwrap();
        assert!(matches!(
            templates.command,
            Some(Command::Mcp(McpAction::ResourceTemplates {
                server: Some(ref name)
            })) if name == "filesystem"
        ));

        let call = Cli::from_argv(vec![
            "mcp".to_string(),
            "call".to_string(),
            "filesystem".to_string(),
            "read_file".to_string(),
            r#"{"path":"README.md"}"#.to_string(),
        ])
        .unwrap();
        assert!(matches!(
            call.command,
            Some(Command::Mcp(McpAction::Call {
                server,
                tool,
                arguments_json: Some(ref args)
            })) if server == "filesystem" && tool == "read_file" && args.contains("README.md")
        ));

        let prompt = Cli::from_argv(vec![
            "mcp".to_string(),
            "prompt".to_string(),
            "github".to_string(),
            "review_pr".to_string(),
            r#"{"number":42}"#.to_string(),
        ])
        .unwrap();
        assert!(matches!(
            prompt.command,
            Some(Command::Mcp(McpAction::Prompt {
                server,
                prompt,
                arguments_json: Some(ref args)
            })) if server == "github" && prompt == "review_pr" && args.contains("42")
        ));

        let resource = Cli::from_argv(vec![
            "mcp".to_string(),
            "resource".to_string(),
            "filesystem".to_string(),
            "file:///tmp/readme.md".to_string(),
        ])
        .unwrap();
        assert!(matches!(
            resource.command,
            Some(Command::Mcp(McpAction::Resource { server, uri }))
                if server == "filesystem" && uri == "file:///tmp/readme.md"
        ));

        let add = Cli::from_argv(vec![
            "mcp".to_string(),
            "add".to_string(),
            "filesystem".to_string(),
            "--command".to_string(),
            "npx".to_string(),
            "--arg".to_string(),
            "-y".to_string(),
            "--arg".to_string(),
            "@modelcontextprotocol/server-filesystem".to_string(),
            "--env".to_string(),
            "ROOT=.".to_string(),
            "--project".to_string(),
        ])
        .unwrap();
        assert!(matches!(
            add.command,
            Some(Command::Mcp(McpAction::Add {
                name,
                command: Some(ref command),
                args,
                scope: McpConfigScope::Project,
                ..
            })) if name == "filesystem"
                && command == "npx"
                && args == vec![
                    "-y".to_string(),
                    "@modelcontextprotocol/server-filesystem".to_string()
                ]
        ));

        let remove = Cli::from_argv(vec![
            "mcp".to_string(),
            "remove".to_string(),
            "filesystem".to_string(),
            "--project".to_string(),
        ])
        .unwrap();
        assert!(matches!(
            remove.command,
            Some(Command::Mcp(McpAction::Remove {
                name,
                scope: McpConfigScope::Project,
            })) if name == "filesystem"
        ));

        let init = Cli::from_argv(vec![
            "mcp".to_string(),
            "init".to_string(),
            "--force".to_string(),
        ])
        .unwrap();
        assert!(matches!(
            init.command,
            Some(Command::Mcp(McpAction::Init { force: true }))
        ));

        let add_self = Cli::from_argv(vec![
            "mcp".to_string(),
            "add-self".to_string(),
            "--name".to_string(),
            "deepseek-code".to_string(),
            "--workspace".to_string(),
            "/tmp/workspace".to_string(),
            "--project".to_string(),
        ])
        .unwrap();
        assert!(matches!(
            add_self.command,
            Some(Command::Mcp(McpAction::AddSelf {
                name,
                workspace: Some(ref path),
                scope: McpConfigScope::Project,
            })) if name == "deepseek-code" && path == "/tmp/workspace"
        ));
    }

    #[test]
    fn parses_dogfood_run_subcommand_with_flags() {
        let parsed = parse_dogfood_subcommand(vec![
            "run".to_string(),
            "--from-benchmark".to_string(),
            "fixture-pr-retry-validate-rust-mini".to_string(),
            "--manifest".to_string(),
            "benchmarks.txt".to_string(),
            "--skill".to_string(),
            "debug".to_string(),
            "--budget".to_string(),
            "12".to_string(),
            "--workdir".to_string(),
            "fixtures/rust-write-mini".to_string(),
            "--isolate-workdir".to_string(),
            "--outcome".to_string(),
            "manual".to_string(),
            "--manual-intervention".to_string(),
            "--benchmark-gate".to_string(),
            "--notes".to_string(),
            "needed one retry".to_string(),
        ])
        .unwrap();

        match parsed {
            DogfoodAction::Run(args) => {
                assert_eq!(
                    args.from_benchmark.as_deref(),
                    Some("fixture-pr-retry-validate-rust-mini")
                );
                assert_eq!(args.benchmark_manifest.as_deref(), Some("benchmarks.txt"));
                assert_eq!(args.skill.as_deref(), Some("debug"));
                assert_eq!(args.budget, Some(12));
                assert_eq!(args.workdir.as_deref(), Some("fixtures/rust-write-mini"));
                assert!(args.isolate_workdir);
                assert_eq!(args.outcome, Some(DogfoodOutcome::Manual));
                assert!(args.manual_intervention);
                assert!(args.benchmark_gate);
                assert_eq!(args.notes.as_deref(), Some("needed one retry"));
                assert_eq!(args.task, "");
            }
            DogfoodAction::ExternalFixture(_) => panic!("expected dogfood run args"),
            DogfoodAction::ReplayBenchmark(_) => panic!("expected dogfood run args"),
            DogfoodAction::LivePlan(_) => panic!("expected dogfood run args"),
            DogfoodAction::LiveRun(_) => panic!("expected dogfood run args"),
            DogfoodAction::Report(_) => panic!("expected dogfood run args"),
            DogfoodAction::ExportBenchmark(_) => panic!("expected dogfood run args"),
            DogfoodAction::PromoteBenchmark(_) => panic!("expected dogfood run args"),
        }
    }

    #[test]
    fn dogfood_run_requires_task_or_benchmark_case() {
        let error = parse_dogfood_subcommand(vec!["run".to_string()]).unwrap_err();
        assert!(error.contains("requires a task or --from-benchmark"));
    }

    #[test]
    fn parses_dogfood_external_fixture_subcommand() {
        let parsed = parse_dogfood_subcommand(vec![
            "external-fixture".to_string(),
            "--workdir".to_string(),
            "/tmp/external-repo".to_string(),
            "--budget".to_string(),
            "12".to_string(),
            "--notes".to_string(),
            "live write fixture".to_string(),
            "--dry-run".to_string(),
            "replace".to_string(),
            "`a".to_string(),
            "-".to_string(),
            "b`".to_string(),
            "with".to_string(),
            "`a".to_string(),
            "+".to_string(),
            "b`".to_string(),
            "in".to_string(),
            "src/lib.rs".to_string(),
            "and".to_string(),
            "validate".to_string(),
            "with".to_string(),
            "cargo".to_string(),
            "test".to_string(),
        ])
        .expect("parse should succeed");

        match parsed {
            DogfoodAction::ExternalFixture(args) => {
                assert_eq!(args.workdir, "/tmp/external-repo");
                assert_eq!(args.budget, Some(12));
                assert!(args.dry_run);
                assert_eq!(args.notes.as_deref(), Some("live write fixture"));
                assert!(args.task.contains("validate with cargo test"));
            }
            DogfoodAction::Run(_) => panic!("expected external fixture args"),
            DogfoodAction::ReplayBenchmark(_) => panic!("expected external fixture args"),
            DogfoodAction::LivePlan(_) => panic!("expected external fixture args"),
            DogfoodAction::LiveRun(_) => panic!("expected external fixture args"),
            DogfoodAction::Report(_) => panic!("expected external fixture args"),
            DogfoodAction::ExportBenchmark(_) => panic!("expected external fixture args"),
            DogfoodAction::PromoteBenchmark(_) => panic!("expected external fixture args"),
        }
    }

    #[test]
    fn parses_dogfood_report_subcommand() {
        let parsed = parse_dogfood_subcommand(vec![
            "report".to_string(),
            "--out".to_string(),
            "dogfood.md".to_string(),
            "--limit".to_string(),
            "50".to_string(),
            "--require-min-runs".to_string(),
            "100".to_string(),
            "--require-success-rate".to_string(),
            "90%".to_string(),
            "--require-live-runs".to_string(),
            "80".to_string(),
            "--require-live-success-rate".to_string(),
            "92".to_string(),
            "--require-external-write-fixtures".to_string(),
            "3".to_string(),
            "--require-recent-clean".to_string(),
            "20".to_string(),
            "--require-category".to_string(),
            "write_validate:25:90".to_string(),
            "--require-live-category".to_string(),
            "pr_workflow:25:90".to_string(),
        ])
        .unwrap();

        match parsed {
            DogfoodAction::Report(args) => {
                assert_eq!(args.out.as_deref(), Some("dogfood.md"));
                assert_eq!(args.limit, Some(50));
                assert_eq!(args.require_min_runs, Some(100));
                assert_eq!(args.require_success_rate, Some(90.0));
                assert_eq!(args.require_live_runs, Some(80));
                assert_eq!(args.require_live_success_rate, Some(92.0));
                assert_eq!(args.require_external_write_fixtures, Some(3));
                assert_eq!(args.require_recent_clean, Some(20));
                assert_eq!(args.require_categories.len(), 1);
                assert_eq!(args.require_categories[0].category, "write_validate");
                assert_eq!(args.require_categories[0].min_runs, 25);
                assert_eq!(args.require_categories[0].min_success_percent, 90.0);
                assert_eq!(args.require_live_categories.len(), 1);
                assert_eq!(args.require_live_categories[0].category, "pr_workflow");
                assert_eq!(args.require_live_categories[0].min_runs, 25);
                assert_eq!(args.require_live_categories[0].min_success_percent, 90.0);
            }
            DogfoodAction::Run(_) => panic!("expected dogfood report args"),
            DogfoodAction::ExternalFixture(_) => panic!("expected dogfood report args"),
            DogfoodAction::ReplayBenchmark(_) => panic!("expected dogfood report args"),
            DogfoodAction::LivePlan(_) => panic!("expected dogfood report args"),
            DogfoodAction::LiveRun(_) => panic!("expected dogfood report args"),
            DogfoodAction::ExportBenchmark(_) => panic!("expected dogfood report args"),
            DogfoodAction::PromoteBenchmark(_) => panic!("expected dogfood report args"),
        }
    }

    #[test]
    fn dogfood_report_rejects_invalid_evidence_gate() {
        let error = parse_dogfood_subcommand(vec![
            "report".to_string(),
            "--require-category".to_string(),
            "write_validate:0:90".to_string(),
        ])
        .unwrap_err();
        assert!(error.contains("--require-category min-runs"));
    }

    #[test]
    fn parses_dogfood_replay_benchmark_subcommand() {
        let parsed = parse_dogfood_subcommand(vec![
            "replay-benchmark".to_string(),
            "--manifest".to_string(),
            "benchmarks.txt".to_string(),
            "--category".to_string(),
            "pr_workflow".to_string(),
            "--limit".to_string(),
            "3".to_string(),
            "--benchmark-gate".to_string(),
        ])
        .unwrap();

        match parsed {
            DogfoodAction::ReplayBenchmark(args) => {
                assert_eq!(args.manifest.as_deref(), Some("benchmarks.txt"));
                assert_eq!(args.category.as_deref(), Some("pr_workflow"));
                assert_eq!(args.limit, Some(3));
                assert!(args.benchmark_gate);
            }
            DogfoodAction::Run(_) => panic!("expected replay args"),
            DogfoodAction::ExternalFixture(_) => panic!("expected replay args"),
            DogfoodAction::LivePlan(_) => panic!("expected replay args"),
            DogfoodAction::LiveRun(_) => panic!("expected replay args"),
            DogfoodAction::Report(_) => panic!("expected replay args"),
            DogfoodAction::ExportBenchmark(_) => panic!("expected replay args"),
            DogfoodAction::PromoteBenchmark(_) => panic!("expected replay args"),
        }
    }

    #[test]
    fn parses_dogfood_live_plan_subcommand() {
        let parsed = parse_dogfood_subcommand(vec![
            "live-plan".to_string(),
            "--manifest".to_string(),
            "benchmarks.txt".to_string(),
            "--target-live-runs".to_string(),
            "100".to_string(),
            "--target-live-success-rate".to_string(),
            "90".to_string(),
            "--target-category".to_string(),
            "write_validate:25:90".to_string(),
            "--limit".to_string(),
            "12".to_string(),
            "--json".to_string(),
        ])
        .unwrap();

        match parsed {
            DogfoodAction::LivePlan(args) => {
                assert_eq!(args.manifest.as_deref(), Some("benchmarks.txt"));
                assert_eq!(args.target_live_runs, Some(100));
                assert_eq!(args.target_live_success_rate, Some(90.0));
                assert_eq!(args.target_categories.len(), 1);
                assert_eq!(args.target_categories[0].category, "write_validate");
                assert_eq!(args.target_categories[0].min_runs, 25);
                assert_eq!(args.target_categories[0].min_success_percent, 90.0);
                assert_eq!(args.limit, Some(12));
                assert!(args.json);
            }
            DogfoodAction::Run(_) => panic!("expected live plan args"),
            DogfoodAction::ExternalFixture(_) => panic!("expected live plan args"),
            DogfoodAction::ReplayBenchmark(_) => panic!("expected live plan args"),
            DogfoodAction::LiveRun(_) => panic!("expected live plan args"),
            DogfoodAction::Report(_) => panic!("expected live plan args"),
            DogfoodAction::ExportBenchmark(_) => panic!("expected live plan args"),
            DogfoodAction::PromoteBenchmark(_) => panic!("expected live plan args"),
        }
    }

    #[test]
    fn parses_dogfood_live_run_subcommand() {
        let parsed = parse_dogfood_subcommand(vec![
            "live-run".to_string(),
            "--manifest".to_string(),
            "benchmarks.txt".to_string(),
            "--target-live-runs".to_string(),
            "100".to_string(),
            "--target-live-success-rate".to_string(),
            "90".to_string(),
            "--target-category".to_string(),
            "write_validate:25:90".to_string(),
            "--category".to_string(),
            "write_validate".to_string(),
            "--limit".to_string(),
            "2".to_string(),
            "--execute".to_string(),
            "--benchmark-gate".to_string(),
        ])
        .unwrap();

        match parsed {
            DogfoodAction::LiveRun(args) => {
                assert_eq!(args.manifest.as_deref(), Some("benchmarks.txt"));
                assert_eq!(args.target_live_runs, Some(100));
                assert_eq!(args.target_live_success_rate, Some(90.0));
                assert_eq!(args.target_categories.len(), 1);
                assert_eq!(args.target_categories[0].category, "write_validate");
                assert_eq!(args.categories, vec!["write_validate".to_string()]);
                assert_eq!(args.limit, Some(2));
                assert!(args.execute);
                assert!(args.benchmark_gate);
            }
            DogfoodAction::Run(_) => panic!("expected live run args"),
            DogfoodAction::ExternalFixture(_) => panic!("expected live run args"),
            DogfoodAction::ReplayBenchmark(_) => panic!("expected live run args"),
            DogfoodAction::LivePlan(_) => panic!("expected live run args"),
            DogfoodAction::Report(_) => panic!("expected live run args"),
            DogfoodAction::ExportBenchmark(_) => panic!("expected live run args"),
            DogfoodAction::PromoteBenchmark(_) => panic!("expected live run args"),
        }
    }

    #[test]
    fn parses_dogfood_export_benchmark_subcommand() {
        let parsed = parse_dogfood_subcommand(vec![
            "export-benchmark".to_string(),
            "--out".to_string(),
            "dogfood-seeds.txt".to_string(),
            "--limit".to_string(),
            "5".to_string(),
            "--outcome".to_string(),
            "stuck".to_string(),
        ])
        .unwrap();
        match parsed {
            DogfoodAction::ExportBenchmark(args) => {
                assert_eq!(args.out.as_deref(), Some("dogfood-seeds.txt"));
                assert_eq!(args.limit, Some(5));
                assert_eq!(args.outcome, Some(DogfoodOutcome::Stuck));
            }
            _ => panic!("expected dogfood export args"),
        }
    }

    #[test]
    fn parses_dogfood_promote_benchmark_subcommand() {
        let parsed = parse_dogfood_subcommand(vec![
            "promote-benchmark".to_string(),
            "--manifest".to_string(),
            ".dscode/benchmarks.txt".to_string(),
            "--limit".to_string(),
            "3".to_string(),
            "--outcome".to_string(),
            "failed".to_string(),
            "--dry-run".to_string(),
        ])
        .unwrap();
        match parsed {
            DogfoodAction::PromoteBenchmark(args) => {
                assert_eq!(args.manifest.as_deref(), Some(".dscode/benchmarks.txt"));
                assert_eq!(args.limit, Some(3));
                assert_eq!(args.outcome, Some(DogfoodOutcome::Failed));
                assert!(args.dry_run);
            }
            _ => panic!("expected dogfood promote args"),
        }
    }

    #[test]
    fn parses_pr_fix_with_job_flag() {
        let args = vec![
            "fix".to_string(),
            "owner/repo#7".to_string(),
            "--job".to_string(),
            "test-rust".to_string(),
            "--benchmark-gate".to_string(),
        ];
        let parsed = parse_pr_subcommand(args).unwrap();
        match parsed {
            PrAction::Fix {
                reference,
                job,
                benchmark_gate,
            } => {
                assert_eq!(reference, "owner/repo#7");
                assert_eq!(job.as_deref(), Some("test-rust"));
                assert!(benchmark_gate);
            }
            _ => panic!("expected fix"),
        }
    }

    #[test]
    fn parses_pr_live_status_with_require_write_flag() {
        let parsed = parse_pr_subcommand(vec![
            "live-status".to_string(),
            "owner/repo#42".to_string(),
            "--require-write".to_string(),
        ])
        .unwrap();
        match parsed {
            PrAction::LiveStatus {
                reference,
                require_write,
                json,
            } => {
                assert_eq!(reference, "owner/repo#42");
                assert!(require_write);
                assert!(!json);
            }
            _ => panic!("expected pr live-status args"),
        }
    }

    #[test]
    fn parses_pr_live_status_with_json_flag() {
        let parsed = parse_pr_subcommand(vec![
            "live-status".to_string(),
            "owner/repo#42".to_string(),
            "--json".to_string(),
        ])
        .unwrap();
        match parsed {
            PrAction::LiveStatus {
                reference,
                require_write,
                json,
            } => {
                assert_eq!(reference, "owner/repo#42");
                assert!(!require_write);
                assert!(json);
            }
            _ => panic!("expected pr live-status args"),
        }
    }

    #[test]
    fn parses_pr_patch_with_commit_flag() {
        let args = vec![
            "patch".to_string(),
            "5".to_string(),
            "--commit".to_string(),
            "--benchmark-gate".to_string(),
        ];
        let parsed = parse_pr_subcommand(args).unwrap();
        assert!(matches!(
            parsed,
            PrAction::Patch {
                commit: true,
                benchmark_gate: true,
                ref reference,
            } if reference == "5"
        ));
    }

    #[test]
    fn rejects_unknown_pr_subaction() {
        let args = vec!["delete".to_string(), "5".to_string()];
        assert!(parse_pr_subcommand(args).is_err());
    }

    #[test]
    fn cli_from_argv_routes_pr_subcommand_to_command_pr() {
        let argv = vec![
            "pr".to_string(),
            "review".to_string(),
            "42".to_string(),
            "--post".to_string(),
        ];
        let cli = Cli::from_argv(argv).expect("parse should succeed");
        match cli.command {
            Some(Command::Pr(PrAction::Review {
                reference,
                post,
                out: _,
            })) => {
                assert_eq!(reference, "42");
                assert!(post);
            }
            other => panic!("expected Command::Pr(Review), got {:?}", other),
        }
    }

    #[test]
    fn cli_from_argv_propagates_pr_parse_error() {
        let argv = vec!["pr".to_string(), "delete".to_string(), "5".to_string()];
        let result = Cli::from_argv(argv);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("unknown pr sub-action"));
    }

    #[test]
    fn cli_from_argv_parses_global_help_flag() {
        let cli = Cli::from_argv(vec!["--help".to_string()]).expect("parse should succeed");
        assert!(matches!(
            cli.command,
            Some(Command::Help(HelpArgs { ref topics })) if topics.is_empty()
        ));
    }

    #[test]
    fn cli_from_argv_parses_help_command_topics() {
        let cli = Cli::from_argv(vec![
            "help".to_string(),
            "dogfood".to_string(),
            "live-plan".to_string(),
        ])
        .expect("parse should succeed");
        assert!(matches!(
            cli.command,
            Some(Command::Help(HelpArgs { ref topics }))
                if topics == &vec!["dogfood".to_string(), "live-plan".to_string()]
        ));
    }

    #[test]
    fn cli_from_argv_routes_subcommand_help_without_running_subcommand_parser() {
        let cli = Cli::from_argv(vec![
            "dogfood".to_string(),
            "replay-benchmark".to_string(),
            "--help".to_string(),
        ])
        .expect("parse should succeed");
        assert!(matches!(
            cli.command,
            Some(Command::Help(HelpArgs { ref topics }))
                if topics == &vec!["dogfood".to_string(), "replay-benchmark".to_string()]
        ));
    }

    #[test]
    fn cli_from_argv_falls_back_to_chat_for_unknown_first_arg() {
        let argv = vec!["explore".to_string(), "the".to_string(), "repo".to_string()];
        let cli = Cli::from_argv(argv).expect("parse should succeed");
        match cli.command {
            Some(Command::Chat(args)) => {
                assert_eq!(args.task.as_deref(), Some("explore the repo"));
            }
            other => panic!("expected Command::Chat, got {:?}", other),
        }
    }

    #[test]
    fn cli_from_argv_defaults_to_chat_when_no_args_are_provided() {
        let cli = Cli::from_argv(Vec::new()).expect("parse should succeed");
        assert!(matches!(cli.command, Some(Command::Chat(_))));
    }

    #[test]
    fn cli_from_argv_defaults_to_tui_when_no_args_have_terminal() {
        let cli = Cli::from_argv_with_terminal(
            Vec::new(),
            TerminalContext {
                stdin_tty: true,
                stdout_tty: true,
            },
        )
        .expect("parse should succeed");
        assert!(matches!(
            cli.command,
            Some(Command::Tui(TuiArgs {
                demo: false,
                once: false,
                runtime_url: None,
                entrypoint_smoke: false,
                smoke_bin: None
            }))
        ));
    }

    #[test]
    fn cli_from_argv_keeps_no_args_repl_without_full_terminal() {
        for terminal in [
            TerminalContext {
                stdin_tty: false,
                stdout_tty: true,
            },
            TerminalContext {
                stdin_tty: true,
                stdout_tty: false,
            },
            TerminalContext {
                stdin_tty: false,
                stdout_tty: false,
            },
        ] {
            let cli =
                Cli::from_argv_with_terminal(Vec::new(), terminal).expect("parse should succeed");
            assert!(matches!(cli.command, Some(Command::Chat(_))));
        }
    }

    #[test]
    fn cli_from_argv_routes_explicit_chat_aliases_to_chat_command() {
        for alias in ["chat", "repl", "interactive"] {
            let cli = Cli::from_argv(vec![alias.to_string()]).expect("parse should succeed");
            assert!(
                matches!(cli.command, Some(Command::Chat(_))),
                "alias: {alias}"
            );
        }
    }

    #[test]
    fn cli_from_argv_parses_skill_on_explicit_chat_alias() {
        let cli = Cli::from_argv(vec![
            "chat".to_string(),
            "--skill".to_string(),
            "debug".to_string(),
        ])
        .expect("parse should succeed");
        match cli.command {
            Some(Command::Chat(args)) => {
                assert_eq!(args.skill.as_deref(), Some("debug"));
            }
            other => panic!("expected Command::Chat, got {:?}", other),
        }
    }

    #[test]
    fn cli_from_argv_routes_benchmark_subcommand() {
        let argv = vec![
            "benchmark".to_string(),
            "--manifest".to_string(),
            "bench.txt".to_string(),
            "--out".to_string(),
            "report.md".to_string(),
            "--accept-live-baseline".to_string(),
        ];
        let cli = Cli::from_argv(argv).expect("parse should succeed");
        match cli.command {
            Some(Command::Benchmark(args)) => {
                assert_eq!(args.manifest.as_deref(), Some("bench.txt"));
                assert_eq!(args.out.as_deref(), Some("report.md"));
                assert!(args.accept_live_baseline);
            }
            other => panic!("expected Command::Benchmark, got {:?}", other),
        }
    }

    #[test]
    fn cli_from_argv_routes_run_subcommand_with_benchmark_gate() {
        let argv = vec![
            "run".to_string(),
            "--skill".to_string(),
            "research".to_string(),
            "--budget".to_string(),
            "7".to_string(),
            "--benchmark-gate".to_string(),
            "inspect".to_string(),
            "repo".to_string(),
        ];
        let cli = Cli::from_argv(argv).expect("parse should succeed");
        match cli.command {
            Some(Command::Run(args)) => {
                assert_eq!(args.skill.as_deref(), Some("research"));
                assert_eq!(args.budget, Some(7));
                assert!(args.benchmark_gate);
                assert_eq!(args.task, "inspect".to_string());
            }
            other => panic!("expected Command::Run, got {:?}", other),
        }
    }

    #[test]
    fn cli_from_argv_routes_exec_json_stdin() {
        let cli = Cli::from_argv(vec![
            "exec".to_string(),
            "--json".to_string(),
            "--skill".to_string(),
            "debug".to_string(),
            "--budget".to_string(),
            "7".to_string(),
            "--image".to_string(),
            "a.png,b.jpg".to_string(),
            "-".to_string(),
        ])
        .expect("parse should succeed");

        match cli.command {
            Some(Command::Exec(ExecAction::Run(args))) => {
                assert!(args.json);
                assert_eq!(args.skill.as_deref(), Some("debug"));
                assert_eq!(args.budget, Some(7));
                assert_eq!(args.images, vec!["a.png", "b.jpg"]);
                assert_eq!(args.task, "-");
            }
            other => panic!("expected Command::Exec(Run), got {other:?}"),
        }
    }

    #[test]
    fn cli_from_argv_routes_exec_resume_last_with_followup() {
        let cli = Cli::from_argv(vec![
            "exec".to_string(),
            "resume".to_string(),
            "--last".to_string(),
            "--json".to_string(),
            "Fix".to_string(),
            "the".to_string(),
            "tests".to_string(),
        ])
        .expect("parse should succeed");

        match cli.command {
            Some(Command::Exec(ExecAction::Resume(args))) => {
                assert!(args.json);
                assert_eq!(args.session, None);
                assert_eq!(args.task.as_deref(), Some("Fix the tests"));
            }
            other => panic!("expected Command::Exec(Resume), got {other:?}"),
        }
    }

    #[test]
    fn cli_from_argv_routes_exec_resume_with_session() {
        let cli = Cli::from_argv(vec![
            "exec".to_string(),
            "resume".to_string(),
            "session-123".to_string(),
            "--budget".to_string(),
            "12".to_string(),
            "-i".to_string(),
            "screen.png".to_string(),
            "-".to_string(),
        ])
        .expect("parse should succeed");

        match cli.command {
            Some(Command::Exec(ExecAction::Resume(args))) => {
                assert_eq!(args.session.as_deref(), Some("session-123"));
                assert_eq!(args.budget, Some(12));
                assert_eq!(args.images, vec!["screen.png"]);
                assert_eq!(args.task.as_deref(), Some("-"));
            }
            other => panic!("expected Command::Exec(Resume), got {other:?}"),
        }
    }

    #[test]
    fn cli_from_argv_rejects_exec_without_prompt() {
        let error = Cli::from_argv(vec!["exec".to_string()]).expect_err("parse should fail");
        assert!(error.contains("requires a prompt"));
    }

    #[test]
    fn cli_from_argv_routes_agents_subcommands() {
        let list = Cli::from_argv(vec!["agents".to_string()]).expect("parse should succeed");
        assert!(matches!(
            list.command,
            Some(Command::Agents(AgentsAction::List))
        ));

        let show = Cli::from_argv(vec![
            "agents".to_string(),
            "show".to_string(),
            "reviewer".to_string(),
        ])
        .expect("parse should succeed");
        assert!(matches!(
            show.command,
            Some(Command::Agents(AgentsAction::Show { ref name })) if name == "reviewer"
        ));

        let validate = Cli::from_argv(vec![
            "agents".to_string(),
            "validate".to_string(),
            ".dscode/agents/reviewer.md".to_string(),
        ])
        .expect("parse should succeed");
        assert!(matches!(
            validate.command,
            Some(Command::Agents(AgentsAction::Validate {
                path: Some(ref path)
            })) if path == ".dscode/agents/reviewer.md"
        ));

        let run_task = Cli::from_argv(vec![
            "agents".to_string(),
            "run-task".to_string(),
            "--budget".to_string(),
            "7".to_string(),
            "--json".to_string(),
            "task-123".to_string(),
        ])
        .expect("parse should succeed");
        assert!(matches!(
            run_task.command,
            Some(Command::Agents(AgentsAction::RunTask {
                ref id,
                budget: Some(7),
                json: true
            })) if id == "task-123"
        ));

        let daemon = Cli::from_argv(vec![
            "agents".to_string(),
            "daemon".to_string(),
            "--budget".to_string(),
            "3".to_string(),
            "--interval-ms".to_string(),
            "250".to_string(),
            "--once".to_string(),
            "--json".to_string(),
        ])
        .expect("parse should succeed");
        assert!(matches!(
            daemon.command,
            Some(Command::Agents(AgentsAction::Daemon {
                budget: Some(3),
                interval_ms: 250,
                once: true,
                json: true,
            }))
        ));

        let rlm_status = Cli::from_argv(vec![
            "agents".to_string(),
            "rlm-status".to_string(),
            "live.1".to_string(),
            "--limit".to_string(),
            "5".to_string(),
            "--json".to_string(),
        ])
        .expect("parse should succeed");
        assert!(matches!(
            rlm_status.command,
            Some(Command::Agents(AgentsAction::RlmStatus(AgentsRlmStatusArgs {
                session_id: Some(ref id),
                limit: Some(5),
                json: true,
            }))) if id == "live.1"
        ));

        let rlm_events = Cli::from_argv(vec![
            "agents".to_string(),
            "rlm-events".to_string(),
            "live.1".to_string(),
            "--since-seq".to_string(),
            "7".to_string(),
            "--limit".to_string(),
            "3".to_string(),
        ])
        .expect("parse should succeed");
        assert!(matches!(
            rlm_events.command,
            Some(Command::Agents(AgentsAction::RlmEvents(AgentsRlmEventsArgs {
                ref session_id,
                cursor: Some(7),
                limit: Some(3),
                json: false,
            }))) if session_id == "live.1"
        ));

        let rlm_wait = Cli::from_argv(vec![
            "agents".to_string(),
            "rlm-wait".to_string(),
            "live.1".to_string(),
            "--cursor".to_string(),
            "9".to_string(),
            "--timeout-ms".to_string(),
            "2500".to_string(),
            "--poll-interval-ms".to_string(),
            "50".to_string(),
            "--json".to_string(),
        ])
        .expect("parse should succeed");
        assert!(matches!(
            rlm_wait.command,
            Some(Command::Agents(AgentsAction::RlmWait(AgentsRlmWaitArgs {
                ref session_id,
                cursor: Some(9),
                limit: None,
                timeout_ms: Some(2500),
                poll_interval_ms: Some(50),
                json: true,
            }))) if session_id == "live.1"
        ));

        let rlm_cancel = Cli::from_argv(vec![
            "agents".to_string(),
            "rlm-cancel".to_string(),
            "live.1".to_string(),
            "task-1".to_string(),
            "--force".to_string(),
            "--reason".to_string(),
            "operator stop".to_string(),
            "--json".to_string(),
        ])
        .expect("parse should succeed");
        assert!(matches!(
            rlm_cancel.command,
            Some(Command::Agents(AgentsAction::RlmCancel(AgentsRlmCancelArgs {
                ref session_id,
                task_id: Some(ref task_id),
                all: false,
                force: true,
                reason: Some(ref reason),
                json: true,
            }))) if session_id == "live.1" && task_id == "task-1" && reason == "operator stop"
        ));

        let rlm_recover = Cli::from_argv(vec![
            "agents".to_string(),
            "rlm-recover".to_string(),
            "--all".to_string(),
            "--mode".to_string(),
            "fail".to_string(),
            "--dry-run".to_string(),
            "--force".to_string(),
            "--limit".to_string(),
            "8".to_string(),
            "--reason".to_string(),
            "takeover".to_string(),
        ])
        .expect("parse should succeed");
        assert!(matches!(
            rlm_recover.command,
            Some(Command::Agents(AgentsAction::RlmRecover(AgentsRlmRecoverArgs {
                session_id: None,
                all: true,
                mode: Some(ref mode),
                dry_run: true,
                force: true,
                limit: Some(8),
                reason: Some(ref reason),
                json: false,
            }))) if mode == "fail" && reason == "takeover"
        ));

        let rlm_stop = Cli::from_argv(vec![
            "agents".to_string(),
            "rlm-stop".to_string(),
            "live.1".to_string(),
            "--reason".to_string(),
            "done".to_string(),
        ])
        .expect("parse should succeed");
        assert!(matches!(
            rlm_stop.command,
            Some(Command::Agents(AgentsAction::RlmStop(AgentsRlmStopArgs {
                ref session_id,
                reason: Some(ref reason),
                json: false,
            }))) if session_id == "live.1" && reason == "done"
        ));

        let rlm_run_next = Cli::from_argv(vec![
            "agents".to_string(),
            "rlm-run-next".to_string(),
            "live.1".to_string(),
            "task-2".to_string(),
            "--dry-run".to_string(),
        ])
        .expect("parse should succeed");
        assert!(matches!(
            rlm_run_next.command,
            Some(Command::Agents(AgentsAction::RlmRunNext(AgentsRlmRunNextArgs {
                ref session_id,
                task_id: Some(ref task_id),
                dry_run: true,
                json: false,
            }))) if session_id == "live.1" && task_id == "task-2"
        ));

        let rlm_drain = Cli::from_argv(vec![
            "agents".to_string(),
            "rlm-drain".to_string(),
            "live.1".to_string(),
            "--max-turns".to_string(),
            "4".to_string(),
            "--dry-run".to_string(),
            "--json".to_string(),
        ])
        .expect("parse should succeed");
        assert!(matches!(
            rlm_drain.command,
            Some(Command::Agents(AgentsAction::RlmDrain(AgentsRlmDrainArgs {
                ref session_id,
                max_turns: Some(4),
                dry_run: true,
                json: true,
            }))) if session_id == "live.1"
        ));

        let shell_supervisor = Cli::from_argv(vec![
            "agents".to_string(),
            "shell-supervisor".to_string(),
            "--once".to_string(),
            "--json".to_string(),
        ])
        .expect("parse should succeed");
        assert!(matches!(
            shell_supervisor.command,
            Some(Command::Agents(AgentsAction::ShellSupervisor(
                AgentsShellSupervisorArgs {
                    once: true,
                    json: true,
                }
            )))
        ));

        let shell_start = Cli::from_argv(vec![
            "agents".to_string(),
            "shell".to_string(),
            "start".to_string(),
            "--tty".to_string(),
            "--rows".to_string(),
            "33".to_string(),
            "--cols".to_string(),
            "101".to_string(),
            "--json".to_string(),
            "--".to_string(),
            "echo".to_string(),
            "hello".to_string(),
        ])
        .expect("parse should succeed");
        assert!(matches!(
            shell_start.command,
            Some(Command::Agents(AgentsAction::Shell(AgentsShellArgs {
                action: AgentsShellAction::Start {
                    ref command,
                    cwd: None,
                    tty: true,
                    tty_rows: Some(33),
                    tty_cols: Some(101),
                },
                json: true,
            }))) if command == "echo hello"
        ));

        let shell_stdin = Cli::from_argv(vec![
            "agents".to_string(),
            "shell".to_string(),
            "stdin".to_string(),
            "task-1".to_string(),
            "--input".to_string(),
            "hello".to_string(),
            "--timeout-ms".to_string(),
            "100".to_string(),
        ])
        .expect("parse should succeed");
        assert!(matches!(
            shell_stdin.command,
            Some(Command::Agents(AgentsAction::Shell(AgentsShellArgs {
                action: AgentsShellAction::Stdin {
                    ref task_id,
                    input: Some(ref input),
                    close_stdin: false,
                    timeout_ms: Some(100),
                },
                json: false,
            }))) if task_id == "task-1" && input == "hello"
        ));

        let shell_attach_follow = Cli::from_argv(vec![
            "agents".to_string(),
            "shell".to_string(),
            "attach".to_string(),
            "task-1".to_string(),
            "--follow".to_string(),
            "--cursor".to_string(),
            "7".to_string(),
            "--wait-ms".to_string(),
            "250".to_string(),
            "--poll-ms".to_string(),
            "50".to_string(),
            "--max-ms".to_string(),
            "1000".to_string(),
            "--limit-bytes".to_string(),
            "4096".to_string(),
        ])
        .expect("parse should succeed");
        assert!(matches!(
            shell_attach_follow.command,
            Some(Command::Agents(AgentsAction::Shell(AgentsShellArgs {
                action: AgentsShellAction::Attach {
                    ref task_id,
                    cursor: Some(7),
                    wait_ms: Some(250),
                    limit_bytes: Some(4096),
                    tail: false,
                    follow: true,
                    interactive: false,
                    poll_ms: Some(50),
                    max_ms: Some(1000),
                },
                json: false,
            }))) if task_id == "task-1"
        ));

        let shell_attach_interactive = Cli::from_argv(vec![
            "agents".to_string(),
            "shell".to_string(),
            "attach".to_string(),
            "task-2".to_string(),
            "--takeover".to_string(),
            "--cursor".to_string(),
            "12".to_string(),
        ])
        .expect("parse should succeed");
        assert!(matches!(
            shell_attach_interactive.command,
            Some(Command::Agents(AgentsAction::Shell(AgentsShellArgs {
                action: AgentsShellAction::Attach {
                    ref task_id,
                    cursor: Some(12),
                    wait_ms: None,
                    limit_bytes: None,
                    tail: false,
                    follow: false,
                    interactive: true,
                    poll_ms: None,
                    max_ms: None,
                },
                json: false,
            }))) if task_id == "task-2"
        ));

        let shell_resize = Cli::from_argv(vec![
            "agents".to_string(),
            "shell".to_string(),
            "resize".to_string(),
            "task-1".to_string(),
            "40".to_string(),
            "120".to_string(),
            "--json".to_string(),
        ])
        .expect("parse should succeed");
        assert!(matches!(
            shell_resize.command,
            Some(Command::Agents(AgentsAction::Shell(AgentsShellArgs {
                action: AgentsShellAction::Resize {
                    ref task_id,
                    tty_rows: 40,
                    tty_cols: 120,
                },
                json: true,
            }))) if task_id == "task-1"
        ));

        let shell_cancel_all = Cli::from_argv(vec![
            "agents".to_string(),
            "shell".to_string(),
            "cancel".to_string(),
            "--all".to_string(),
        ])
        .expect("parse should succeed");
        assert!(matches!(
            shell_cancel_all.command,
            Some(Command::Agents(AgentsAction::Shell(AgentsShellArgs {
                action: AgentsShellAction::Cancel {
                    task_id: None,
                    all: true,
                },
                json: false,
            })))
        ));

        let service = Cli::from_argv(vec![
            "agents".to_string(),
            "service".to_string(),
            "--kind".to_string(),
            "systemd".to_string(),
            "--out".to_string(),
            "target/services".to_string(),
            "--bin".to_string(),
            "/usr/local/bin/deepseek".to_string(),
            "--workdir".to_string(),
            "/work/repo".to_string(),
            "--addr".to_string(),
            "127.0.0.1:9999".to_string(),
            "--interval-ms".to_string(),
            "500".to_string(),
            "--budget".to_string(),
            "9".to_string(),
        ])
        .expect("parse should succeed");
        assert!(matches!(
            service.command,
            Some(Command::Agents(AgentsAction::Service(AgentsServiceArgs {
                kind: AgentsServiceKind::Systemd,
                ref out,
                ref bin,
                ref workdir,
                ref addr,
                interval_ms: 500,
                budget: Some(9),
            }))) if out.as_deref() == Some("target/services")
                && bin.as_deref() == Some("/usr/local/bin/deepseek")
                && workdir.as_deref() == Some("/work/repo")
                && addr == "127.0.0.1:9999"
        ));

        let service_doctor = Cli::from_argv(vec![
            "agents".to_string(),
            "service-doctor".to_string(),
            "--kind".to_string(),
            "all".to_string(),
            "--out".to_string(),
            "target/services".to_string(),
            "--bin".to_string(),
            "/usr/local/bin/deepseek".to_string(),
            "--workdir".to_string(),
            "/work/repo".to_string(),
            "--addr".to_string(),
            "127.0.0.1:9999".to_string(),
            "--interval-ms".to_string(),
            "500".to_string(),
            "--budget".to_string(),
            "9".to_string(),
            "--json".to_string(),
        ])
        .expect("parse should succeed");
        assert!(matches!(
            service_doctor.command,
            Some(Command::Agents(AgentsAction::ServiceDoctor(AgentsServiceDoctorArgs {
                kind: AgentsServiceKind::All,
                ref out,
                ref bin,
                ref workdir,
                ref addr,
                interval_ms: 500,
                budget: Some(9),
                json: true,
            }))) if out.as_deref() == Some("target/services")
                && bin.as_deref() == Some("/usr/local/bin/deepseek")
                && workdir.as_deref() == Some("/work/repo")
                && addr == "127.0.0.1:9999"
        ));

        let service_smoke = Cli::from_argv(vec![
            "agents".to_string(),
            "service-smoke".to_string(),
            "--bin".to_string(),
            "/usr/local/bin/deepseek".to_string(),
            "--workdir".to_string(),
            "/work/repo".to_string(),
            "--addr".to_string(),
            "127.0.0.1:0".to_string(),
            "--timeout-ms".to_string(),
            "2500".to_string(),
            "--json".to_string(),
        ])
        .expect("parse should succeed");
        assert!(matches!(
            service_smoke.command,
            Some(Command::Agents(AgentsAction::ServiceSmoke(AgentsServiceSmokeArgs {
                ref bin,
                ref workdir,
                ref addr,
                timeout_ms: 2500,
                json: true,
            }))) if bin.as_deref() == Some("/usr/local/bin/deepseek")
                && workdir.as_deref() == Some("/work/repo")
                && addr == "127.0.0.1:0"
        ));

        let threads = Cli::from_argv(vec!["agents".to_string(), "threads".to_string()])
            .expect("parse should succeed");
        assert!(matches!(
            threads.command,
            Some(Command::Agents(AgentsAction::Threads))
        ));

        let show_thread = Cli::from_argv(vec![
            "agents".to_string(),
            "show-thread".to_string(),
            "thread-1".to_string(),
        ])
        .expect("parse should succeed");
        assert!(matches!(
            show_thread.command,
            Some(Command::Agents(AgentsAction::ShowThread { ref id })) if id == "thread-1"
        ));

        let switch = Cli::from_argv(vec![
            "agents".to_string(),
            "switch".to_string(),
            "thread-1".to_string(),
        ])
        .expect("parse should succeed");
        assert!(matches!(
            switch.command,
            Some(Command::Agents(AgentsAction::SwitchThread { ref id })) if id == "thread-1"
        ));

        let current = Cli::from_argv(vec!["agents".to_string(), "current".to_string()])
            .expect("parse should succeed");
        assert!(matches!(
            current.command,
            Some(Command::Agents(AgentsAction::CurrentThread))
        ));
    }

    #[test]
    fn cli_from_argv_routes_agents_service_doctor() {
        let cli = Cli::from_argv(vec![
            "agents".to_string(),
            "service-doctor".to_string(),
            "--kind".to_string(),
            "systemd".to_string(),
            "--out".to_string(),
            "target/services".to_string(),
            "--bin".to_string(),
            "/usr/local/bin/deepseek".to_string(),
            "--workdir".to_string(),
            "/work/repo".to_string(),
            "--addr".to_string(),
            "127.0.0.1:9999".to_string(),
            "--interval-ms".to_string(),
            "500".to_string(),
            "--budget".to_string(),
            "9".to_string(),
            "--json".to_string(),
        ])
        .expect("parse should succeed");

        assert!(matches!(
            cli.command,
            Some(Command::Agents(AgentsAction::ServiceDoctor(AgentsServiceDoctorArgs {
                kind: AgentsServiceKind::Systemd,
                ref out,
                ref bin,
                ref workdir,
                ref addr,
                interval_ms: 500,
                budget: Some(9),
                json: true,
            }))) if out.as_deref() == Some("target/services")
                && bin.as_deref() == Some("/usr/local/bin/deepseek")
                && workdir.as_deref() == Some("/work/repo")
                && addr == "127.0.0.1:9999"
        ));
    }

    #[test]
    fn cli_from_argv_routes_agents_service_smoke() {
        let cli = Cli::from_argv(vec![
            "agents".to_string(),
            "service-smoke".to_string(),
            "--bin".to_string(),
            "/usr/local/bin/deepseek".to_string(),
            "--workdir".to_string(),
            "/work/repo".to_string(),
            "--addr".to_string(),
            "127.0.0.1:0".to_string(),
            "--timeout-ms".to_string(),
            "2500".to_string(),
            "--json".to_string(),
        ])
        .expect("parse should succeed");

        assert!(matches!(
            cli.command,
            Some(Command::Agents(AgentsAction::ServiceSmoke(AgentsServiceSmokeArgs {
                ref bin,
                ref workdir,
                ref addr,
                timeout_ms: 2500,
                json: true,
            }))) if bin.as_deref() == Some("/usr/local/bin/deepseek")
                && workdir.as_deref() == Some("/work/repo")
                && addr == "127.0.0.1:0"
        ));
    }

    #[test]
    fn cli_from_argv_routes_dogfood_subcommand() {
        let cli = Cli::from_argv(vec![
            "dogfood".to_string(),
            "run".to_string(),
            "--budget".to_string(),
            "9".to_string(),
            "investigate".to_string(),
            "planner".to_string(),
        ])
        .unwrap();

        match cli.command.unwrap() {
            Command::Dogfood(DogfoodAction::Run(args)) => {
                assert_eq!(args.budget, Some(9));
                assert!(!args.benchmark_gate);
                assert_eq!(args.task, "investigate planner");
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn cli_from_argv_routes_version_subcommand() {
        let cli = Cli::from_argv(vec!["version".to_string()]).expect("parse should succeed");
        assert!(matches!(cli.command, Some(Command::Version)));
    }

    #[test]
    fn cli_from_argv_routes_version_flags() {
        for flag in ["--version", "-V"] {
            let cli = Cli::from_argv(vec![flag.to_string()]).expect("parse should succeed");
            assert!(
                matches!(cli.command, Some(Command::Version)),
                "flag: {flag}"
            );
        }
    }

    #[test]
    fn cli_from_argv_routes_config_init_with_force() {
        let cli = Cli::from_argv(vec![
            "config".to_string(),
            "init".to_string(),
            "--force".to_string(),
        ])
        .expect("parse should succeed");

        match cli.command {
            Some(Command::Config(args)) => {
                assert!(args.init);
                assert!(args.force);
                assert!(!args.print_default);
            }
            other => panic!("expected Command::Config, got {other:?}"),
        }
    }

    #[test]
    fn cli_from_argv_routes_config_network_allow() {
        let cli = Cli::from_argv(vec![
            "config".to_string(),
            "network".to_string(),
            "allow".to_string(),
            "api.example.com".to_string(),
        ])
        .expect("parse should succeed");

        match cli.command {
            Some(Command::Config(args)) => {
                assert_eq!(args.network_allow.as_deref(), Some("api.example.com"));
                assert_eq!(args.network_deny, None);
                assert!(!args.init);
                assert!(!args.print_default);
            }
            other => panic!("expected Command::Config, got {other:?}"),
        }
    }

    #[test]
    fn cli_from_argv_routes_config_auth_stdin() {
        let cli = Cli::from_argv(vec![
            "config".to_string(),
            "auth".to_string(),
            "DEEPSEEK_API_KEY".to_string(),
            "--stdin".to_string(),
        ])
        .expect("parse should succeed");

        match cli.command {
            Some(Command::Config(args)) => {
                assert_eq!(args.auth_env.as_deref(), Some("DEEPSEEK_API_KEY"));
                assert!(args.auth_stdin);
                assert!(!args.init);
                assert!(!args.print_default);
            }
            other => panic!("expected Command::Config, got {other:?}"),
        }
    }

    #[test]
    fn cli_from_argv_rejects_config_auth_without_stdin() {
        let error = Cli::from_argv(vec![
            "config".to_string(),
            "auth".to_string(),
            "DEEPSEEK_API_KEY".to_string(),
        ])
        .unwrap_err();

        assert!(error.contains("config auth requires --stdin"));
    }

    #[test]
    fn cli_from_argv_routes_doctor_json() {
        let cli = Cli::from_argv(vec!["doctor".to_string(), "--json".to_string()])
            .expect("parse should succeed");

        match cli.command {
            Some(Command::Doctor(args)) => assert!(args.json),
            other => panic!("expected Command::Doctor, got {other:?}"),
        }
    }

    #[test]
    fn cli_from_argv_rejects_unknown_doctor_arg() {
        let error = Cli::from_argv(vec!["doctor".to_string(), "--verbose".to_string()])
            .expect_err("parse should fail");

        assert!(error.contains("unknown doctor argument"));
    }

    #[test]
    fn cli_from_argv_routes_serve_http() {
        let cli = Cli::from_argv(vec![
            "serve".to_string(),
            "--http".to_string(),
            "--addr".to_string(),
            "127.0.0.1:0".to_string(),
            "--once".to_string(),
        ])
        .expect("parse should succeed");

        match cli.command {
            Some(Command::Serve(args)) => match args.action {
                ServeAction::Http(http) => {
                    assert_eq!(http.addr, "127.0.0.1:0");
                    assert!(http.once);
                }
                other => panic!("expected serve --http, got {other:?}"),
            },
            other => panic!("expected Command::Serve, got {other:?}"),
        }
    }

    #[test]
    fn cli_from_argv_routes_serve_mcp_workspace() {
        let cli = Cli::from_argv(vec![
            "serve".to_string(),
            "--mcp".to_string(),
            "--workspace".to_string(),
            "/tmp/workspace".to_string(),
        ])
        .expect("parse should succeed");

        match cli.command {
            Some(Command::Serve(args)) => match args.action {
                ServeAction::Mcp(mcp) => {
                    assert_eq!(mcp.workspace.as_deref(), Some("/tmp/workspace"));
                }
                other => panic!("expected serve --mcp, got {other:?}"),
            },
            other => panic!("expected Command::Serve, got {other:?}"),
        }
    }

    #[test]
    fn cli_from_argv_routes_serve_acp_workspace() {
        let cli = Cli::from_argv(vec![
            "serve".to_string(),
            "--acp".to_string(),
            "--workspace".to_string(),
            "/tmp/workspace".to_string(),
        ])
        .expect("parse should succeed");

        match cli.command {
            Some(Command::Serve(args)) => match args.action {
                ServeAction::Acp(acp) => {
                    assert_eq!(acp.workspace.as_deref(), Some("/tmp/workspace"));
                }
                other => panic!("expected serve --acp, got {other:?}"),
            },
            other => panic!("expected Command::Serve, got {other:?}"),
        }
    }

    #[test]
    fn cli_from_argv_rejects_serve_without_mode() {
        let error = Cli::from_argv(vec!["serve".to_string()]).expect_err("parse should fail");

        assert!(error.contains("serve requires one mode"));
    }

    #[test]
    fn cli_from_argv_routes_update_flags() {
        let cli = Cli::from_argv(vec![
            "update".to_string(),
            "--check".to_string(),
            "--print-command".to_string(),
        ])
        .expect("parse should succeed");

        match cli.command {
            Some(Command::Update(args)) => {
                assert!(args.check);
                assert!(args.print_command);
                assert!(matches!(args.action, UpdateAction::Status));
            }
            other => panic!("expected Command::Update, got {other:?}"),
        }
    }

    #[test]
    fn cli_from_argv_routes_update_package() {
        let cli = Cli::from_argv(vec![
            "update".to_string(),
            "package".to_string(),
            "--out".to_string(),
            "dist".to_string(),
            "--bin".to_string(),
            "target/release/deepseek".to_string(),
        ])
        .expect("parse should succeed");

        match cli.command {
            Some(Command::Update(args)) => match args.action {
                UpdateAction::Package(package) => {
                    assert_eq!(package.out.as_deref(), Some("dist"));
                    assert_eq!(package.bin.as_deref(), Some("target/release/deepseek"));
                }
                other => panic!("expected update package, got {other:?}"),
            },
            other => panic!("expected Command::Update, got {other:?}"),
        }
    }

    #[test]
    fn cli_from_argv_routes_update_verify_install() {
        let cli = Cli::from_argv(vec![
            "update".to_string(),
            "verify-install".to_string(),
            "--bin".to_string(),
            "/tmp/deepseek".to_string(),
            "--workdir".to_string(),
            "/tmp/verify".to_string(),
            "--keep-workdir".to_string(),
        ])
        .expect("parse should succeed");

        match cli.command {
            Some(Command::Update(args)) => match args.action {
                UpdateAction::VerifyInstall(verify) => {
                    assert_eq!(verify.bin.as_deref(), Some("/tmp/deepseek"));
                    assert_eq!(verify.workdir.as_deref(), Some("/tmp/verify"));
                    assert!(verify.keep_workdir);
                }
                other => panic!("expected update verify-install, got {other:?}"),
            },
            other => panic!("expected Command::Update, got {other:?}"),
        }
    }

    #[test]
    fn cli_from_argv_routes_update_install_package_and_rollback() {
        let install = Cli::from_argv(vec![
            "update".to_string(),
            "install-package".to_string(),
            "--package".to_string(),
            "dist/deepseek".to_string(),
            "--dest".to_string(),
            "/tmp/bin/deepseek".to_string(),
            "--backup-dir".to_string(),
            "/tmp/rollback".to_string(),
            "--dry-run".to_string(),
        ])
        .expect("parse should succeed");

        match install.command {
            Some(Command::Update(args)) => match args.action {
                UpdateAction::InstallPackage(install) => {
                    assert_eq!(install.package.as_deref(), Some("dist/deepseek"));
                    assert_eq!(install.dest.as_deref(), Some("/tmp/bin/deepseek"));
                    assert_eq!(install.backup_dir.as_deref(), Some("/tmp/rollback"));
                    assert!(install.dry_run);
                }
                other => panic!("expected update install-package, got {other:?}"),
            },
            other => panic!("expected Command::Update, got {other:?}"),
        }

        let rollback = Cli::from_argv(vec![
            "update".to_string(),
            "rollback".to_string(),
            "--backup".to_string(),
            "/tmp/rollback/deepseek.previous".to_string(),
            "--dest".to_string(),
            "/tmp/bin/deepseek".to_string(),
            "--dry-run".to_string(),
        ])
        .expect("parse should succeed");

        match rollback.command {
            Some(Command::Update(args)) => match args.action {
                UpdateAction::Rollback(rollback) => {
                    assert_eq!(
                        rollback.backup.as_deref(),
                        Some("/tmp/rollback/deepseek.previous")
                    );
                    assert_eq!(rollback.dest.as_deref(), Some("/tmp/bin/deepseek"));
                    assert!(rollback.dry_run);
                }
                other => panic!("expected update rollback, got {other:?}"),
            },
            other => panic!("expected Command::Update, got {other:?}"),
        }
    }

    #[test]
    fn cli_from_argv_routes_update_homebrew_formula() {
        let cli = Cli::from_argv(vec![
            "update".to_string(),
            "homebrew-formula".to_string(),
            "--version".to_string(),
            "1.2.3".to_string(),
            "--repo".to_string(),
            "example/deepseek".to_string(),
            "--dist".to_string(),
            "dist".to_string(),
            "--formula".to_string(),
            "packaging/homebrew/deepseek.rb".to_string(),
            "--out".to_string(),
            "target/deepseek.rb".to_string(),
        ])
        .expect("parse should succeed");

        match cli.command {
            Some(Command::Update(args)) => match args.action {
                UpdateAction::HomebrewFormula(formula) => {
                    assert_eq!(formula.version, "1.2.3");
                    assert_eq!(formula.repo, "example/deepseek");
                    assert_eq!(formula.dist, "dist");
                    assert_eq!(formula.formula, "packaging/homebrew/deepseek.rb");
                    assert_eq!(formula.out.as_deref(), Some("target/deepseek.rb"));
                }
                other => panic!("expected update homebrew-formula, got {other:?}"),
            },
            other => panic!("expected Command::Update, got {other:?}"),
        }
    }

    #[test]
    fn cli_from_argv_routes_update_publish_status() {
        let cli = Cli::from_argv(vec![
            "update".to_string(),
            "publish-status".to_string(),
            "--dist".to_string(),
            "dist-assets".to_string(),
            "--npm-dist".to_string(),
            "npm-dist".to_string(),
            "--strict".to_string(),
            "--json".to_string(),
        ])
        .expect("parse should succeed");

        match cli.command {
            Some(Command::Update(args)) => match args.action {
                UpdateAction::PublishStatus(status) => {
                    assert_eq!(status.dist.as_deref(), Some("dist-assets"));
                    assert_eq!(status.npm_dist.as_deref(), Some("npm-dist"));
                    assert!(status.strict);
                    assert!(status.json);
                }
                other => panic!("expected update publish-status, got {other:?}"),
            },
            other => panic!("expected Command::Update, got {other:?}"),
        }
    }

    #[test]
    fn cli_from_argv_routes_update_download_plan() {
        let cli = Cli::from_argv(vec![
            "update".to_string(),
            "download-plan".to_string(),
            "--version".to_string(),
            "v1.2.3".to_string(),
            "--repo".to_string(),
            "example/deepseek".to_string(),
            "--base-url".to_string(),
            "https://mirror.example/releases/v1.2.3".to_string(),
            "--platform".to_string(),
            "linux-x64".to_string(),
            "--json".to_string(),
        ])
        .expect("parse should succeed");

        match cli.command {
            Some(Command::Update(args)) => match args.action {
                UpdateAction::DownloadPlan(plan) => {
                    assert_eq!(plan.version.as_deref(), Some("v1.2.3"));
                    assert_eq!(plan.repo.as_deref(), Some("example/deepseek"));
                    assert_eq!(
                        plan.base_url.as_deref(),
                        Some("https://mirror.example/releases/v1.2.3")
                    );
                    assert_eq!(plan.platform.as_deref(), Some("linux-x64"));
                    assert!(plan.json);
                }
                other => panic!("expected update download-plan, got {other:?}"),
            },
            other => panic!("expected Command::Update, got {other:?}"),
        }
    }

    #[test]
    fn cli_from_argv_rejects_config_init_with_print_default() {
        let error = Cli::from_argv(vec![
            "config".to_string(),
            "init".to_string(),
            "--print-default".to_string(),
        ])
        .expect_err("parse should fail");

        assert!(error.contains("cannot be combined"));
    }

    #[test]
    fn cli_from_argv_rejects_config_force_without_init() {
        let error = Cli::from_argv(vec!["config".to_string(), "--force".to_string()])
            .expect_err("parse should fail");

        assert!(error.contains("requires init"));
    }

    #[test]
    fn cli_from_argv_routes_completion_subcommand() {
        let cli = Cli::from_argv(vec!["completion".to_string(), "bash".to_string()])
            .expect("parse should succeed");
        assert!(matches!(
            cli.command,
            Some(Command::Completion(CompletionShell::Bash))
        ));
    }

    #[test]
    fn cli_from_argv_rejects_unknown_completion_shell() {
        let err = Cli::from_argv(vec!["completion".to_string(), "pwsh".to_string()])
            .expect_err("parse should fail");
        assert!(err.contains("unknown completion shell"));
    }
}
