use std::borrow::Cow;
use std::collections::{BTreeMap, HashSet};
use std::env;
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use crate::cli::app::{
    BenchmarkArgs, DogfoodAction, DogfoodExportArgs, DogfoodOutcome, DogfoodPromoteArgs,
    DogfoodReplayArgs, DogfoodReportArgs, DogfoodRunArgs,
};
use crate::cli::commands::benchmark::BenchmarkCaseSummary;
use crate::config::load::load_or_default;
use crate::core::context::TaskContext;
use crate::core::loop_runtime::{AgentLoop, AgentLoopOptions, RunResult};
use crate::error::{app_error, AppError, AppErrorKind, AppResult};
use crate::model::protocol::ObservationStatus;
use crate::util::json::{
    json_as_string, json_as_u64, json_value_to_string, parse_root_object, JsonValue,
};

const DEFAULT_REPORT_LIMIT: usize = 20;
const CATEGORY_TREND_WINDOW: usize = 5;

pub fn run(action: DogfoodAction) -> AppResult<()> {
    let config = load_or_default()?;
    match action {
        DogfoodAction::Run(args) => run_live_task(&config, args),
        DogfoodAction::ReplayBenchmark(args) => replay_benchmark_command(&config, args),
        DogfoodAction::Report(args) => render_report_command(&config, args),
        DogfoodAction::ExportBenchmark(args) => export_benchmark_command(&config, args),
        DogfoodAction::PromoteBenchmark(args) => promote_benchmark_command(&config, args),
    }
}

fn run_live_task(config: &crate::config::types::AppConfig, args: DogfoodRunArgs) -> AppResult<()> {
    let args = resolve_run_args(config, args)?;
    let started = Instant::now();
    let repo_root = std::env::current_dir()?;
    let requested_workdir = resolve_run_workdir(&repo_root, args.workdir.as_deref())?;
    let (run_workdir, cleanup_workdir) =
        prepare_run_workdir(&requested_workdir, args.isolate_workdir)?;
    let workdir = run_workdir.display().to_string();
    let budget = args.budget.unwrap_or(AgentLoopOptions::default().steps);
    let manual_intervention =
        args.manual_intervention || matches!(args.outcome, Some(DogfoodOutcome::Manual));

    println!("DeepseekCode dogfood");
    println!("task: {}", args.task);
    println!("budget: {budget}");
    println!("workdir: {workdir}");

    let auto_approve =
        args.from_benchmark.is_some() && args.isolate_workdir && run_workdir != repo_root;
    let run_result = run_task_in_workdir(&repo_root, &run_workdir, auto_approve, || {
        AgentLoop::new(config.clone()).run_with(
            TaskContext::new(args.task.clone(), args.skill.clone()),
            AgentLoopOptions {
                steps: budget,
                ..AgentLoopOptions::default()
            },
        )
    });
    if let Some(path) = cleanup_workdir.as_ref() {
        let _ = fs::remove_dir_all(path);
    }

    let duration_ms = started.elapsed().as_millis() as u64;
    let timestamp_secs = unix_now_secs()?;
    let ledger_path = config.workspace.dogfood_ledger_path();

    let record = match &run_result {
        Ok(result) => DogfoodRecord::from_result(
            timestamp_secs,
            duration_ms,
            config.model.model.clone(),
            workdir,
            budget,
            &args,
            manual_intervention,
            result,
        ),
        Err(error) => DogfoodRecord::from_error(
            timestamp_secs,
            duration_ms,
            config.model.model.clone(),
            workdir,
            budget,
            &args,
            manual_intervention,
            error.as_ref(),
        ),
    };

    append_record(&ledger_path, &record)?;
    let records = load_records(&ledger_path)?;
    let report_path = config.workspace.dogfood_report_path();
    write_report(&ledger_path, &report_path, &records, DEFAULT_REPORT_LIMIT)?;

    println!(
        "ledger: {} (outcome: {}, manual_intervention: {})",
        ledger_path.display(),
        record.outcome.label(),
        if record.manual_intervention {
            "yes"
        } else {
            "no"
        }
    );
    println!("report: {}", report_path.display());
    if args.benchmark_gate {
        println!("post-task benchmark gate: running default benchmark baseline");
        crate::cli::commands::benchmark::run_with_config(config.clone(), BenchmarkArgs::default())?;
    }

    match run_result {
        Ok(_) => Ok(()),
        Err(error) => Err(error),
    }
}

fn resolve_run_args(
    config: &crate::config::types::AppConfig,
    mut args: DogfoodRunArgs,
) -> AppResult<DogfoodRunArgs> {
    let Some(case_name) = args.from_benchmark.as_deref() else {
        return Ok(args);
    };
    let manifest_path = args
        .benchmark_manifest
        .as_ref()
        .map(PathBuf::from)
        .unwrap_or_else(|| config.workspace.benchmark_manifest_path());
    let manifest_dir = manifest_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    let summaries = crate::cli::commands::benchmark::load_manifest_case_summaries(&manifest_path)?;
    let case = summaries
        .into_iter()
        .find(|case| case.name == case_name)
        .ok_or_else(|| {
            app_error(format!(
                "benchmark case `{case_name}` was not found in {}",
                manifest_path.display()
            ))
        })?;
    args.task = case.task;
    if args.skill.is_none() {
        args.skill = case.skill;
    }
    if args.budget.is_none() {
        args.budget = Some(case.budget);
    }
    if args.workdir.is_none() {
        args.workdir = case.workdir.map(|workdir| {
            let candidate = PathBuf::from(&workdir);
            if candidate.is_absolute() {
                workdir
            } else {
                manifest_dir.join(candidate).display().to_string()
            }
        });
    }
    if !args.isolate_workdir {
        args.isolate_workdir = case.isolate_workdir;
    }
    if args.notes.is_none() {
        args.notes = case.notes;
    }
    Ok(args)
}

fn resolve_run_workdir(repo_root: &Path, requested: Option<&str>) -> AppResult<PathBuf> {
    let path = match requested {
        Some(raw) if !raw.trim().is_empty() => {
            let candidate = PathBuf::from(raw);
            if candidate.is_absolute() {
                candidate
            } else {
                repo_root.join(candidate)
            }
        }
        _ => repo_root.to_path_buf(),
    };

    if path.is_dir() {
        Ok(path)
    } else {
        Err(app_error(format!(
            "dogfood workdir does not exist or is not a directory: {}",
            path.display()
        )))
    }
}

fn prepare_run_workdir(
    workdir: &Path,
    isolate_workdir: bool,
) -> AppResult<(PathBuf, Option<PathBuf>)> {
    if !isolate_workdir {
        return Ok((workdir.to_path_buf(), None));
    }
    let suffix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| app_error(format!("system clock error: {error}")))?
        .as_nanos();

    let temp_root = std::env::temp_dir().join(format!(
        "deepseek-dogfood-{}-{}",
        std::process::id(),
        suffix
    ));
    copy_dir_recursive(workdir, &temp_root)?;
    Ok((temp_root.clone(), Some(temp_root)))
}

fn dogfood_cwd_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

fn run_task_in_workdir<T>(
    repo_root: &Path,
    run_workdir: &Path,
    auto_approve: bool,
    f: impl FnOnce() -> AppResult<T>,
) -> AppResult<T> {
    let _cwd_guard = dogfood_cwd_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let previous_auto_approve_writes = env::var_os("DSCODE_AUTO_APPROVE_WRITES");
    let previous_auto_approve_shell = env::var_os("DSCODE_AUTO_APPROVE_SHELL");
    let changed_workdir = run_workdir != repo_root;
    if changed_workdir {
        env::set_current_dir(run_workdir)?;
    }
    if auto_approve {
        unsafe {
            env::set_var("DSCODE_AUTO_APPROVE_WRITES", "1");
            env::set_var("DSCODE_AUTO_APPROVE_SHELL", "1");
        }
    }
    let result = f();
    let restore_result = if changed_workdir {
        env::set_current_dir(repo_root)
    } else {
        Ok(())
    };
    if auto_approve {
        restore_env_var("DSCODE_AUTO_APPROVE_WRITES", previous_auto_approve_writes);
        restore_env_var("DSCODE_AUTO_APPROVE_SHELL", previous_auto_approve_shell);
    }
    match (result, restore_result) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(error), Ok(())) => Err(error),
        (Ok(_), Err(error)) => Err(app_error(format!("failed to restore dogfood cwd: {error}"))),
        (Err(error), Err(_restore_error)) => Err(error),
    }
}

fn restore_env_var(name: &str, value: Option<std::ffi::OsString>) {
    match value {
        Some(value) => unsafe { env::set_var(name, value) },
        None => unsafe { env::remove_var(name) },
    }
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> AppResult<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let source = entry.path();
        let target = dst.join(entry.file_name());
        let metadata = entry.metadata()?;
        if metadata.is_dir() {
            copy_dir_recursive(&source, &target)?;
        } else if metadata.is_file() {
            fs::copy(&source, &target)?;
        }
    }
    Ok(())
}

fn render_report_command(
    config: &crate::config::types::AppConfig,
    args: DogfoodReportArgs,
) -> AppResult<()> {
    let ledger_path = config.workspace.dogfood_ledger_path();
    let report_path = args
        .out
        .map(PathBuf::from)
        .unwrap_or_else(|| config.workspace.dogfood_report_path());
    let limit = args.limit.unwrap_or(DEFAULT_REPORT_LIMIT);
    let records = load_records(&ledger_path)?;
    write_report(&ledger_path, &report_path, &records, limit)?;
    println!("DeepseekCode dogfood report");
    println!("ledger: {}", ledger_path.display());
    println!("report: {}", report_path.display());
    Ok(())
}

fn replay_benchmark_command(
    config: &crate::config::types::AppConfig,
    args: DogfoodReplayArgs,
) -> AppResult<()> {
    let manifest_path = args
        .manifest
        .map(PathBuf::from)
        .unwrap_or_else(|| config.workspace.benchmark_manifest_path());
    let summaries = crate::cli::commands::benchmark::load_manifest_case_summaries(&manifest_path)?;
    let selected = select_replayable_cases(&summaries, args.category.as_deref(), args.limit);

    println!("DeepseekCode dogfood benchmark replay");
    println!("manifest: {}", manifest_path.display());
    println!(
        "selected: {}{}",
        selected.len(),
        args.category
            .as_deref()
            .map(|category| format!(" (category: {category})"))
            .unwrap_or_default()
    );

    if selected.is_empty() {
        println!("no replayable benchmark cases matched the requested filters");
        return Ok(());
    }

    for case in &selected {
        println!("replay: {}", case.name);
        run_live_task(
            config,
            DogfoodRunArgs {
                task: String::new(),
                from_benchmark: Some(case.name.clone()),
                benchmark_manifest: Some(manifest_path.display().to_string()),
                skill: None,
                budget: None,
                workdir: None,
                isolate_workdir: false,
                outcome: None,
                manual_intervention: false,
                benchmark_gate: false,
                notes: None,
            },
        )?;
    }

    if args.benchmark_gate {
        println!("post-replay benchmark gate: running default benchmark baseline");
        crate::cli::commands::benchmark::run_with_config(config.clone(), BenchmarkArgs::default())?;
    }
    Ok(())
}

fn export_benchmark_command(
    config: &crate::config::types::AppConfig,
    args: DogfoodExportArgs,
) -> AppResult<()> {
    let ledger_path = config.workspace.dogfood_ledger_path();
    let out_path = args
        .out
        .map(PathBuf::from)
        .unwrap_or_else(|| config.workspace.dogfood_benchmark_seed_path());
    let limit = args.limit.unwrap_or(DEFAULT_REPORT_LIMIT);
    let records = load_records(&ledger_path)?;
    let repo_root = std::env::current_dir()?;
    let export = render_benchmark_seed_export(&records, limit, args.outcome, &repo_root);
    if let Some(parent) = out_path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&out_path, export)?;
    println!("DeepseekCode dogfood benchmark export");
    println!("ledger: {}", ledger_path.display());
    println!("out: {}", out_path.display());
    Ok(())
}

fn promote_benchmark_command(
    config: &crate::config::types::AppConfig,
    args: DogfoodPromoteArgs,
) -> AppResult<()> {
    let ledger_path = config.workspace.dogfood_ledger_path();
    let manifest_path = args
        .manifest
        .map(PathBuf::from)
        .unwrap_or_else(|| config.workspace.benchmark_manifest_path());
    let limit = args.limit.unwrap_or(DEFAULT_REPORT_LIMIT);
    let records = load_records(&ledger_path)?;
    let existing = crate::cli::commands::benchmark::load_manifest_case_summaries(&manifest_path)?;
    let repo_root = std::env::current_dir()?;
    let plan = build_promotion_plan(&records, &existing, limit, args.outcome, &repo_root);

    println!("DeepseekCode dogfood benchmark promotion");
    println!("ledger: {}", ledger_path.display());
    println!("manifest: {}", manifest_path.display());
    println!(
        "selected: {} (duplicates skipped: {}, policy skipped: {}, dry_run: {})",
        plan.cases.len(),
        plan.duplicates_skipped,
        plan.policy_skipped,
        if args.dry_run { "yes" } else { "no" }
    );
    if !plan.policy_skip_reasons.is_empty() {
        println!("policy skip reasons:");
        for reason in &plan.policy_skip_reasons {
            println!(
                "- {}: {} (example task: {})",
                reason.reason_label,
                reason.count,
                clip(&reason.example_task, 72)
            );
        }
    }

    if plan.cases.is_empty() {
        println!("no new replayable seed cases matched the requested filters");
        return Ok(());
    }

    if args.dry_run {
        println!("dry run only; manifest not modified");
        return Ok(());
    }

    append_promoted_cases(&manifest_path, &plan.cases)?;
    println!("appended: {}", plan.cases.len());
    Ok(())
}

fn select_replayable_cases(
    cases: &[BenchmarkCaseSummary],
    category: Option<&str>,
    limit: Option<usize>,
) -> Vec<BenchmarkCaseSummary> {
    let mut selected = cases
        .iter()
        .filter(|case| case.workdir.is_some() && case.seed_observations.is_none())
        .filter(|case| category.is_none_or(|expected| case.category == expected))
        .cloned()
        .collect::<Vec<_>>();
    if let Some(limit) = limit {
        selected.truncate(limit);
    }
    selected
}

#[derive(Debug, Clone)]
struct DogfoodRecord {
    version: u64,
    timestamp_secs: u64,
    duration_ms: u64,
    task: String,
    skill: Option<String>,
    budget: u64,
    model: String,
    workdir: String,
    outcome: DogfoodOutcome,
    manual_intervention: bool,
    notes: Option<String>,
    tool_calls: u64,
    failed_tool_calls: u64,
    repeated_call_failures: u64,
    diagnostic_expected_failure: bool,
    used_subagent: bool,
    final_message: String,
    tool_trace: String,
    error_kind: Option<String>,
    benchmark_category: Option<String>,
    benchmark_seed_observations: Option<String>,
}

impl DogfoodRecord {
    fn from_result(
        timestamp_secs: u64,
        duration_ms: u64,
        model: String,
        workdir: String,
        budget: usize,
        args: &DogfoodRunArgs,
        manual_intervention: bool,
        result: &RunResult,
    ) -> Self {
        let failed_tool_calls = result
            .tool_events
            .iter()
            .filter(|event| tool_event_counts_as_failed(event))
            .count() as u64;
        let repeated_call_failures = result
            .tool_events
            .iter()
            .filter(|event| {
                event
                    .output
                    .contains("repeated identical tool call detected")
            })
            .count() as u64;
        let used_subagent = result
            .tool_events
            .iter()
            .any(|event| event.tool_name == "dispatch_subagent");
        let tool_trace = if result.tool_events.is_empty() {
            "none".to_string()
        } else {
            result
                .tool_events
                .iter()
                .map(|event| event.tool_name.as_str())
                .collect::<Vec<_>>()
                .join(" -> ")
        };
        let diagnostic_expected_failure = args.outcome.is_none()
            && is_expected_failure_diagnosis_result(
                &args.task,
                &result.tool_events,
                repeated_call_failures,
            );
        let recovered_validation_success = args.outcome.is_none()
            && (is_recovered_validation_success(&result.tool_events)
                || is_successful_write_validation_result(&result.tool_events));
        let outcome = args.outcome.unwrap_or_else(|| {
            derive_default_outcome(
                failed_tool_calls,
                repeated_call_failures,
                diagnostic_expected_failure,
                recovered_validation_success,
            )
        });
        let benchmark_category = infer_benchmark_category(
            &args.task,
            &tool_trace,
            failed_tool_calls,
            repeated_call_failures,
            false,
            None,
        );

        Self {
            version: 1,
            timestamp_secs,
            duration_ms,
            task: args.task.clone(),
            skill: args.skill.clone(),
            budget: budget as u64,
            model,
            workdir,
            outcome,
            manual_intervention,
            notes: args.notes.clone(),
            tool_calls: result.tool_events.len() as u64,
            failed_tool_calls,
            repeated_call_failures,
            diagnostic_expected_failure,
            used_subagent,
            final_message: first_non_empty_line(&result.final_message)
                .unwrap_or("")
                .to_string(),
            tool_trace,
            error_kind: None,
            benchmark_category: Some(benchmark_category.to_string()),
            benchmark_seed_observations: serialize_seed_observations(&result.tool_events),
        }
    }

    fn from_error(
        timestamp_secs: u64,
        duration_ms: u64,
        model: String,
        workdir: String,
        budget: usize,
        args: &DogfoodRunArgs,
        manual_intervention: bool,
        error: &(dyn std::error::Error + 'static),
    ) -> Self {
        let outcome = args.outcome.unwrap_or(DogfoodOutcome::Failed);
        let benchmark_category = infer_benchmark_category(&args.task, "none", 1, 0, false, None);
        Self {
            version: 1,
            timestamp_secs,
            duration_ms,
            task: args.task.clone(),
            skill: args.skill.clone(),
            budget: budget as u64,
            model,
            workdir,
            outcome,
            manual_intervention,
            notes: args.notes.clone(),
            tool_calls: 0,
            failed_tool_calls: 1,
            repeated_call_failures: 0,
            diagnostic_expected_failure: false,
            used_subagent: false,
            final_message: error.to_string(),
            tool_trace: "none".to_string(),
            error_kind: Some(error_kind_for_ref(error)),
            benchmark_category: Some(benchmark_category.to_string()),
            benchmark_seed_observations: None,
        }
    }

    fn to_json_line(&self) -> String {
        let mut root = BTreeMap::new();
        root.insert(
            "version".to_string(),
            JsonValue::Number(self.version.to_string()),
        );
        root.insert(
            "timestamp_secs".to_string(),
            JsonValue::Number(self.timestamp_secs.to_string()),
        );
        root.insert(
            "duration_ms".to_string(),
            JsonValue::Number(self.duration_ms.to_string()),
        );
        root.insert("task".to_string(), JsonValue::String(self.task.clone()));
        root.insert(
            "skill".to_string(),
            self.skill
                .as_ref()
                .map(|value| JsonValue::String(value.clone()))
                .unwrap_or(JsonValue::Null),
        );
        root.insert(
            "budget".to_string(),
            JsonValue::Number(self.budget.to_string()),
        );
        root.insert("model".to_string(), JsonValue::String(self.model.clone()));
        root.insert(
            "workdir".to_string(),
            JsonValue::String(self.workdir.clone()),
        );
        root.insert(
            "outcome".to_string(),
            JsonValue::String(self.outcome.label().to_string()),
        );
        root.insert(
            "manual_intervention".to_string(),
            JsonValue::Bool(self.manual_intervention),
        );
        root.insert(
            "notes".to_string(),
            self.notes
                .as_ref()
                .map(|value| JsonValue::String(value.clone()))
                .unwrap_or(JsonValue::Null),
        );
        root.insert(
            "tool_calls".to_string(),
            JsonValue::Number(self.tool_calls.to_string()),
        );
        root.insert(
            "failed_tool_calls".to_string(),
            JsonValue::Number(self.failed_tool_calls.to_string()),
        );
        root.insert(
            "repeated_call_failures".to_string(),
            JsonValue::Number(self.repeated_call_failures.to_string()),
        );
        root.insert(
            "diagnostic_expected_failure".to_string(),
            JsonValue::Bool(self.diagnostic_expected_failure),
        );
        root.insert(
            "used_subagent".to_string(),
            JsonValue::Bool(self.used_subagent),
        );
        root.insert(
            "final_message".to_string(),
            JsonValue::String(self.final_message.clone()),
        );
        root.insert(
            "tool_trace".to_string(),
            JsonValue::String(self.tool_trace.clone()),
        );
        root.insert(
            "error_kind".to_string(),
            self.error_kind
                .as_ref()
                .map(|value| JsonValue::String(value.clone()))
                .unwrap_or(JsonValue::Null),
        );
        root.insert(
            "benchmark_category".to_string(),
            self.benchmark_category
                .as_ref()
                .map(|value| JsonValue::String(value.clone()))
                .unwrap_or(JsonValue::Null),
        );
        root.insert(
            "benchmark_seed_observations".to_string(),
            self.benchmark_seed_observations
                .as_ref()
                .map(|value| JsonValue::String(value.clone()))
                .unwrap_or(JsonValue::Null),
        );
        json_value_to_string(&JsonValue::Object(root))
    }

    fn from_json_line(line: &str) -> AppResult<Self> {
        let root = parse_root_object(line)?;
        let version = read_u64(&root, "version")?;
        let timestamp_secs = read_u64(&root, "timestamp_secs")?;
        let duration_ms = read_u64(&root, "duration_ms")?;
        let task = read_string(&root, "task")?.to_string();
        let skill = read_optional_string(&root, "skill").map(str::to_string);
        let budget = read_u64(&root, "budget")?;
        let model = read_string(&root, "model")?.to_string();
        let workdir = read_string(&root, "workdir")?.to_string();
        let outcome = parse_dogfood_outcome(read_string(&root, "outcome")?)
            .ok_or_else(|| app_error("dogfood record has invalid `outcome`"))?;
        let manual_intervention = read_bool(&root, "manual_intervention")?;
        let notes = read_optional_string(&root, "notes").map(str::to_string);
        let tool_calls = read_u64(&root, "tool_calls")?;
        let failed_tool_calls = read_u64(&root, "failed_tool_calls")?;
        let repeated_call_failures = read_u64(&root, "repeated_call_failures")?;
        let diagnostic_expected_failure =
            read_optional_bool(&root, "diagnostic_expected_failure").unwrap_or(false);
        let used_subagent = read_bool(&root, "used_subagent")?;
        let final_message = read_string(&root, "final_message")?.to_string();
        let tool_trace = read_string(&root, "tool_trace")?.to_string();
        let error_kind = read_optional_string(&root, "error_kind").map(str::to_string);
        let stored_benchmark_category =
            read_optional_string(&root, "benchmark_category").map(str::to_string);
        let benchmark_seed_observations =
            read_optional_string(&root, "benchmark_seed_observations").map(str::to_string);

        let inferred_category = infer_benchmark_category(
            &task,
            &tool_trace,
            failed_tool_calls,
            repeated_call_failures,
            used_subagent,
            benchmark_seed_observations.as_deref(),
        )
        .to_string();
        let benchmark_category = match stored_benchmark_category {
            Some(stored)
                if stored == "planning"
                    && inferred_category != "planning"
                    && !task_looks_like_planning(&task) =>
            {
                Some(inferred_category)
            }
            Some(stored) => Some(stored),
            None => Some(inferred_category),
        };

        Ok(Self {
            version,
            timestamp_secs,
            duration_ms,
            task,
            skill,
            budget,
            model,
            workdir,
            outcome,
            manual_intervention,
            notes,
            tool_calls,
            failed_tool_calls,
            repeated_call_failures,
            diagnostic_expected_failure,
            used_subagent,
            final_message,
            tool_trace,
            error_kind,
            benchmark_category,
            benchmark_seed_observations,
        })
    }
}

impl DogfoodOutcome {
    fn label(&self) -> &'static str {
        match self {
            DogfoodOutcome::Success => "success",
            DogfoodOutcome::Failed => "failed",
            DogfoodOutcome::Stuck => "stuck",
            DogfoodOutcome::Manual => "manual",
        }
    }
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

fn derive_default_outcome(
    failed_tool_calls: u64,
    repeated_call_failures: u64,
    diagnostic_expected_failure: bool,
    recovered_validation_success: bool,
) -> DogfoodOutcome {
    if diagnostic_expected_failure || recovered_validation_success {
        DogfoodOutcome::Success
    } else if repeated_call_failures > 0 {
        DogfoodOutcome::Stuck
    } else if failed_tool_calls > 0 {
        DogfoodOutcome::Failed
    } else {
        DogfoodOutcome::Success
    }
}

fn is_expected_failure_diagnosis_result(
    task: &str,
    events: &[crate::core::loop_runtime::ToolEvent],
    repeated_call_failures: u64,
) -> bool {
    if repeated_call_failures > 0 || !task_looks_like_failure_diagnosis(task) {
        return false;
    }
    let mut saw_expected_failed_command = false;
    let mut saw_follow_up_diagnostic = false;
    for event in events {
        if matches!(event.status, ObservationStatus::Failed) || event.tool_name == "apply_patch" {
            return false;
        }
        if event.tool_name == "run_shell"
            && event
                .output
                .lines()
                .any(|line| line.trim() == "meta.result=failed")
        {
            saw_expected_failed_command = true;
        }
        if matches!(
            event.tool_name.as_str(),
            "read_file" | "search_text" | "list_files"
        ) {
            saw_follow_up_diagnostic = true;
        }
    }
    saw_expected_failed_command && saw_follow_up_diagnostic
}

fn is_recovered_validation_success(events: &[crate::core::loop_runtime::ToolEvent]) -> bool {
    let mut saw_failed_validation = false;
    let mut saw_success_after_failure = false;
    for event in events {
        if matches!(event.status, ObservationStatus::Failed) {
            return false;
        }
        if event.tool_name != "run_shell" {
            continue;
        }
        if event
            .output
            .lines()
            .any(|line| line.trim() == "meta.result=failed")
        {
            saw_failed_validation = true;
            saw_success_after_failure = false;
            continue;
        }
        if event
            .output
            .lines()
            .any(|line| line.trim() == "meta.result=ok")
            && saw_failed_validation
        {
            saw_success_after_failure = true;
        }
    }

    saw_success_after_failure
}

fn is_successful_write_validation_result(events: &[crate::core::loop_runtime::ToolEvent]) -> bool {
    let mut saw_successful_patch = false;
    for event in events {
        if event.tool_name == "apply_patch" && matches!(event.status, ObservationStatus::Ok) {
            saw_successful_patch = true;
            continue;
        }
        if !saw_successful_patch || event.tool_name != "run_shell" {
            continue;
        }
        let is_test = event
            .output
            .lines()
            .any(|line| line.trim() == "meta.command_kind=test");
        let is_ok = event
            .output
            .lines()
            .any(|line| line.trim() == "meta.result=ok");
        if is_test && is_ok {
            return true;
        }
    }
    false
}

fn tool_event_counts_as_failed(event: &crate::core::loop_runtime::ToolEvent) -> bool {
    matches!(event.status, ObservationStatus::Failed)
        || event
            .output
            .lines()
            .any(|line| line.trim() == "meta.result=failed")
}

fn error_kind_label(kind: AppErrorKind) -> String {
    match kind {
        AppErrorKind::Other => "other",
        AppErrorKind::PolicyDenied => "policy_denied",
        AppErrorKind::ToolFailure => "tool_failure",
    }
    .to_string()
}

fn error_kind_for_ref(error: &(dyn std::error::Error + 'static)) -> String {
    error
        .downcast_ref::<AppError>()
        .map(|app| error_kind_label(app.kind))
        .unwrap_or_else(|| error_kind_label(AppErrorKind::Other))
}

fn append_record(path: &Path, record: &DogfoodRecord) -> AppResult<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    writeln!(file, "{}", record.to_json_line())?;
    Ok(())
}

fn load_records(path: &Path) -> AppResult<Vec<DogfoodRecord>> {
    let file = fs::File::open(path).map_err(|error| {
        app_error(format!(
            "failed to read dogfood ledger {}: {error}",
            path.display()
        ))
    })?;
    let reader = BufReader::new(file);
    let mut records = Vec::new();
    for (index, line) in reader.lines().enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let record = DogfoodRecord::from_json_line(&line).map_err(|error| {
            app_error(format!(
                "failed to parse dogfood ledger line {} in {}: {error}",
                index + 1,
                path.display()
            ))
        })?;
        records.push(record);
    }
    if records.is_empty() {
        return Err(app_error(format!(
            "dogfood ledger {} does not contain any records",
            path.display()
        )));
    }
    Ok(records)
}

fn write_report(
    ledger_path: &Path,
    report_path: &Path,
    records: &[DogfoodRecord],
    limit: usize,
) -> AppResult<()> {
    if let Some(parent) = report_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let report = render_report(ledger_path, records, limit);
    fs::write(report_path, report)?;
    Ok(())
}

fn render_report(ledger_path: &Path, records: &[DogfoodRecord], limit: usize) -> String {
    let total = records.len();
    let success = records
        .iter()
        .filter(|record| matches!(record.outcome, DogfoodOutcome::Success))
        .count();
    let diagnostic = records
        .iter()
        .filter(|record| record.diagnostic_expected_failure)
        .count();
    let stuck = records
        .iter()
        .filter(|record| matches!(record.outcome, DogfoodOutcome::Stuck))
        .count();
    let manual = records
        .iter()
        .filter(|record| record.manual_intervention)
        .count();
    let failed = records
        .iter()
        .filter(|record| matches!(record.outcome, DogfoodOutcome::Failed))
        .count();
    let total_tool_calls = records.iter().map(|record| record.tool_calls).sum::<u64>();
    let overall_avg_tool_calls = if total == 0 {
        0.0
    } else {
        total_tool_calls as f64 / total as f64
    };
    let benchmark_seed_candidates = records
        .iter()
        .filter(|record| is_benchmark_seed_candidate(record, None))
        .count();
    let category_stats = aggregate_category_stats(records);
    let recent_start = total.saturating_sub(CATEGORY_TREND_WINDOW);
    let previous_start = recent_start.saturating_sub(CATEGORY_TREND_WINDOW);
    let recent_window = &records[recent_start..];
    let previous_window = &records[previous_start..recent_start];
    let recent_category_stats = aggregate_category_stats(recent_window);
    let previous_category_stats = aggregate_category_stats(previous_window);

    let mut out = String::new();
    out.push_str("# DeepseekCode Dogfood Report\n\n");
    out.push_str(&format!("- Ledger: `{}`\n", ledger_path.display()));
    out.push_str(&format!("- Runs: {total}\n"));
    out.push_str(&format!("- Success rate: {}\n", rate_line(success, total)));
    out.push_str(&format!(
        "- Diagnostic expected-failure rate: {}\n",
        rate_line(diagnostic, total)
    ));
    out.push_str(&format!("- Failed rate: {}\n", rate_line(failed, total)));
    out.push_str(&format!("- Stuck rate: {}\n", rate_line(stuck, total)));
    out.push_str(&format!(
        "- Manual intervention rate: {}\n",
        rate_line(manual, total)
    ));
    out.push_str(&format!(
        "- Average tool calls: {:.2}\n\n",
        overall_avg_tool_calls
    ));
    out.push_str(&format!(
        "- Benchmark seed candidates: {benchmark_seed_candidates}\n\n"
    ));
    if !category_stats.is_empty() {
        out.push_str("## Category Breakdown\n\n");
        out.push_str(
            "| Category | Runs | Success | Diagnostic | Failed | Stuck | Manual | Avg Tool Calls | Seed Candidates |\n",
        );
        out.push_str("| --- | ---: | --- | --- | --- | --- | --- | ---: | ---: |\n");
        for (category, stats) in &category_stats {
            let avg_tool_calls = if stats.runs == 0 {
                0.0
            } else {
                stats.total_tool_calls as f64 / stats.runs as f64
            };
            out.push_str(&format!(
                "| {} | {} | {} | {} | {} | {} | {} | {:.2} | {} |\n",
                escape_table(category),
                stats.runs,
                rate_line(stats.success, stats.runs),
                rate_line(stats.diagnostic, stats.runs),
                rate_line(stats.failed, stats.runs),
                rate_line(stats.stuck, stats.runs),
                rate_line(stats.manual, stats.runs),
                avg_tool_calls,
                stats.seed_candidates,
            ));
        }
        out.push('\n');
    }
    out.push_str("## Category Trend\n\n");
    out.push_str(&format!(
        "- Trend window: recent {} runs vs previous {} runs\n",
        CATEGORY_TREND_WINDOW, CATEGORY_TREND_WINDOW
    ));
    if previous_window.is_empty() {
        out.push_str("- Status: insufficient history\n\n");
    } else {
        out.push_str(&format!(
            "- Compared windows: recent={} previous={}\n\n",
            recent_window.len(),
            previous_window.len()
        ));
        out.push_str(
            "| Category | Recent Runs | Prev Runs | Recent Success | Prev Success | Δ Success pp | Recent Avg Tools | Prev Avg Tools | Δ Tools | Recent Seeds | Prev Seeds |\n",
        );
        out.push_str(
            "| --- | ---: | ---: | --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |\n",
        );
        let mut categories = BTreeMap::<String, ()>::new();
        for category in recent_category_stats.keys() {
            categories.insert(category.clone(), ());
        }
        for category in previous_category_stats.keys() {
            categories.insert(category.clone(), ());
        }
        for category in categories.keys() {
            let recent = recent_category_stats
                .get(category)
                .cloned()
                .unwrap_or_default();
            let previous = previous_category_stats
                .get(category)
                .cloned()
                .unwrap_or_default();
            out.push_str(&format!(
                "| {} | {} | {} | {} | {} | {:+.1} | {:.2} | {:.2} | {:+.2} | {} | {} |\n",
                escape_table(category),
                recent.runs,
                previous.runs,
                rate_line(recent.success, recent.runs),
                rate_line(previous.success, previous.runs),
                rate_percent(recent.success, recent.runs)
                    - rate_percent(previous.success, previous.runs),
                avg_tool_calls(&recent),
                avg_tool_calls(&previous),
                avg_tool_calls(&recent) - avg_tool_calls(&previous),
                recent.seed_candidates,
                previous.seed_candidates,
            ));
        }
        out.push('\n');
    }
    out.push_str("| Timestamp | Category | Outcome | Manual | Budget | Tool Calls | Failed Tools | Workdir | Task | Notes |\n");
    out.push_str("| --- | --- | --- | --- | ---: | ---: | ---: | --- | --- | --- |\n");
    for record in records.iter().rev().take(limit) {
        out.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |\n",
            record.timestamp_secs,
            escape_table(&benchmark_case_category(record)),
            report_outcome_label(record),
            if record.manual_intervention {
                "yes"
            } else {
                "no"
            },
            record.budget,
            record.tool_calls,
            record.failed_tool_calls,
            escape_table(&clip(&record.workdir, 36)),
            escape_table(&clip(&record.task, 56)),
            escape_table(&clip(record.notes.as_deref().unwrap_or(""), 48)),
        ));
    }
    out
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct DogfoodCategoryStats {
    runs: usize,
    success: usize,
    diagnostic: usize,
    failed: usize,
    stuck: usize,
    manual: usize,
    total_tool_calls: u64,
    seed_candidates: usize,
}

fn aggregate_category_stats(records: &[DogfoodRecord]) -> BTreeMap<String, DogfoodCategoryStats> {
    let mut category_stats = BTreeMap::<String, DogfoodCategoryStats>::new();
    for record in records {
        let category = benchmark_case_category(record).to_string();
        let stats = category_stats.entry(category).or_default();
        stats.runs += 1;
        stats.total_tool_calls += record.tool_calls;
        if record.manual_intervention {
            stats.manual += 1;
        }
        if is_benchmark_seed_candidate(record, None) {
            stats.seed_candidates += 1;
        }
        if record.diagnostic_expected_failure {
            stats.diagnostic += 1;
        }
        match record.outcome {
            DogfoodOutcome::Success => stats.success += 1,
            DogfoodOutcome::Failed => stats.failed += 1,
            DogfoodOutcome::Stuck => stats.stuck += 1,
            DogfoodOutcome::Manual => {}
        }
    }
    category_stats
}

fn report_outcome_label(record: &DogfoodRecord) -> &'static str {
    if record.diagnostic_expected_failure {
        "diagnostic"
    } else {
        record.outcome.label()
    }
}

fn avg_tool_calls(stats: &DogfoodCategoryStats) -> f64 {
    if stats.runs == 0 {
        0.0
    } else {
        stats.total_tool_calls as f64 / stats.runs as f64
    }
}

fn rate_percent(count: usize, total: usize) -> f64 {
    if total == 0 {
        0.0
    } else {
        (count as f64 / total as f64) * 100.0
    }
}

fn rate_line(count: usize, total: usize) -> String {
    if total == 0 {
        return "0/0 (0.0%)".to_string();
    }
    format!(
        "{count}/{total} ({:.1}%)",
        (count as f64 / total as f64) * 100.0
    )
}

fn serialize_seed_observations(events: &[crate::core::loop_runtime::ToolEvent]) -> Option<String> {
    if events.is_empty() {
        return None;
    }
    let entries = events
        .iter()
        .rev()
        .take(3)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .map(|event| {
            format!(
                "{}:{}:{}",
                event.tool_name,
                if matches!(event.status, ObservationStatus::Failed) {
                    "failed"
                } else {
                    "ok"
                },
                clip(&event.output, 600)
            )
        })
        .collect::<Vec<_>>();
    Some(entries.join(" || "))
}

fn is_benchmark_seed_candidate(
    record: &DogfoodRecord,
    outcome_filter: Option<DogfoodOutcome>,
) -> bool {
    if let Some(filter) = outcome_filter {
        if record.outcome != filter {
            return false;
        }
    } else if matches!(record.outcome, DogfoodOutcome::Success) {
        return false;
    }

    record
        .benchmark_seed_observations
        .as_deref()
        .map(str::trim)
        .is_some_and(|value| !value.is_empty())
}

fn render_benchmark_seed_export(
    records: &[DogfoodRecord],
    limit: usize,
    outcome_filter: Option<DogfoodOutcome>,
    repo_root: &Path,
) -> String {
    let mut out = String::new();
    out.push_str("# Generated benchmark seed cases from dogfood ledger\n");
    out.push_str("# Review and curate before appending to .dscode/benchmarks.txt.\n\n");

    let mut emitted = 0usize;
    for record in records.iter().rev() {
        if emitted >= limit || !is_benchmark_seed_candidate(record, outcome_filter) {
            continue;
        }
        let Some(seed_observations) = record.benchmark_seed_observations.as_deref() else {
            continue;
        };
        out.push_str(&format!(
            "# outcome={} timestamp={} tool_trace={} final_message={}\n",
            record.outcome.label(),
            record.timestamp_secs,
            clip(&record.tool_trace, 80),
            clip(&record.final_message, 96),
        ));
        out.push_str(&format!("name = \"{}\"\n", benchmark_case_name(record)));
        out.push_str(&format!("task = \"{}\"\n", manifest_escape(&record.task)));
        out.push_str(&format!(
            "category = \"{}\"\n",
            manifest_escape(&benchmark_case_category(record))
        ));
        if let Some(skill) = record.skill.as_deref() {
            out.push_str(&format!("skill = \"{}\"\n", manifest_escape(skill)));
        }
        if let Some(workdir) = manifest_workdir(&record.workdir, repo_root) {
            out.push_str(&format!("workdir = \"{}\"\n", manifest_escape(&workdir)));
        }
        out.push_str(&format!("budget = {}\n", record.budget));
        out.push_str(&format!(
            "notes = \"{}\"\n",
            manifest_escape(&format!(
                "Generated from dogfood outcome={} trace={}{}",
                record.outcome.label(),
                clip(&record.tool_trace, 80),
                record
                    .notes
                    .as_deref()
                    .map(|value| format!(" notes={}", clip(value, 48)))
                    .unwrap_or_default()
            ))
        ));
        out.push_str(&format!(
            "seed_observations = \"{}\"\n\n",
            manifest_escape(seed_observations)
        ));
        emitted += 1;
    }

    if emitted == 0 {
        out.push_str("# No matching failed/stuck/manual runs with replayable seed observations were found.\n");
    }

    out
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PromotedBenchmarkCase {
    name: String,
    block: String,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct PromotionPlan {
    cases: Vec<PromotedBenchmarkCase>,
    duplicates_skipped: usize,
    policy_skipped: usize,
    policy_skip_reasons: Vec<PolicySkipReasonCount>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PolicySkipReasonCount {
    reason_code: &'static str,
    reason_label: &'static str,
    count: usize,
    example_task: String,
}

fn build_promotion_plan(
    records: &[DogfoodRecord],
    existing: &[BenchmarkCaseSummary],
    limit: usize,
    outcome_filter: Option<DogfoodOutcome>,
    repo_root: &Path,
) -> PromotionPlan {
    let mut existing_names = existing
        .iter()
        .map(|case| case.name.clone())
        .collect::<HashSet<_>>();
    let mut existing_keys = existing
        .iter()
        .map(benchmark_summary_key)
        .collect::<HashSet<_>>();
    let mut cases = Vec::new();
    let mut duplicates_skipped = 0usize;
    let mut policy_skipped = 0usize;
    let mut policy_skip_reasons = BTreeMap::<&'static str, PolicySkipReasonCount>::new();

    for record in records.iter().rev() {
        if cases.len() >= limit || !is_benchmark_seed_candidate(record, outcome_filter) {
            continue;
        }
        if let Some(reason) = promotion_policy_rejection(record, outcome_filter) {
            policy_skipped += 1;
            policy_skip_reasons
                .entry(reason.code)
                .and_modify(|summary| summary.count += 1)
                .or_insert_with(|| PolicySkipReasonCount {
                    reason_code: reason.code,
                    reason_label: reason.label,
                    count: 1,
                    example_task: record.task.clone(),
                });
            continue;
        }
        let Some(seed_observations) = record.benchmark_seed_observations.as_deref() else {
            continue;
        };
        let skill = record.skill.clone();
        let workdir = manifest_workdir(&record.workdir, repo_root);
        let category = benchmark_case_category(record);
        let key = benchmark_identity_key(
            &record.task,
            Some(category.as_ref()),
            skill.as_deref(),
            workdir.as_deref(),
            seed_observations,
        );
        if existing_keys.contains(&key) {
            duplicates_skipped += 1;
            continue;
        }
        existing_keys.insert(key);
        let name = unique_benchmark_case_name(benchmark_case_name(record), &mut existing_names);
        cases.push(PromotedBenchmarkCase {
            block: render_promoted_case_block(record, &name, workdir.as_deref(), seed_observations),
            name,
        });
    }

    PromotionPlan {
        cases,
        duplicates_skipped,
        policy_skipped,
        policy_skip_reasons: policy_skip_reasons.into_values().collect(),
    }
}

fn append_promoted_cases(path: &Path, cases: &[PromotedBenchmarkCase]) -> AppResult<()> {
    if cases.is_empty() {
        return Ok(());
    }
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|error| {
            app_error(format!(
                "failed to open benchmark manifest {} for append: {error}",
                path.display()
            ))
        })?;
    let needs_separator = fs::metadata(path)
        .map(|metadata| metadata.len() > 0)
        .unwrap_or(false);
    if needs_separator {
        writeln!(file)?;
    }
    for (index, case) in cases.iter().enumerate() {
        if index > 0 {
            writeln!(file)?;
        }
        write!(file, "{}", case.block)?;
    }
    Ok(())
}

fn render_promoted_case_block(
    record: &DogfoodRecord,
    name: &str,
    workdir: Option<&str>,
    seed_observations: &str,
) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "# promoted from dogfood outcome={} timestamp={} tool_trace={} final_message={}\n",
        record.outcome.label(),
        record.timestamp_secs,
        clip(&record.tool_trace, 80),
        clip(&record.final_message, 96),
    ));
    out.push_str(&format!("name = \"{}\"\n", manifest_escape(name)));
    out.push_str(&format!("task = \"{}\"\n", manifest_escape(&record.task)));
    out.push_str(&format!(
        "category = \"{}\"\n",
        manifest_escape(&benchmark_case_category(record))
    ));
    if let Some(skill) = record.skill.as_deref() {
        out.push_str(&format!("skill = \"{}\"\n", manifest_escape(skill)));
    }
    if let Some(workdir) = workdir {
        out.push_str(&format!("workdir = \"{}\"\n", manifest_escape(workdir)));
    }
    out.push_str(&format!("budget = {}\n", record.budget));
    out.push_str(&format!(
        "notes = \"{}\"\n",
        manifest_escape(&format!(
            "Promoted from dogfood outcome={} trace={}{}",
            record.outcome.label(),
            clip(&record.tool_trace, 80),
            record
                .notes
                .as_deref()
                .map(|value| format!(" notes={}", clip(value, 48)))
                .unwrap_or_default()
        ))
    ));
    out.push_str(&format!(
        "seed_observations = \"{}\"\n",
        manifest_escape(seed_observations)
    ));
    out
}

fn benchmark_case_name(record: &DogfoodRecord) -> String {
    let slug = slugify(&record.task, 32);
    format!("dogfood-{}-{}", record.outcome.label(), slug)
}

fn benchmark_case_category(record: &DogfoodRecord) -> Cow<'_, str> {
    resolved_benchmark_category(
        record.benchmark_category.as_deref(),
        &record.task,
        &record.tool_trace,
        record.failed_tool_calls,
        record.repeated_call_failures,
        record.used_subagent,
        record.benchmark_seed_observations.as_deref(),
    )
}

pub(crate) fn resolved_benchmark_category<'a>(
    stored: Option<&'a str>,
    task: &'a str,
    tool_trace: &'a str,
    failed_tool_calls: u64,
    repeated_call_failures: u64,
    used_subagent: bool,
    benchmark_seed_observations: Option<&'a str>,
) -> Cow<'a, str> {
    let inferred = infer_benchmark_category(
        task,
        tool_trace,
        failed_tool_calls,
        repeated_call_failures,
        used_subagent,
        benchmark_seed_observations,
    );
    match stored {
        Some("read_only") if inferred == "recovery" => Cow::Borrowed(inferred),
        Some("write_validate") if inferred == "recovery" && !tool_trace.contains("apply_patch") => {
            Cow::Borrowed(inferred)
        }
        Some("planning") if inferred != "planning" && !task_looks_like_planning(task) => {
            Cow::Borrowed(inferred)
        }
        Some(stored) => Cow::Borrowed(stored),
        None => Cow::Borrowed(inferred),
    }
}

pub(crate) fn infer_benchmark_category<'a>(
    task: &'a str,
    tool_trace: &'a str,
    failed_tool_calls: u64,
    repeated_call_failures: u64,
    used_subagent: bool,
    benchmark_seed_observations: Option<&'a str>,
) -> &'static str {
    let task_lower = task.to_ascii_lowercase();
    let trace_lower = tool_trace.to_ascii_lowercase();
    if task_looks_like_pr_workflow(&task_lower) {
        return "pr_workflow";
    }
    if benchmark_seed_observations.is_some_and(|seed| seed.contains("recovery_hint:"))
        || repeated_call_failures > 0
        || task_looks_like_recovery(&task_lower)
        || task_looks_like_failure_diagnosis(&task_lower)
    {
        return "recovery";
    }
    if task_looks_like_write_validate(&task_lower)
        || trace_lower.contains("apply_patch")
        || trace_lower.contains("run_shell")
    {
        return "write_validate";
    }
    if failed_tool_calls > 0 {
        return "recovery";
    }
    if task_looks_like_subagent(&task_lower) {
        return "subagent";
    }
    if task_looks_like_planning(&task_lower)
        || (trace_lower.contains("todo_write")
            && !trace_lower.contains("read_file")
            && !trace_lower.contains("list_files")
            && !trace_lower.contains("search_text")
            && !trace_lower.contains("dispatch_subagent")
            && !used_subagent)
    {
        return "planning";
    }
    "read_only"
}

fn task_looks_like_planning(task: &str) -> bool {
    let task_lower = task.to_ascii_lowercase();
    task_lower.starts_with("plan ")
        || task_lower.contains(" plan ")
        || task_lower.contains(" planning")
        || task_lower.contains("before acting")
        || task_lower.contains("execution steps")
        || task_lower.contains("step-by-step plan")
}

fn task_looks_like_subagent(task: &str) -> bool {
    task.contains("dispatch_subagent")
        || task.contains("subagent")
        || task.contains("child loop")
        || task.contains("parent loop")
}

fn task_looks_like_recovery(task: &str) -> bool {
    task.contains("if there are no matches")
        || task.contains("if there are no match")
        || task.contains("if no matches")
        || task.contains("before retrying the command")
        || task.contains("before retrying the read")
        || task.contains("broaden the lookup")
}

fn task_looks_like_write_validate(task: &str) -> bool {
    let has_explicit_edit =
        task.contains("replace ") && task.contains(" with ") && task.contains(" in ");
    let asks_validation = task.contains("validate")
        || task.contains("rerun")
        || task.contains("tests pass")
        || task.contains("cargo test")
        || task.contains("pytest")
        || task.contains("npm test");
    has_explicit_edit && asks_validation
}

fn task_looks_like_failure_diagnosis(task: &str) -> bool {
    let task_lower = task.to_ascii_lowercase();
    task_lower.contains("investigate why")
        || task_lower.contains("diagnose")
        || task_lower.contains("reproduce")
        || task_lower.contains("inspect the failing")
        || task_lower.contains("before retrying")
}

fn task_looks_like_pr_workflow(task: &str) -> bool {
    task.contains("pull request")
        || task.contains("review feedback")
        || task.contains("ci job")
        || task.contains("failed ci")
        || task.contains("pr #")
        || task.contains("github pr")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PolicyRejection {
    code: &'static str,
    label: &'static str,
}

fn promotion_policy_rejection(
    record: &DogfoodRecord,
    outcome_filter: Option<DogfoodOutcome>,
) -> Option<PolicyRejection> {
    if record.tool_trace == "none" || record.tool_calls == 0 {
        return Some(PolicyRejection {
            code: "no_tool_trace",
            label: "missing real tool trace",
        });
    }
    if record.tool_calls > 8 {
        return Some(PolicyRejection {
            code: "tool_trace_too_long",
            label: "tool trace too long (>8 calls)",
        });
    }

    if outcome_filter.is_none()
        && !matches!(
            record.outcome,
            DogfoodOutcome::Failed | DogfoodOutcome::Stuck
        )
    {
        return Some(PolicyRejection {
            code: "manual_requires_explicit_filter",
            label: "manual outcome requires --outcome manual",
        });
    }

    if !(record.failed_tool_calls > 0
        || record.repeated_call_failures > 0
        || record.manual_intervention)
    {
        return Some(PolicyRejection {
            code: "missing_failure_signal",
            label: "missing failed/stuck/manual signal",
        });
    }

    None
}

fn unique_benchmark_case_name(base: String, existing_names: &mut HashSet<String>) -> String {
    if existing_names.insert(base.clone()) {
        return base;
    }
    for suffix in 2..=9999 {
        let candidate = format!("{base}-{suffix}");
        if existing_names.insert(candidate.clone()) {
            return candidate;
        }
    }
    format!("{base}-overflow")
}

fn benchmark_summary_key(case: &BenchmarkCaseSummary) -> String {
    benchmark_identity_key(
        &case.task,
        Some(case.category.as_str()),
        case.skill.as_deref(),
        case.workdir.as_deref(),
        case.seed_observations.as_deref().unwrap_or(""),
    )
}

fn benchmark_identity_key(
    task: &str,
    category: Option<&str>,
    skill: Option<&str>,
    workdir: Option<&str>,
    seed_observations: &str,
) -> String {
    format!(
        "{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}",
        task.trim(),
        category.unwrap_or(""),
        skill.unwrap_or(""),
        workdir.unwrap_or(""),
        seed_observations.trim()
    )
}

fn slugify(text: &str, max_len: usize) -> String {
    let mut out = String::new();
    let mut last_was_dash = false;
    for ch in text.chars() {
        let mapped = if ch.is_ascii_alphanumeric() {
            Some(ch.to_ascii_lowercase())
        } else if ch.is_whitespace() || matches!(ch, '-' | '_' | '/' | ':') {
            Some('-')
        } else {
            None
        };
        let Some(ch) = mapped else {
            continue;
        };
        if ch == '-' {
            if out.is_empty() || last_was_dash {
                continue;
            }
            last_was_dash = true;
        } else {
            last_was_dash = false;
        }
        out.push(ch);
        if out.len() >= max_len {
            break;
        }
    }
    out.trim_matches('-').to_string()
}

fn manifest_workdir(workdir: &str, repo_root: &Path) -> Option<String> {
    let path = Path::new(workdir);
    if path == repo_root {
        return None;
    }
    if let Ok(relative) = path.strip_prefix(repo_root) {
        let value = relative.display().to_string();
        if value.is_empty() || value == "." {
            None
        } else {
            Some(value)
        }
    } else if path.is_relative() {
        Some(workdir.to_string())
    } else {
        None
    }
}

fn manifest_escape(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('\n', "\\n")
        .replace('"', "\\\"")
}

fn read_string<'a>(root: &'a BTreeMap<String, JsonValue>, key: &str) -> AppResult<&'a str> {
    root.get(key)
        .and_then(json_as_string)
        .ok_or_else(|| app_error(format!("dogfood record missing string `{key}`")))
}

fn read_optional_string<'a>(root: &'a BTreeMap<String, JsonValue>, key: &str) -> Option<&'a str> {
    match root.get(key) {
        Some(JsonValue::Null) | None => None,
        Some(value) => json_as_string(value),
    }
}

fn read_u64(root: &BTreeMap<String, JsonValue>, key: &str) -> AppResult<u64> {
    root.get(key)
        .and_then(json_as_u64)
        .ok_or_else(|| app_error(format!("dogfood record missing numeric `{key}`")))
}

fn read_bool(root: &BTreeMap<String, JsonValue>, key: &str) -> AppResult<bool> {
    match root.get(key) {
        Some(JsonValue::Bool(value)) => Ok(*value),
        _ => Err(app_error(format!("dogfood record missing boolean `{key}`"))),
    }
}

fn read_optional_bool(root: &BTreeMap<String, JsonValue>, key: &str) -> Option<bool> {
    match root.get(key) {
        Some(JsonValue::Bool(value)) => Some(*value),
        _ => None,
    }
}

fn unix_now_secs() -> AppResult<u64> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| app_error(format!("system clock error: {error}")))?
        .as_secs())
}

fn first_non_empty_line(text: &str) -> Option<&str> {
    text.lines().map(str::trim).find(|line| !line.is_empty())
}

fn clip(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_string();
    }
    let head: String = value.chars().take(max_chars).collect();
    format!("{head}…")
}

fn escape_table(value: &str) -> String {
    value.replace('|', "\\|")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::loop_runtime::ToolEvent;
    use crate::model::protocol::TokenUsage;
    use std::fs;

    fn temp_test_dir(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        std::env::temp_dir().join(format!("deepseek-dogfood-{name}-{nanos}"))
    }

    #[test]
    fn derive_default_outcome_prefers_stuck_over_failed() {
        assert!(matches!(
            derive_default_outcome(1, 1, false, false),
            DogfoodOutcome::Stuck
        ));
        assert!(matches!(
            derive_default_outcome(1, 0, false, false),
            DogfoodOutcome::Failed
        ));
        assert!(matches!(
            derive_default_outcome(0, 0, false, false),
            DogfoodOutcome::Success
        ));
    }

    #[test]
    fn derive_default_outcome_treats_expected_failure_diagnosis_as_success() {
        assert!(matches!(
            derive_default_outcome(1, 0, true, false),
            DogfoodOutcome::Success
        ));
    }

    #[test]
    fn derive_default_outcome_treats_recovered_validation_as_success() {
        assert!(matches!(
            derive_default_outcome(1, 0, false, true),
            DogfoodOutcome::Success
        ));
        assert!(matches!(
            derive_default_outcome(1, 1, false, true),
            DogfoodOutcome::Success
        ));
    }

    #[test]
    fn resolve_run_workdir_defaults_to_repo_root() {
        let repo_root = temp_test_dir("repo-root-default");
        fs::create_dir_all(&repo_root).expect("repo root");

        let resolved = super::resolve_run_workdir(&repo_root, None).expect("repo root");
        assert_eq!(resolved, repo_root);

        fs::remove_dir_all(&resolved).ok();
    }

    #[test]
    fn resolve_run_workdir_resolves_relative_path_under_repo_root() {
        let repo_root = temp_test_dir("repo-root-relative");
        let fixture = repo_root.join("fixtures").join("mini");
        fs::create_dir_all(&fixture).expect("fixture dir");

        let resolved =
            super::resolve_run_workdir(&repo_root, Some("fixtures/mini")).expect("fixture path");
        assert_eq!(resolved, fixture);

        fs::remove_dir_all(&repo_root).ok();
    }

    #[test]
    fn resolve_run_workdir_rejects_missing_directory() {
        let repo_root = temp_test_dir("repo-root-missing");
        fs::create_dir_all(&repo_root).expect("repo root");

        let error = super::resolve_run_workdir(&repo_root, Some("fixtures/missing")).unwrap_err();
        assert!(error
            .to_string()
            .contains("dogfood workdir does not exist or is not a directory"));

        fs::remove_dir_all(&repo_root).ok();
    }

    #[test]
    fn record_json_round_trip_preserves_key_fields() {
        let record = DogfoodRecord {
            version: 1,
            timestamp_secs: 42,
            duration_ms: 88,
            task: "inspect planner".to_string(),
            skill: Some("research".to_string()),
            budget: 6,
            model: "deepseek-v4-pro".to_string(),
            workdir: "/tmp/demo".to_string(),
            outcome: DogfoodOutcome::Manual,
            manual_intervention: true,
            notes: Some("needed one retry".to_string()),
            tool_calls: 4,
            failed_tool_calls: 1,
            repeated_call_failures: 0,
            diagnostic_expected_failure: false,
            used_subagent: true,
            final_message: "done".to_string(),
            tool_trace: "todo_write -> search_text".to_string(),
            error_kind: Some("tool_failure".to_string()),
            benchmark_category: Some("planning".to_string()),
            benchmark_seed_observations: Some("search_text:failed:no matches".to_string()),
        };
        let decoded = DogfoodRecord::from_json_line(&record.to_json_line()).unwrap();
        assert_eq!(decoded.task, "inspect planner");
        assert_eq!(decoded.skill.as_deref(), Some("research"));
        assert!(matches!(decoded.outcome, DogfoodOutcome::Manual));
        assert!(decoded.manual_intervention);
        assert_eq!(decoded.tool_trace, "todo_write -> search_text");
        assert_eq!(decoded.error_kind.as_deref(), Some("tool_failure"));
        assert!(!decoded.diagnostic_expected_failure);
        assert_eq!(decoded.benchmark_category.as_deref(), Some("recovery"));
        assert_eq!(
            decoded.benchmark_seed_observations.as_deref(),
            Some("search_text:failed:no matches")
        );
    }

    #[test]
    fn record_json_round_trip_backfills_category_for_legacy_rows() {
        let decoded = DogfoodRecord::from_json_line(
            "{\"version\":1,\"timestamp_secs\":42,\"duration_ms\":88,\"task\":\"inspect planner\",\"skill\":\"research\",\"budget\":6,\"model\":\"deepseek-v4-pro\",\"workdir\":\"/tmp/demo\",\"outcome\":\"manual\",\"manual_intervention\":true,\"notes\":\"needed one retry\",\"tool_calls\":4,\"failed_tool_calls\":1,\"repeated_call_failures\":0,\"used_subagent\":true,\"final_message\":\"done\",\"tool_trace\":\"todo_write -> search_text\",\"error_kind\":\"tool_failure\",\"benchmark_seed_observations\":\"search_text:failed:no matches\"}"
        )
        .unwrap();
        assert_eq!(decoded.benchmark_category.as_deref(), Some("recovery"));
    }

    #[test]
    fn render_report_includes_core_rates() {
        let records = vec![
            DogfoodRecord {
                version: 1,
                timestamp_secs: 1,
                duration_ms: 10,
                task: "one".to_string(),
                skill: None,
                budget: 4,
                model: "x".to_string(),
                workdir: ".".to_string(),
                outcome: DogfoodOutcome::Success,
                manual_intervention: false,
                notes: None,
                tool_calls: 2,
                failed_tool_calls: 0,
                repeated_call_failures: 0,
                diagnostic_expected_failure: false,
                used_subagent: false,
                final_message: "ok".to_string(),
                tool_trace: "list_files".to_string(),
                error_kind: None,
                benchmark_category: Some("read_only".to_string()),
                benchmark_seed_observations: None,
            },
            DogfoodRecord {
                version: 1,
                timestamp_secs: 2,
                duration_ms: 12,
                task: "two".to_string(),
                skill: None,
                budget: 4,
                model: "x".to_string(),
                workdir: ".".to_string(),
                outcome: DogfoodOutcome::Stuck,
                manual_intervention: true,
                notes: Some("needed manual help".to_string()),
                tool_calls: 3,
                failed_tool_calls: 1,
                repeated_call_failures: 1,
                diagnostic_expected_failure: false,
                used_subagent: false,
                final_message: "stuck".to_string(),
                tool_trace: "list_files -> list_files".to_string(),
                error_kind: None,
                benchmark_category: Some("recovery".to_string()),
                benchmark_seed_observations: Some(
                    "list_files:failed:repeated identical tool call detected".to_string(),
                ),
            },
        ];
        let report = render_report(Path::new(".dscode/dogfood/ledger.jsonl"), &records, 20);
        assert!(report.contains("# DeepseekCode Dogfood Report"));
        assert!(report.contains("Success rate: 1/2 (50.0%)"));
        assert!(report.contains("Diagnostic expected-failure rate: 0/2 (0.0%)"));
        assert!(report.contains("Stuck rate: 1/2 (50.0%)"));
        assert!(report.contains("Manual intervention rate: 1/2 (50.0%)"));
        assert!(report.contains("Benchmark seed candidates: 1"));
        assert!(report.contains("## Category Breakdown"));
        assert!(report.contains("## Category Trend"));
        assert!(report.contains("Status: insufficient history"));
        assert!(report.contains("| read_only | 1 | 1/1 (100.0%) | 0/1 (0.0%) | 0/1 (0.0%) | 0/1 (0.0%) | 0/1 (0.0%) | 2.00 | 0 |"));
        assert!(report.contains("| recovery | 1 | 0/1 (0.0%) | 0/1 (0.0%) | 0/1 (0.0%) | 1/1 (100.0%) | 1/1 (100.0%) | 3.00 | 1 |"));
        assert!(report.contains("| Timestamp | Category | Outcome | Manual | Budget | Tool Calls | Failed Tools | Workdir | Task | Notes |"));
    }

    #[test]
    fn render_report_includes_category_trend_deltas() {
        let mut records = Vec::new();
        for index in 0..10u64 {
            let recent = index >= 5;
            let success = recent || index < 3;
            let tool_calls = if recent { 2 } else { 4 };
            records.push(DogfoodRecord {
                version: 1,
                timestamp_secs: index + 1,
                duration_ms: 10,
                task: format!("inspect file {index}"),
                skill: None,
                budget: 4,
                model: "x".to_string(),
                workdir: ".".to_string(),
                outcome: if success {
                    DogfoodOutcome::Success
                } else {
                    DogfoodOutcome::Failed
                },
                manual_intervention: false,
                notes: None,
                tool_calls,
                failed_tool_calls: 0,
                repeated_call_failures: 0,
                diagnostic_expected_failure: false,
                used_subagent: false,
                final_message: "done".to_string(),
                tool_trace: "list_files -> read_file".to_string(),
                error_kind: None,
                benchmark_category: Some("read_only".to_string()),
                benchmark_seed_observations: None,
            });
        }
        let report = render_report(Path::new(".dscode/dogfood/ledger.jsonl"), &records, 20);
        assert!(report.contains("Compared windows: recent=5 previous=5"));
        assert!(report.contains("| read_only | 5 | 5 | 5/5 (100.0%) | 3/5 (60.0%) | +40.0 | 2.00 | 4.00 | -2.00 | 0 | 0 |"));
    }

    #[test]
    fn render_report_labels_expected_failure_diagnosis_separately() {
        let records = vec![DogfoodRecord {
            version: 1,
            timestamp_secs: 7,
            duration_ms: 11,
            task: "investigate why npm test fails in the JavaScript CLI and inspect the failing test file before retrying".to_string(),
            skill: Some("debug".to_string()),
            budget: 4,
            model: "x".to_string(),
            workdir: ".".to_string(),
            outcome: DogfoodOutcome::Success,
            manual_intervention: false,
            notes: Some("diagnosis only".to_string()),
            tool_calls: 2,
            failed_tool_calls: 1,
            repeated_call_failures: 0,
            diagnostic_expected_failure: true,
            used_subagent: false,
            final_message: "read back the failing test file".to_string(),
            tool_trace: "run_shell -> read_file".to_string(),
            error_kind: None,
            benchmark_category: Some("recovery".to_string()),
            benchmark_seed_observations: Some(
                "run_shell:ok:meta.result=failed || read_file:ok:test('route benchmark stays stable')"
                    .to_string(),
            ),
        }];

        let report = render_report(Path::new(".dscode/dogfood/ledger.jsonl"), &records, 20);
        assert!(report.contains("Diagnostic expected-failure rate: 1/1 (100.0%)"));
        assert!(report.contains("| recovery | 1 | 1/1 (100.0%) | 1/1 (100.0%) | 0/1 (0.0%) | 0/1 (0.0%) | 0/1 (0.0%) | 2.00 | 0 |"));
        assert!(report.contains("| 7 | recovery | diagnostic | no | 4 | 2 | 1 | . | investigate why npm test fails in the JavaScript CLI"));
    }

    #[test]
    fn infer_benchmark_category_keeps_read_only_tasks_out_of_planning() {
        let category = infer_benchmark_category(
            "inspect repository layout and summarize the main entrypoints",
            "todo_write -> dispatch_subagent -> list_files -> read_file",
            0,
            0,
            true,
            None,
        );
        assert_eq!(category, "read_only");
    }

    #[test]
    fn benchmark_case_category_corrects_stale_planning_label_for_read_only_task() {
        let record = DogfoodRecord {
            version: 1,
            timestamp_secs: 1,
            duration_ms: 10,
            task: "inspect repository layout and summarize the main entrypoints".to_string(),
            skill: None,
            budget: 4,
            model: "x".to_string(),
            workdir: ".".to_string(),
            outcome: DogfoodOutcome::Success,
            manual_intervention: false,
            notes: None,
            tool_calls: 4,
            failed_tool_calls: 0,
            repeated_call_failures: 0,
            diagnostic_expected_failure: false,
            used_subagent: true,
            final_message: "ok".to_string(),
            tool_trace: "todo_write -> dispatch_subagent -> list_files -> read_file".to_string(),
            error_kind: None,
            benchmark_category: Some("planning".to_string()),
            benchmark_seed_observations: None,
        };
        assert_eq!(benchmark_case_category(&record), "read_only");
    }

    #[test]
    fn benchmark_case_category_corrects_stale_read_only_label_for_natural_recovery_task() {
        let record = DogfoodRecord {
            version: 1,
            timestamp_secs: 1,
            duration_ms: 10,
            task: "find where `missing_fixture_symbol_js_20260509` is implemented, and if there are no matches inspect the repository layout instead".to_string(),
            skill: None,
            budget: 6,
            model: "x".to_string(),
            workdir: ".".to_string(),
            outcome: DogfoodOutcome::Success,
            manual_intervention: false,
            notes: None,
            tool_calls: 4,
            failed_tool_calls: 0,
            repeated_call_failures: 0,
            diagnostic_expected_failure: false,
            used_subagent: false,
            final_message: "ok".to_string(),
            tool_trace: "todo_write -> search_text -> list_files -> read_file".to_string(),
            error_kind: None,
            benchmark_category: Some("read_only".to_string()),
            benchmark_seed_observations: None,
        };
        assert_eq!(benchmark_case_category(&record), "recovery");
    }

    #[test]
    fn benchmark_case_category_corrects_stale_write_validate_label_for_failure_repro_task() {
        let record = DogfoodRecord {
            version: 1,
            timestamp_secs: 1,
            duration_ms: 10,
            task: "investigate why npm test fails in the JavaScript CLI and inspect the failing test file before retrying".to_string(),
            skill: Some("debug".to_string()),
            budget: 4,
            model: "x".to_string(),
            workdir: ".".to_string(),
            outcome: DogfoodOutcome::Failed,
            manual_intervention: false,
            notes: None,
            tool_calls: 2,
            failed_tool_calls: 1,
            repeated_call_failures: 0,
            diagnostic_expected_failure: false,
            used_subagent: false,
            final_message: "stopped after readback".to_string(),
            tool_trace: "run_shell -> read_file".to_string(),
            error_kind: Some("tool_failure".to_string()),
            benchmark_category: Some("write_validate".to_string()),
            benchmark_seed_observations: None,
        };
        assert_eq!(benchmark_case_category(&record), "recovery");
    }

    #[test]
    fn infer_benchmark_category_keeps_plan_only_tasks_as_planning() {
        let category = infer_benchmark_category(
            "plan an end-to-end improvement for benchmark reliability and report the execution steps before acting",
            "todo_write",
            0,
            0,
            false,
            None,
        );
        assert_eq!(category, "planning");
    }

    #[test]
    fn infer_benchmark_category_prefers_pr_workflow_for_pull_request_tasks() {
        let category = infer_benchmark_category(
            "Review pull request #42 and fix the failed CI job",
            "run_shell -> apply_patch",
            1,
            0,
            false,
            None,
        );
        assert_eq!(category, "pr_workflow");
    }

    #[test]
    fn infer_benchmark_category_marks_natural_search_fallback_as_recovery() {
        let category = infer_benchmark_category(
            "find where `missing_fixture_symbol_js_20260509` is implemented, and if there are no matches inspect the repository layout instead",
            "todo_write -> search_text -> list_files -> read_file",
            0,
            0,
            false,
            None,
        );
        assert_eq!(category, "recovery");
    }

    #[test]
    fn infer_benchmark_category_keeps_recovered_edit_retry_as_write_validate() {
        let category = infer_benchmark_category(
            "replace `a - b` with `a * b` in src/math_ops.py and validate with pytest until the tests pass",
            "apply_patch -> git_diff -> run_shell -> read_file -> apply_patch -> git_diff -> run_shell",
            1,
            0,
            false,
            None,
        );
        assert_eq!(category, "write_validate");
    }

    #[test]
    fn from_result_derives_metrics_from_tool_events() {
        let result = RunResult {
            final_message: "done".to_string(),
            tool_events: vec![
                ToolEvent {
                    tool_name: "todo_write".to_string(),
                    input: BTreeMap::new(),
                    output: "ok".to_string(),
                    status: ObservationStatus::Ok,
                },
                ToolEvent {
                    tool_name: "dispatch_subagent".to_string(),
                    input: BTreeMap::new(),
                    output: "repeated identical tool call detected".to_string(),
                    status: ObservationStatus::Failed,
                },
            ],
            usage: TokenUsage::default(),
        };
        let args = DogfoodRunArgs {
            task: "debug parser".to_string(),
            from_benchmark: None,
            benchmark_manifest: None,
            skill: None,
            budget: Some(4),
            workdir: None,
            isolate_workdir: false,
            outcome: None,
            manual_intervention: false,
            benchmark_gate: false,
            notes: None,
        };
        let record = DogfoodRecord::from_result(
            1,
            10,
            "deepseek-v4-pro".to_string(),
            ".".to_string(),
            4,
            &args,
            false,
            &result,
        );
        assert!(matches!(record.outcome, DogfoodOutcome::Stuck));
        assert!(!record.diagnostic_expected_failure);
        assert!(record.used_subagent);
        assert_eq!(record.failed_tool_calls, 1);
        assert!(record
            .benchmark_seed_observations
            .as_deref()
            .unwrap_or("")
            .contains("dispatch_subagent:failed"));
    }

    #[test]
    fn from_result_treats_meta_result_failed_as_failed_outcome() {
        let result = RunResult {
            final_message: "read back the failing file".to_string(),
            tool_events: vec![
                ToolEvent {
                    tool_name: "run_shell".to_string(),
                    input: BTreeMap::new(),
                    output: "meta.command_kind=test\nmeta.exit_code=101\nmeta.result=failed\nmeta.failure_kind=test_failure".to_string(),
                    status: ObservationStatus::Ok,
                },
                ToolEvent {
                    tool_name: "read_file".to_string(),
                    input: BTreeMap::new(),
                    output: "1 pub fn add(a: i32, b: i32) -> i32 {".to_string(),
                    status: ObservationStatus::Ok,
                },
            ],
            usage: TokenUsage::default(),
        };
        let args = DogfoodRunArgs {
            task: "replace `a - b` with `a * b` in src/lib.rs and validate with cargo test"
                .to_string(),
            from_benchmark: None,
            benchmark_manifest: None,
            skill: None,
            budget: Some(6),
            workdir: None,
            isolate_workdir: false,
            outcome: None,
            manual_intervention: false,
            benchmark_gate: false,
            notes: None,
        };
        let record = DogfoodRecord::from_result(
            1,
            10,
            "deepseek-v4-pro".to_string(),
            ".".to_string(),
            6,
            &args,
            false,
            &result,
        );
        assert_eq!(record.failed_tool_calls, 1);
        assert!(matches!(record.outcome, DogfoodOutcome::Failed));
        assert!(!record.diagnostic_expected_failure);
    }

    #[test]
    fn from_result_treats_recovered_validation_retry_as_success() {
        let result = RunResult {
            final_message: "tests pass".to_string(),
            tool_events: vec![
                ToolEvent {
                    tool_name: "apply_patch".to_string(),
                    input: BTreeMap::new(),
                    output: "Updated src/math_ops.py using single replacement mode.".to_string(),
                    status: ObservationStatus::Ok,
                },
                ToolEvent {
                    tool_name: "git_diff".to_string(),
                    input: BTreeMap::new(),
                    output: "No local diff.".to_string(),
                    status: ObservationStatus::Ok,
                },
                ToolEvent {
                    tool_name: "run_shell".to_string(),
                    input: BTreeMap::new(),
                    output: "meta.command_kind=test\nmeta.exit_code=1\nmeta.result=failed\nmeta.failure_kind=test_failure".to_string(),
                    status: ObservationStatus::Ok,
                },
                ToolEvent {
                    tool_name: "read_file".to_string(),
                    input: BTreeMap::new(),
                    output: "2     return a * b".to_string(),
                    status: ObservationStatus::Ok,
                },
                ToolEvent {
                    tool_name: "apply_patch".to_string(),
                    input: BTreeMap::new(),
                    output: "Updated src/math_ops.py using single replacement mode.".to_string(),
                    status: ObservationStatus::Ok,
                },
                ToolEvent {
                    tool_name: "git_diff".to_string(),
                    input: BTreeMap::new(),
                    output: "No local diff.".to_string(),
                    status: ObservationStatus::Ok,
                },
                ToolEvent {
                    tool_name: "run_shell".to_string(),
                    input: BTreeMap::new(),
                    output: "meta.command_kind=test\nmeta.exit_code=0\nmeta.result=ok".to_string(),
                    status: ObservationStatus::Ok,
                },
            ],
            usage: TokenUsage::default(),
        };
        let args = DogfoodRunArgs {
            task: "replace `a - b` with `a * b` in src/math_ops.py and validate with pytest until the tests pass"
                .to_string(),
            from_benchmark: None,
            benchmark_manifest: None,
            skill: None,
            budget: Some(8),
            workdir: None,
            isolate_workdir: false,
            outcome: None,
            manual_intervention: false,
            benchmark_gate: false,
            notes: None,
        };
        let record = DogfoodRecord::from_result(
            1,
            10,
            "deepseek-v4-pro".to_string(),
            ".".to_string(),
            8,
            &args,
            false,
            &result,
        );
        assert_eq!(record.failed_tool_calls, 1);
        assert!(matches!(record.outcome, DogfoodOutcome::Success));
        assert_eq!(record.benchmark_category.as_deref(), Some("write_validate"));
    }

    #[test]
    fn from_result_treats_validated_patch_as_success_after_repeated_read_recovery() {
        let result = RunResult {
            final_message: "tests pass".to_string(),
            tool_events: vec![
                ToolEvent {
                    tool_name: "read_file".to_string(),
                    input: BTreeMap::new(),
                    output: "1 pub fn add(a: i32, b: i32) -> i32 {".to_string(),
                    status: ObservationStatus::Ok,
                },
                ToolEvent {
                    tool_name: "read_file".to_string(),
                    input: BTreeMap::new(),
                    output: "repeated identical tool call detected".to_string(),
                    status: ObservationStatus::Failed,
                },
                ToolEvent {
                    tool_name: "apply_patch".to_string(),
                    input: BTreeMap::new(),
                    output: "Updated src/lib.rs using single replacement mode.".to_string(),
                    status: ObservationStatus::Ok,
                },
                ToolEvent {
                    tool_name: "run_shell".to_string(),
                    input: BTreeMap::new(),
                    output: "meta.command_kind=test\nmeta.exit_code=0\nmeta.result=ok".to_string(),
                    status: ObservationStatus::Ok,
                },
            ],
            usage: TokenUsage::default(),
        };
        let args = DogfoodRunArgs {
            task: "replace `a - b` with `a + b` in src/lib.rs and validate with cargo test"
                .to_string(),
            from_benchmark: None,
            benchmark_manifest: None,
            skill: None,
            budget: Some(8),
            workdir: None,
            isolate_workdir: false,
            outcome: None,
            manual_intervention: false,
            benchmark_gate: false,
            notes: None,
        };
        let record = DogfoodRecord::from_result(
            1,
            10,
            "deepseek-chat".to_string(),
            ".".to_string(),
            8,
            &args,
            false,
            &result,
        );
        assert_eq!(record.failed_tool_calls, 1);
        assert_eq!(record.repeated_call_failures, 1);
        assert!(matches!(record.outcome, DogfoodOutcome::Success));
    }

    #[test]
    fn from_result_treats_expected_failure_diagnosis_as_success() {
        let result = RunResult {
            final_message: "read back the failing test file".to_string(),
            tool_events: vec![
                ToolEvent {
                    tool_name: "run_shell".to_string(),
                    input: BTreeMap::new(),
                    output: "meta.command_kind=test\nmeta.exit_code=101\nmeta.result=failed\nmeta.failure_kind=test_failure".to_string(),
                    status: ObservationStatus::Ok,
                },
                ToolEvent {
                    tool_name: "read_file".to_string(),
                    input: BTreeMap::new(),
                    output: "test('route benchmark stays stable', () => {})".to_string(),
                    status: ObservationStatus::Ok,
                },
            ],
            usage: TokenUsage::default(),
        };
        let args = DogfoodRunArgs {
            task: "investigate why npm test fails in the JavaScript CLI and inspect the failing test file before retrying"
                .to_string(),
            from_benchmark: None,
            benchmark_manifest: None,
            skill: Some("debug".to_string()),
            budget: Some(4),
            workdir: None,
            isolate_workdir: false,
            outcome: None,
            manual_intervention: false,
            benchmark_gate: false,
            notes: None,
        };
        let record = DogfoodRecord::from_result(
            1,
            10,
            "deepseek-v4-pro".to_string(),
            ".".to_string(),
            4,
            &args,
            false,
            &result,
        );
        assert_eq!(record.failed_tool_calls, 1);
        assert!(matches!(record.outcome, DogfoodOutcome::Success));
        assert!(record.diagnostic_expected_failure);
    }

    #[test]
    fn prepare_run_workdir_clones_fixture_when_isolation_enabled() {
        let fixture_root =
            std::env::temp_dir().join(format!("deepseek-dogfood-fixture-{}", std::process::id()));
        let fixture = fixture_root.join("fixture");
        let nested = fixture.join("src");
        fs::create_dir_all(&nested).unwrap();
        fs::write(
            nested.join("lib.rs"),
            "pub fn add(a: i32, b: i32) -> i32 { a - b }\n",
        )
        .unwrap();

        let (execution, cleanup) = prepare_run_workdir(&fixture, true).unwrap();
        assert_ne!(execution, fixture);
        assert!(execution.join("src/lib.rs").is_file());

        fs::remove_dir_all(fixture_root).ok();
        if let Some(path) = cleanup {
            fs::remove_dir_all(path).ok();
        }
    }

    #[test]
    fn resolve_run_args_loads_task_defaults_from_benchmark_case() {
        let root = std::env::temp_dir().join(format!(
            "deepseek-dogfood-benchmark-run-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let manifest_path = root.join("benchmarks.txt");
        fs::write(
            &manifest_path,
            r#"name = "fixture-pr-retry-validate-rust-mini"
task = "Address PR #48 review feedback: replace `a - b` with `a * b` in src/lib.rs and validate with cargo test until the tests pass."
category = "pr_workflow"
skill = "verify-changes"
workdir = "fixtures/rust-write-mini"
isolate_workdir = true
budget = 8
notes = "Real PR workflow retry case over an isolated Rust fixture"
"#,
        )
        .unwrap();

        let args = DogfoodRunArgs {
            task: String::new(),
            from_benchmark: Some("fixture-pr-retry-validate-rust-mini".to_string()),
            benchmark_manifest: Some(manifest_path.display().to_string()),
            skill: None,
            budget: None,
            workdir: None,
            isolate_workdir: false,
            outcome: None,
            manual_intervention: false,
            benchmark_gate: false,
            notes: None,
        };
        let resolved = resolve_run_args(&crate::config::types::AppConfig::default(), args).unwrap();

        assert!(resolved
            .task
            .contains("replace `a - b` with `a * b` in src/lib.rs"));
        assert_eq!(resolved.skill.as_deref(), Some("verify-changes"));
        assert_eq!(resolved.budget, Some(8));
        let expected_workdir = root.join("fixtures/rust-write-mini").display().to_string();
        assert_eq!(resolved.workdir.as_deref(), Some(expected_workdir.as_str()));
        assert!(resolved.isolate_workdir);
        assert_eq!(
            resolved.notes.as_deref(),
            Some("Real PR workflow retry case over an isolated Rust fixture")
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn benchmark_replay_auto_approve_env_is_temporary() {
        unsafe {
            env::remove_var("DSCODE_AUTO_APPROVE_WRITES");
            env::remove_var("DSCODE_AUTO_APPROVE_SHELL");
        }

        let root = temp_test_dir("dogfood-auto-approve");
        fs::create_dir_all(&root).unwrap();

        let snapshot = run_task_in_workdir(&root, &root, true, || {
            Ok((
                env::var("DSCODE_AUTO_APPROVE_WRITES").ok(),
                env::var("DSCODE_AUTO_APPROVE_SHELL").ok(),
            ))
        })
        .unwrap();

        assert_eq!(snapshot.0.as_deref(), Some("1"));
        assert_eq!(snapshot.1.as_deref(), Some("1"));
        assert!(env::var("DSCODE_AUTO_APPROVE_WRITES").is_err());
        assert!(env::var("DSCODE_AUTO_APPROVE_SHELL").is_err());

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn resolve_run_args_keeps_explicit_overrides_over_benchmark_defaults() {
        let root = std::env::temp_dir().join(format!(
            "deepseek-dogfood-benchmark-override-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let manifest_path = root.join("benchmarks.txt");
        fs::write(
            &manifest_path,
            r#"name = "fixture-inspect-rust-cli-mini"
task = "inspect the fixture"
category = "read_only"
skill = "research"
workdir = "fixtures/rust-cli-mini"
budget = 6
"#,
        )
        .unwrap();

        let args = DogfoodRunArgs {
            task: String::new(),
            from_benchmark: Some("fixture-inspect-rust-cli-mini".to_string()),
            benchmark_manifest: Some(manifest_path.display().to_string()),
            skill: Some("debug".to_string()),
            budget: Some(3),
            workdir: Some("custom-fixture".to_string()),
            isolate_workdir: true,
            outcome: None,
            manual_intervention: false,
            benchmark_gate: false,
            notes: Some("custom note".to_string()),
        };
        let resolved = resolve_run_args(&crate::config::types::AppConfig::default(), args).unwrap();

        assert_eq!(resolved.skill.as_deref(), Some("debug"));
        assert_eq!(resolved.budget, Some(3));
        assert_eq!(resolved.workdir.as_deref(), Some("custom-fixture"));
        assert!(resolved.isolate_workdir);
        assert_eq!(resolved.notes.as_deref(), Some("custom note"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn select_replayable_cases_skips_seed_only_cases() {
        let cases = vec![
            BenchmarkCaseSummary {
                name: "seeded-pr-review".to_string(),
                task: "review pull request".to_string(),
                category: "pr_workflow".to_string(),
                skill: None,
                workdir: Some("fixtures/rust-cli-mini".to_string()),
                isolate_workdir: false,
                budget: 4,
                notes: None,
                seed_observations: Some("git_diff:ok:src/lib.rs".to_string()),
            },
            BenchmarkCaseSummary {
                name: "fixture-pr-retry-validate-rust-mini".to_string(),
                task: "fix and validate".to_string(),
                category: "pr_workflow".to_string(),
                skill: None,
                workdir: Some("fixtures/rust-write-mini".to_string()),
                isolate_workdir: true,
                budget: 8,
                notes: None,
                seed_observations: None,
            },
        ];

        let selected = select_replayable_cases(&cases, Some("pr_workflow"), None);
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].name, "fixture-pr-retry-validate-rust-mini");
    }

    #[test]
    fn select_replayable_cases_applies_category_and_limit() {
        let cases = vec![
            BenchmarkCaseSummary {
                name: "fixture-a".to_string(),
                task: "a".to_string(),
                category: "write_validate".to_string(),
                skill: None,
                workdir: Some("fixtures/a".to_string()),
                isolate_workdir: true,
                budget: 6,
                notes: None,
                seed_observations: None,
            },
            BenchmarkCaseSummary {
                name: "fixture-b".to_string(),
                task: "b".to_string(),
                category: "write_validate".to_string(),
                skill: None,
                workdir: Some("fixtures/b".to_string()),
                isolate_workdir: true,
                budget: 6,
                notes: None,
                seed_observations: None,
            },
            BenchmarkCaseSummary {
                name: "fixture-c".to_string(),
                task: "c".to_string(),
                category: "recovery".to_string(),
                skill: None,
                workdir: Some("fixtures/c".to_string()),
                isolate_workdir: true,
                budget: 6,
                notes: None,
                seed_observations: None,
            },
        ];

        let selected = select_replayable_cases(&cases, Some("write_validate"), Some(1));
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].name, "fixture-a");
    }

    #[test]
    fn render_benchmark_seed_export_emits_non_success_records() {
        let records = vec![
            DogfoodRecord {
                version: 1,
                timestamp_secs: 10,
                duration_ms: 4,
                task: "investigate repeated list_files loop".to_string(),
                skill: None,
                budget: 6,
                model: "x".to_string(),
                workdir: "/repo".to_string(),
                outcome: DogfoodOutcome::Stuck,
                manual_intervention: false,
                notes: Some("dogfood seed".to_string()),
                tool_calls: 3,
                failed_tool_calls: 1,
                repeated_call_failures: 1,
                diagnostic_expected_failure: false,
                used_subagent: false,
                final_message: "stuck".to_string(),
                tool_trace: "list_files -> list_files".to_string(),
                error_kind: None,
                benchmark_category: Some("recovery".to_string()),
                benchmark_seed_observations: Some(
                    "list_files:failed:repeated identical tool call detected".to_string(),
                ),
            },
            DogfoodRecord {
                version: 1,
                timestamp_secs: 11,
                duration_ms: 3,
                task: "inspect repository".to_string(),
                skill: None,
                budget: 4,
                model: "x".to_string(),
                workdir: "/repo".to_string(),
                outcome: DogfoodOutcome::Success,
                manual_intervention: false,
                notes: None,
                tool_calls: 2,
                failed_tool_calls: 0,
                repeated_call_failures: 0,
                diagnostic_expected_failure: false,
                used_subagent: false,
                final_message: "ok".to_string(),
                tool_trace: "list_files -> read_file".to_string(),
                error_kind: None,
                benchmark_category: Some("read_only".to_string()),
                benchmark_seed_observations: Some("list_files:ok:src/".to_string()),
            },
        ];

        let export = render_benchmark_seed_export(&records, 10, None, Path::new("/repo"));
        assert!(export.contains("name = \"dogfood-stuck-"));
        assert!(export.contains("task = \"investigate repeated list_files loop\""));
        assert!(export.contains("category = \"recovery\""));
        assert!(export.contains(
            "seed_observations = \"list_files:failed:repeated identical tool call detected\""
        ));
        assert!(!export.contains("inspect repository"));
    }

    #[test]
    fn build_promotion_plan_skips_duplicate_cases_and_renames_conflicts() {
        let records = vec![
            DogfoodRecord {
                version: 1,
                timestamp_secs: 10,
                duration_ms: 4,
                task: "investigate repeated list_files loop".to_string(),
                skill: None,
                budget: 6,
                model: "x".to_string(),
                workdir: "/repo".to_string(),
                outcome: DogfoodOutcome::Stuck,
                manual_intervention: false,
                notes: Some("dogfood seed".to_string()),
                tool_calls: 3,
                failed_tool_calls: 1,
                repeated_call_failures: 1,
                diagnostic_expected_failure: false,
                used_subagent: false,
                final_message: "stuck".to_string(),
                tool_trace: "list_files -> list_files".to_string(),
                error_kind: None,
                benchmark_category: Some("recovery".to_string()),
                benchmark_seed_observations: Some(
                    "list_files:failed:repeated identical tool call detected".to_string(),
                ),
            },
            DogfoodRecord {
                version: 1,
                timestamp_secs: 11,
                duration_ms: 4,
                task: "investigate repeated list_files loop".to_string(),
                skill: None,
                budget: 6,
                model: "x".to_string(),
                workdir: "/repo".to_string(),
                outcome: DogfoodOutcome::Stuck,
                manual_intervention: false,
                notes: Some("second seed".to_string()),
                tool_calls: 3,
                failed_tool_calls: 1,
                repeated_call_failures: 1,
                diagnostic_expected_failure: false,
                used_subagent: false,
                final_message: "stuck again".to_string(),
                tool_trace: "list_files -> search_text".to_string(),
                error_kind: None,
                benchmark_category: Some("recovery".to_string()),
                benchmark_seed_observations: Some(
                    "search_text:failed:no matches || recovery_hint:ok:after=search_text; next=list_files".to_string(),
                ),
            },
        ];
        let existing = vec![BenchmarkCaseSummary {
            name: "dogfood-stuck-investigate-repeated-list-files".to_string(),
            task: "investigate repeated list_files loop".to_string(),
            category: "recovery".to_string(),
            skill: None,
            workdir: None,
            isolate_workdir: false,
            budget: 4,
            notes: None,
            seed_observations: Some(
                "list_files:failed:repeated identical tool call detected".to_string(),
            ),
        }];

        let plan = build_promotion_plan(&records, &existing, 10, None, Path::new("/repo"));
        assert_eq!(plan.duplicates_skipped, 1);
        assert_eq!(plan.policy_skipped, 0);
        assert!(plan.policy_skip_reasons.is_empty());
        assert_eq!(plan.cases.len(), 1);
        assert_eq!(
            plan.cases[0].name,
            "dogfood-stuck-investigate-repeated-list-files-2"
        );
        assert!(plan.cases[0].block.contains("category = \"recovery\""));
        assert!(plan.cases[0]
            .block
            .contains("seed_observations = \"search_text:failed:no matches"));
    }

    #[test]
    fn build_promotion_plan_skips_manual_and_long_trace_by_default_policy() {
        let records = vec![
            DogfoodRecord {
                version: 1,
                timestamp_secs: 10,
                duration_ms: 4,
                task: "manual triage".to_string(),
                skill: None,
                budget: 6,
                model: "x".to_string(),
                workdir: "/repo".to_string(),
                outcome: DogfoodOutcome::Manual,
                manual_intervention: true,
                notes: None,
                tool_calls: 4,
                failed_tool_calls: 1,
                repeated_call_failures: 0,
                diagnostic_expected_failure: false,
                used_subagent: false,
                final_message: "manual".to_string(),
                tool_trace: "search_text -> read_file -> list_files -> todo_write".to_string(),
                error_kind: None,
                benchmark_category: Some("planning".to_string()),
                benchmark_seed_observations: Some("search_text:failed:no matches".to_string()),
            },
            DogfoodRecord {
                version: 1,
                timestamp_secs: 11,
                duration_ms: 4,
                task: "too many hops".to_string(),
                skill: None,
                budget: 12,
                model: "x".to_string(),
                workdir: "/repo".to_string(),
                outcome: DogfoodOutcome::Failed,
                manual_intervention: false,
                notes: None,
                tool_calls: 9,
                failed_tool_calls: 1,
                repeated_call_failures: 0,
                diagnostic_expected_failure: false,
                used_subagent: false,
                final_message: "failed".to_string(),
                tool_trace: "a -> b -> c -> d -> e -> f -> g -> h -> i".to_string(),
                error_kind: None,
                benchmark_category: Some("read_only".to_string()),
                benchmark_seed_observations: Some("run_shell:failed:boom".to_string()),
            },
        ];

        let plan = build_promotion_plan(&records, &[], 10, None, Path::new("/repo"));
        assert_eq!(plan.cases.len(), 0);
        assert_eq!(plan.policy_skipped, 2);
        assert_eq!(plan.policy_skip_reasons.len(), 2);
        assert!(plan.policy_skip_reasons.iter().any(|reason| {
            reason.reason_code == "manual_requires_explicit_filter"
                && reason.count == 1
                && reason.example_task == "manual triage"
        }));
        assert!(plan.policy_skip_reasons.iter().any(|reason| {
            reason.reason_code == "tool_trace_too_long"
                && reason.count == 1
                && reason.example_task == "too many hops"
        }));
    }

    #[test]
    fn build_promotion_plan_allows_manual_when_explicitly_filtered() {
        let records = vec![DogfoodRecord {
            version: 1,
            timestamp_secs: 10,
            duration_ms: 4,
            task: "manual triage".to_string(),
            skill: None,
            budget: 6,
            model: "x".to_string(),
            workdir: "/repo".to_string(),
            outcome: DogfoodOutcome::Manual,
            manual_intervention: true,
            notes: None,
            tool_calls: 4,
            failed_tool_calls: 1,
            repeated_call_failures: 0,
            diagnostic_expected_failure: false,
            used_subagent: false,
            final_message: "manual".to_string(),
            tool_trace: "search_text -> read_file -> list_files -> todo_write".to_string(),
            error_kind: None,
            benchmark_category: Some("planning".to_string()),
            benchmark_seed_observations: Some("search_text:failed:no matches".to_string()),
        }];

        let plan = build_promotion_plan(
            &records,
            &[],
            10,
            Some(DogfoodOutcome::Manual),
            Path::new("/repo"),
        );
        assert_eq!(plan.cases.len(), 1);
        assert_eq!(plan.policy_skipped, 0);
        assert!(plan.policy_skip_reasons.is_empty());
    }

    #[test]
    fn build_promotion_plan_reports_missing_failure_signal_reason() {
        let records = vec![DogfoodRecord {
            version: 1,
            timestamp_secs: 10,
            duration_ms: 4,
            task: "ambiguous success-looking run".to_string(),
            skill: None,
            budget: 6,
            model: "x".to_string(),
            workdir: "/repo".to_string(),
            outcome: DogfoodOutcome::Failed,
            manual_intervention: false,
            notes: None,
            tool_calls: 3,
            failed_tool_calls: 0,
            repeated_call_failures: 0,
            diagnostic_expected_failure: false,
            used_subagent: false,
            final_message: "gave up without a hard failure".to_string(),
            tool_trace: "search_text -> read_file -> todo_write".to_string(),
            error_kind: None,
            benchmark_category: Some("planning".to_string()),
            benchmark_seed_observations: Some("search_text:ok:hit".to_string()),
        }];

        let plan = build_promotion_plan(&records, &[], 10, None, Path::new("/repo"));
        assert_eq!(plan.cases.len(), 0);
        assert_eq!(plan.policy_skipped, 1);
        assert_eq!(
            plan.policy_skip_reasons,
            vec![PolicySkipReasonCount {
                reason_code: "missing_failure_signal",
                reason_label: "missing failed/stuck/manual signal",
                count: 1,
                example_task: "ambiguous success-looking run".to_string(),
            }]
        );
    }

    #[test]
    fn append_promoted_cases_separates_blocks() {
        let root = std::env::temp_dir().join(format!("deepseek-promote-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let manifest_path = root.join("benchmarks.txt");
        fs::write(
            &manifest_path,
            "name = \"existing\"\ntask = \"inspect repo\"\n",
        )
        .unwrap();
        let cases = vec![
            PromotedBenchmarkCase {
                name: "case-one".to_string(),
                block: "name = \"case-one\"\ntask = \"one\"\n".to_string(),
            },
            PromotedBenchmarkCase {
                name: "case-two".to_string(),
                block: "name = \"case-two\"\ntask = \"two\"\n".to_string(),
            },
        ];

        append_promoted_cases(&manifest_path, &cases).unwrap();
        let written = fs::read_to_string(&manifest_path).unwrap();
        assert!(
            written.contains("name = \"existing\"\ntask = \"inspect repo\"\n\nname = \"case-one\"")
        );
        assert!(written.contains("name = \"case-one\"\ntask = \"one\"\n\nname = \"case-two\""));

        let _ = fs::remove_dir_all(root);
    }
}
