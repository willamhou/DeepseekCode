use std::collections::BTreeMap;
use std::env;
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use crate::cli::app::BenchmarkArgs;
use crate::cli::commands::dogfood::resolved_benchmark_category;
use crate::config::load::load_or_default;
use crate::config::types::AppConfig;
use crate::core::context::TaskContext;
use crate::core::loop_runtime::{AgentLoop, AgentLoopOptions, RunResult};
use crate::error::{app_error, AppResult};
use crate::model::protocol::Observation;
use crate::util::json::{
    json_as_array, json_as_object, json_as_string, json_as_u64, json_value_to_string,
    parse_root_object, JsonValue,
};

const DEFAULT_MANIFEST: &str = ".dscode/benchmarks.txt";
const DEFAULT_REPORT: &str = ".dscode/benchmarks/latest.md";
const TREND_GATE_WINDOW: usize = 5;
const TREND_GATE_MIN_HISTORY: usize = 3;

pub fn run(args: BenchmarkArgs) -> AppResult<()> {
    let config = load_or_default()?;
    run_with_config(config, args)
}

pub fn run_with_config(config: AppConfig, args: BenchmarkArgs) -> AppResult<()> {
    let manifest_path = args
        .manifest
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(DEFAULT_MANIFEST));
    let out_path = args
        .out
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(DEFAULT_REPORT));

    let manifest_text = fs::read_to_string(&manifest_path).map_err(|error| {
        app_error(format!(
            "failed to read benchmark manifest {}: {error}",
            manifest_path.display()
        ))
    })?;
    let cases = parse_manifest(&manifest_text)?;
    if cases.is_empty() {
        return Err(app_error(format!(
            "benchmark manifest {} did not contain any cases",
            manifest_path.display()
        )));
    }

    println!("DeepseekCode benchmark");
    println!("manifest: {}", manifest_path.display());
    println!("cases: {}", cases.len());

    let agent = AgentLoop::new(config.clone());
    let benchmark_started = Instant::now();
    let manifest_dir = manifest_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf();
    let mut results = Vec::with_capacity(cases.len());
    for (index, case) in cases.iter().enumerate() {
        println!("[{}/{}] {}", index + 1, cases.len(), case.name);
        let started = Instant::now();
        let resolved_workdir = resolve_case_workdir(&manifest_dir, case.workdir.as_deref())?;
        let (execution_workdir, cleanup_workdir) =
            prepare_case_workdir(resolved_workdir.as_deref(), case.isolate_workdir)?;
        let result =
            run_case_in_workdir(execution_workdir.as_deref(), case.isolate_workdir, || {
                agent.run_with(
                    TaskContext::new(case.task.clone(), case.skill.clone()),
                    AgentLoopOptions {
                        steps: case.budget,
                        initial_observations: case.seed_observations.clone(),
                        emit_progress: false,
                        persist_session: false,
                        ..AgentLoopOptions::default()
                    },
                )
            });
        if let Some(path) = cleanup_workdir.as_deref() {
            let _ = fs::remove_dir_all(path);
        }
        let result = result?;
        let evaluation = case.evaluate(&result);
        results.push(BenchmarkCaseResult {
            case: case.clone(),
            workdir: resolved_workdir,
            duration_ms: started.elapsed().as_millis(),
            passed: evaluation.passed,
            failed_tool_calls: result
                .tool_events
                .iter()
                .filter(|event| {
                    matches!(
                        event.status,
                        crate::model::protocol::ObservationStatus::Failed
                    )
                })
                .count(),
            tool_calls: result.tool_events.len(),
            tool_trace: if result.tool_events.is_empty() {
                "none".to_string()
            } else {
                result
                    .tool_events
                    .iter()
                    .map(|event| event.tool_name.as_str())
                    .collect::<Vec<_>>()
                    .join(" -> ")
            },
            final_message: first_non_empty_line(&result.final_message)
                .unwrap_or("")
                .to_string(),
            failure_summary: if evaluation.failures.is_empty() {
                "ok".to_string()
            } else {
                evaluation.failures.join("; ")
            },
        });
    }

    if let Some(parent) = out_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let history_path = config.workspace.benchmark_history_path();
    let previous_history = load_history(&history_path)?;
    let dogfood_snapshot = load_dogfood_snapshot(&config.workspace.dogfood_ledger_path())?;
    let current_record = BenchmarkRunRecord::from_results(
        unix_now_secs()?,
        manifest_path.display().to_string(),
        benchmark_started.elapsed().as_millis() as u64,
        &results,
        dogfood_snapshot.as_ref(),
    );
    let trend_gate = evaluate_trend_gate(&current_record, &previous_history);
    let live_gate = evaluate_live_gate(&current_record, &previous_history);
    let mut combined_history = previous_history.clone();
    combined_history.push(current_record.clone());
    let report = render_report(
        &manifest_path,
        &results,
        &combined_history,
        dogfood_snapshot.as_ref(),
        &trend_gate,
        &live_gate,
    );
    fs::write(&out_path, report)?;
    append_history_record(&history_path, &current_record)?;
    println!("report: {}", out_path.display());
    println!("history: {}", history_path.display());
    println!("trend gate: {}", trend_gate.summary_line());
    println!("live gate: {}", live_gate.summary_line());
    let passed = results.iter().filter(|result| result.passed).count();
    if passed < results.len() {
        return Err(app_error(format!(
            "benchmark expectations failed: {passed}/{} passed",
            results.len()
        )));
    }
    if trend_gate.failed() {
        return Err(app_error(format!(
            "benchmark trend gate failed: {}",
            trend_gate.summary_line()
        )));
    }
    if live_gate.failed() {
        return Err(app_error(format!(
            "benchmark live gate failed: {}",
            live_gate.summary_line()
        )));
    }
    Ok(())
}

#[derive(Debug, Clone)]
struct BenchmarkCase {
    name: String,
    task: String,
    category: String,
    skill: Option<String>,
    workdir: Option<String>,
    isolate_workdir: bool,
    budget: usize,
    seed_observations: Vec<crate::model::protocol::Observation>,
    expect_tool: Option<String>,
    expect_tool_sequence: Option<Vec<String>>,
    forbid_tool: Option<String>,
    expect_message_contains: Option<String>,
    expect_tool_output_contains: Option<ToolOutputExpectation>,
    expect_last_tool_output_contains: Option<String>,
    min_tool_calls: Option<usize>,
    max_tool_calls: Option<usize>,
    max_failed_tools: Option<usize>,
    notes: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ToolOutputExpectation {
    tool_name: String,
    needle: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BenchmarkCaseSummary {
    pub name: String,
    pub task: String,
    pub category: String,
    pub skill: Option<String>,
    pub workdir: Option<String>,
    pub isolate_workdir: bool,
    pub budget: usize,
    pub notes: Option<String>,
    pub seed_observations: Option<String>,
}

impl BenchmarkCase {
    fn evaluate(&self, result: &RunResult) -> BenchmarkEvaluation {
        let mut failures = Vec::new();
        if let Some(tool) = self.expect_tool.as_deref() {
            if !result
                .tool_events
                .iter()
                .any(|event| event.tool_name == tool)
            {
                failures.push(format!("expected tool `{tool}` was never called"));
            }
        }
        if let Some(sequence) = self.expect_tool_sequence.as_ref() {
            let actual = result
                .tool_events
                .iter()
                .map(|event| event.tool_name.as_str())
                .collect::<Vec<_>>();
            if !contains_tool_sequence(&actual, sequence) {
                failures.push(format!(
                    "tool sequence `{}` was not observed in trace `{}`",
                    sequence.join(" -> "),
                    actual.join(" -> ")
                ));
            }
        }
        if let Some(tool) = self.forbid_tool.as_deref() {
            if result
                .tool_events
                .iter()
                .any(|event| event.tool_name == tool)
            {
                failures.push(format!("forbidden tool `{tool}` was called"));
            }
        }
        if let Some(needle) = self.expect_message_contains.as_deref() {
            if !result.final_message.contains(needle) {
                failures.push(format!("final message did not contain `{needle}`"));
            }
        }
        if let Some(expectation) = self.expect_tool_output_contains.as_ref() {
            if let Some(event) = result
                .tool_events
                .iter()
                .rev()
                .find(|event| event.tool_name == expectation.tool_name)
            {
                if !event.output.contains(&expectation.needle) {
                    failures.push(format!(
                        "tool `{}` output did not contain `{}`",
                        expectation.tool_name, expectation.needle
                    ));
                }
            } else {
                failures.push(format!(
                    "tool `{}` output did not contain `{}` because the tool was never called",
                    expectation.tool_name, expectation.needle
                ));
            }
        }
        if let Some(needle) = self.expect_last_tool_output_contains.as_deref() {
            let Some(last) = result.tool_events.last() else {
                failures.push(format!(
                    "last tool output did not contain `{needle}` because no tool was called"
                ));
                return BenchmarkEvaluation {
                    passed: false,
                    failures,
                };
            };
            if !last.output.contains(needle) {
                let output_preview = clip(
                    first_non_empty_line(&last.output).unwrap_or(&last.output),
                    120,
                );
                failures.push(format!(
                    "last tool `{}` output did not contain `{needle}`; output starts `{output_preview}`",
                    last.tool_name,
                ));
            }
        }
        if let Some(min_tool_calls) = self.min_tool_calls {
            if result.tool_events.len() < min_tool_calls {
                failures.push(format!(
                    "tool calls {} below expected minimum {min_tool_calls}",
                    result.tool_events.len()
                ));
            }
        }
        if let Some(max_tool_calls) = self.max_tool_calls {
            if result.tool_events.len() > max_tool_calls {
                failures.push(format!(
                    "tool calls {} exceeded max {max_tool_calls}",
                    result.tool_events.len()
                ));
            }
        }
        let failed_tool_calls = result
            .tool_events
            .iter()
            .filter(|event| {
                matches!(
                    event.status,
                    crate::model::protocol::ObservationStatus::Failed
                )
            })
            .count();
        if let Some(max_failed_tools) = self.max_failed_tools {
            if failed_tool_calls > max_failed_tools {
                failures.push(format!(
                    "failed tool calls {failed_tool_calls} exceeded max {max_failed_tools}"
                ));
            }
        }
        BenchmarkEvaluation {
            passed: failures.is_empty(),
            failures,
        }
    }
}

impl From<&BenchmarkCase> for BenchmarkCaseSummary {
    fn from(case: &BenchmarkCase) -> Self {
        Self {
            name: case.name.clone(),
            task: case.task.clone(),
            category: case.category.clone(),
            skill: case.skill.clone(),
            workdir: case.workdir.clone(),
            isolate_workdir: case.isolate_workdir,
            budget: case.budget,
            notes: case.notes.clone(),
            seed_observations: format_seed_observations(&case.seed_observations),
        }
    }
}

#[derive(Debug)]
struct BenchmarkEvaluation {
    passed: bool,
    failures: Vec<String>,
}

#[derive(Debug)]
struct BenchmarkCaseResult {
    case: BenchmarkCase,
    workdir: Option<PathBuf>,
    duration_ms: u128,
    passed: bool,
    failed_tool_calls: usize,
    tool_calls: usize,
    tool_trace: String,
    final_message: String,
    failure_summary: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct BenchmarkCategoryStats {
    category: String,
    cases: u64,
    passed: u64,
    total_tool_calls: u64,
    total_failed_tool_calls: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DogfoodSnapshot {
    runs: u64,
    success: u64,
    failed: u64,
    stuck: u64,
    manual: u64,
    category_stats: Vec<DogfoodCategorySnapshot>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DogfoodCategorySnapshot {
    category: String,
    runs: u64,
    success: u64,
    failed: u64,
    stuck: u64,
    manual: u64,
    total_tool_calls: u64,
}

#[derive(Debug, Clone)]
struct BenchmarkRunRecord {
    version: u64,
    timestamp_secs: u64,
    manifest: String,
    cases: u64,
    passed: u64,
    total_tool_calls: u64,
    total_failed_tool_calls: u64,
    duration_ms: u64,
    category_stats: Vec<BenchmarkCategoryStats>,
    dogfood_snapshot: Option<DogfoodSnapshot>,
}

impl BenchmarkRunRecord {
    fn from_results(
        timestamp_secs: u64,
        manifest: String,
        duration_ms: u64,
        results: &[BenchmarkCaseResult],
        dogfood_snapshot: Option<&DogfoodSnapshot>,
    ) -> Self {
        Self {
            version: 3,
            timestamp_secs,
            manifest,
            cases: results.len() as u64,
            passed: results.iter().filter(|result| result.passed).count() as u64,
            total_tool_calls: results.iter().map(|result| result.tool_calls as u64).sum(),
            total_failed_tool_calls: results
                .iter()
                .map(|result| result.failed_tool_calls as u64)
                .sum(),
            duration_ms,
            category_stats: collect_category_stats(results),
            dogfood_snapshot: dogfood_snapshot.cloned(),
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
            "manifest".to_string(),
            JsonValue::String(self.manifest.clone()),
        );
        root.insert(
            "cases".to_string(),
            JsonValue::Number(self.cases.to_string()),
        );
        root.insert(
            "passed".to_string(),
            JsonValue::Number(self.passed.to_string()),
        );
        root.insert(
            "total_tool_calls".to_string(),
            JsonValue::Number(self.total_tool_calls.to_string()),
        );
        root.insert(
            "total_failed_tool_calls".to_string(),
            JsonValue::Number(self.total_failed_tool_calls.to_string()),
        );
        root.insert(
            "duration_ms".to_string(),
            JsonValue::Number(self.duration_ms.to_string()),
        );
        root.insert(
            "category_stats".to_string(),
            JsonValue::Array(
                self.category_stats
                    .iter()
                    .map(|stats| {
                        let mut item = BTreeMap::new();
                        item.insert(
                            "category".to_string(),
                            JsonValue::String(stats.category.clone()),
                        );
                        item.insert(
                            "cases".to_string(),
                            JsonValue::Number(stats.cases.to_string()),
                        );
                        item.insert(
                            "passed".to_string(),
                            JsonValue::Number(stats.passed.to_string()),
                        );
                        item.insert(
                            "total_tool_calls".to_string(),
                            JsonValue::Number(stats.total_tool_calls.to_string()),
                        );
                        item.insert(
                            "total_failed_tool_calls".to_string(),
                            JsonValue::Number(stats.total_failed_tool_calls.to_string()),
                        );
                        JsonValue::Object(item)
                    })
                    .collect(),
            ),
        );
        root.insert(
            "dogfood_runs".to_string(),
            self.dogfood_snapshot
                .as_ref()
                .map(|snapshot| JsonValue::Number(snapshot.runs.to_string()))
                .unwrap_or(JsonValue::Null),
        );
        root.insert(
            "dogfood_success".to_string(),
            self.dogfood_snapshot
                .as_ref()
                .map(|snapshot| JsonValue::Number(snapshot.success.to_string()))
                .unwrap_or(JsonValue::Null),
        );
        root.insert(
            "dogfood_failed".to_string(),
            self.dogfood_snapshot
                .as_ref()
                .map(|snapshot| JsonValue::Number(snapshot.failed.to_string()))
                .unwrap_or(JsonValue::Null),
        );
        root.insert(
            "dogfood_stuck".to_string(),
            self.dogfood_snapshot
                .as_ref()
                .map(|snapshot| JsonValue::Number(snapshot.stuck.to_string()))
                .unwrap_or(JsonValue::Null),
        );
        root.insert(
            "dogfood_manual".to_string(),
            self.dogfood_snapshot
                .as_ref()
                .map(|snapshot| JsonValue::Number(snapshot.manual.to_string()))
                .unwrap_or(JsonValue::Null),
        );
        root.insert(
            "dogfood_category_stats".to_string(),
            self.dogfood_snapshot
                .as_ref()
                .map(|snapshot| {
                    JsonValue::Array(
                        snapshot
                            .category_stats
                            .iter()
                            .map(|stats| {
                                let mut item = BTreeMap::new();
                                item.insert(
                                    "category".to_string(),
                                    JsonValue::String(stats.category.clone()),
                                );
                                item.insert(
                                    "runs".to_string(),
                                    JsonValue::Number(stats.runs.to_string()),
                                );
                                item.insert(
                                    "success".to_string(),
                                    JsonValue::Number(stats.success.to_string()),
                                );
                                item.insert(
                                    "failed".to_string(),
                                    JsonValue::Number(stats.failed.to_string()),
                                );
                                item.insert(
                                    "stuck".to_string(),
                                    JsonValue::Number(stats.stuck.to_string()),
                                );
                                item.insert(
                                    "manual".to_string(),
                                    JsonValue::Number(stats.manual.to_string()),
                                );
                                item.insert(
                                    "total_tool_calls".to_string(),
                                    JsonValue::Number(stats.total_tool_calls.to_string()),
                                );
                                JsonValue::Object(item)
                            })
                            .collect(),
                    )
                })
                .unwrap_or(JsonValue::Null),
        );
        json_value_to_string(&JsonValue::Object(root))
    }

    fn from_json_line(line: &str) -> AppResult<Self> {
        let root = parse_root_object(line)?;
        let dogfood_runs = read_optional_u64(&root, "dogfood_runs");
        let dogfood_success = read_optional_u64(&root, "dogfood_success");
        let dogfood_failed = read_optional_u64(&root, "dogfood_failed");
        let dogfood_stuck = read_optional_u64(&root, "dogfood_stuck");
        let dogfood_manual = read_optional_u64(&root, "dogfood_manual");

        Ok(Self {
            version: read_u64(&root, "version")?,
            timestamp_secs: read_u64(&root, "timestamp_secs")?,
            manifest: read_string(&root, "manifest")?.to_string(),
            cases: read_u64(&root, "cases")?,
            passed: read_u64(&root, "passed")?,
            total_tool_calls: read_u64(&root, "total_tool_calls")?,
            total_failed_tool_calls: read_u64(&root, "total_failed_tool_calls")?,
            duration_ms: read_u64(&root, "duration_ms")?,
            category_stats: read_category_stats(&root)?,
            dogfood_snapshot: match (
                dogfood_runs,
                dogfood_success,
                dogfood_failed,
                dogfood_stuck,
                dogfood_manual,
            ) {
                (Some(runs), Some(success), Some(failed), Some(stuck), Some(manual)) => {
                    Some(DogfoodSnapshot {
                        runs,
                        success,
                        failed,
                        stuck,
                        manual,
                        category_stats: read_dogfood_category_stats(&root)?,
                    })
                }
                _ => None,
            },
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TrendGateStatus {
    Passed,
    Failed,
    InsufficientHistory,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TrendGateEvaluation {
    status: TrendGateStatus,
    comparable_runs: usize,
    best_passed: Option<u64>,
    median_tool_calls: Option<u64>,
    tool_call_limit: Option<u64>,
    median_failed_tool_calls: Option<u64>,
    category_summaries: Vec<CategoryTrendSummary>,
    reasons: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CategoryTrendSummary {
    category: String,
    status: TrendGateStatus,
    comparable_runs: usize,
    best_passed: Option<u64>,
    median_tool_calls: Option<u64>,
    tool_call_limit: Option<u64>,
    median_failed_tool_calls: Option<u64>,
    reasons: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LiveGateEvaluation {
    status: TrendGateStatus,
    current_runs: Option<u64>,
    previous_runs: Option<u64>,
    reasons: Vec<String>,
}

impl TrendGateEvaluation {
    fn failed(&self) -> bool {
        matches!(self.status, TrendGateStatus::Failed)
    }

    fn summary_line(&self) -> String {
        match self.status {
            TrendGateStatus::Passed => format!(
                "pass against {} comparable run{} (best passed {}, median tool calls {}, limit {}, median failed tools {})",
                self.comparable_runs,
                if self.comparable_runs == 1 { "" } else { "s" },
                self.best_passed.unwrap_or(0),
                self.median_tool_calls.unwrap_or(0),
                self.tool_call_limit.unwrap_or(0),
                self.median_failed_tool_calls.unwrap_or(0),
            ),
            TrendGateStatus::Failed => format!(
                "FAILED against {} comparable run{}: {}",
                self.comparable_runs,
                if self.comparable_runs == 1 { "" } else { "s" },
                self.reasons.join("; ")
            ),
            TrendGateStatus::InsufficientHistory => format!(
                "skipped (need at least {} prior comparable runs, found {})",
                TREND_GATE_MIN_HISTORY, self.comparable_runs
            ),
        }
    }
}

impl LiveGateEvaluation {
    fn failed(&self) -> bool {
        matches!(self.status, TrendGateStatus::Failed)
    }

    fn summary_line(&self) -> String {
        match self.status {
            TrendGateStatus::Passed => {
                let current = self.current_runs.unwrap_or(0);
                let previous = self.previous_runs.unwrap_or(0);
                if current == previous {
                    format!("pass (no new dogfood records since previous snapshot, runs={current})")
                } else {
                    format!("pass against previous dogfood snapshot (runs {previous} -> {current})")
                }
            }
            TrendGateStatus::Failed => format!(
                "FAILED against previous dogfood snapshot (runs {} -> {}): {}",
                self.previous_runs.unwrap_or(0),
                self.current_runs.unwrap_or(0),
                self.reasons.join("; ")
            ),
            TrendGateStatus::InsufficientHistory => {
                "skipped (need current and previous dogfood snapshots)".to_string()
            }
        }
    }
}

fn parse_manifest(content: &str) -> AppResult<Vec<BenchmarkCase>> {
    let mut cases = Vec::new();
    let mut current = PendingCase::default();

    for raw_line in content.lines() {
        let line = raw_line.trim();
        if line.is_empty() {
            if current.has_content() {
                cases.push(current.finish()?);
                current = PendingCase::default();
            }
            continue;
        }
        if line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            return Err(app_error(format!(
                "invalid benchmark manifest line: {line}"
            )));
        };
        current.set(key.trim(), unquote(value.trim()))?;
    }

    if current.has_content() {
        cases.push(current.finish()?);
    }

    Ok(cases)
}

pub(crate) fn load_manifest_case_summaries(path: &Path) -> AppResult<Vec<BenchmarkCaseSummary>> {
    let manifest_text = fs::read_to_string(path).map_err(|error| {
        app_error(format!(
            "failed to read benchmark manifest {}: {error}",
            path.display()
        ))
    })?;
    let cases = parse_manifest(&manifest_text)?;
    Ok(cases.iter().map(BenchmarkCaseSummary::from).collect())
}

#[derive(Default)]
struct PendingCase {
    name: Option<String>,
    task: Option<String>,
    category: Option<String>,
    assertion_bundle: Option<String>,
    skill: Option<String>,
    workdir: Option<String>,
    isolate_workdir: bool,
    budget: Option<usize>,
    seed_observations: Vec<crate::model::protocol::Observation>,
    expect_tool: Option<String>,
    expect_tool_sequence: Option<Vec<String>>,
    forbid_tool: Option<String>,
    expect_message_contains: Option<String>,
    expect_tool_output_contains: Option<ToolOutputExpectation>,
    expect_last_tool_output_contains: Option<String>,
    min_tool_calls: Option<usize>,
    max_tool_calls: Option<usize>,
    max_failed_tools: Option<usize>,
    notes: Option<String>,
}

impl PendingCase {
    fn has_content(&self) -> bool {
        self.name.is_some()
            || self.task.is_some()
            || self.category.is_some()
            || self.assertion_bundle.is_some()
            || self.skill.is_some()
            || self.workdir.is_some()
            || self.isolate_workdir
            || self.budget.is_some()
            || !self.seed_observations.is_empty()
            || self.expect_tool.is_some()
            || self.expect_message_contains.is_some()
            || self.expect_tool_output_contains.is_some()
            || self.expect_last_tool_output_contains.is_some()
            || self.forbid_tool.is_some()
            || self.min_tool_calls.is_some()
            || self.max_tool_calls.is_some()
            || self.max_failed_tools.is_some()
            || self.notes.is_some()
    }

    fn set(&mut self, key: &str, value: String) -> AppResult<()> {
        match key {
            "name" => self.name = Some(value),
            "task" => self.task = Some(value),
            "category" => self.category = Some(value),
            "assertion_bundle" => self.assertion_bundle = Some(value),
            "skill" => self.skill = Some(value),
            "workdir" => self.workdir = Some(value),
            "isolate_workdir" => self.isolate_workdir = parse_manifest_bool(key, &value)?,
            "notes" => self.notes = Some(value),
            "seed_observations" => {
                self.seed_observations = parse_seed_observations(&value)?;
            }
            "budget" => {
                let parsed = value
                    .parse::<usize>()
                    .map_err(|_| app_error(format!("invalid benchmark budget: {value}")))?;
                if !(1..=200).contains(&parsed) {
                    return Err(app_error(format!(
                        "benchmark budget must be between 1 and 200, got {parsed}"
                    )));
                }
                self.budget = Some(parsed);
            }
            "expect_tool" => self.expect_tool = Some(value),
            "expect_tool_sequence" => {
                self.expect_tool_sequence = Some(parse_manifest_string_list(key, &value)?);
            }
            "forbid_tool" => self.forbid_tool = Some(value),
            "expect_message_contains" => self.expect_message_contains = Some(value),
            "expect_tool_output_contains" => {
                self.expect_tool_output_contains = Some(parse_tool_output_expectation(&value)?)
            }
            "expect_last_tool_output_contains" => {
                self.expect_last_tool_output_contains = Some(value)
            }
            "min_tool_calls" => self.min_tool_calls = Some(parse_manifest_usize(key, &value)?),
            "max_tool_calls" => self.max_tool_calls = Some(parse_manifest_usize(key, &value)?),
            "max_failed_tools" => self.max_failed_tools = Some(parse_manifest_usize(key, &value)?),
            _ => return Err(app_error(format!("unknown benchmark manifest key: {key}"))),
        }
        Ok(())
    }

    fn finish(self) -> AppResult<BenchmarkCase> {
        let assertion_defaults = self
            .assertion_bundle
            .as_deref()
            .map(assertion_bundle_defaults)
            .transpose()?
            .unwrap_or_default();
        let category = self
            .category
            .clone()
            .unwrap_or_else(|| infer_case_category(&self));
        Ok(BenchmarkCase {
            name: self
                .name
                .ok_or_else(|| app_error("benchmark case missing `name`"))?,
            task: self
                .task
                .ok_or_else(|| app_error("benchmark case missing `task`"))?,
            category,
            skill: self.skill,
            workdir: self.workdir,
            isolate_workdir: self.isolate_workdir,
            budget: self.budget.unwrap_or(8),
            seed_observations: self.seed_observations,
            expect_tool: self.expect_tool.or(assertion_defaults.expect_tool),
            expect_tool_sequence: self
                .expect_tool_sequence
                .or(assertion_defaults.expect_tool_sequence),
            forbid_tool: self.forbid_tool.or(assertion_defaults.forbid_tool),
            expect_message_contains: self
                .expect_message_contains
                .or(assertion_defaults.expect_message_contains),
            expect_tool_output_contains: self
                .expect_tool_output_contains
                .or(assertion_defaults.expect_tool_output_contains),
            expect_last_tool_output_contains: self
                .expect_last_tool_output_contains
                .or(assertion_defaults.expect_last_tool_output_contains),
            min_tool_calls: self.min_tool_calls.or(assertion_defaults.min_tool_calls),
            max_tool_calls: self.max_tool_calls.or(assertion_defaults.max_tool_calls),
            max_failed_tools: self
                .max_failed_tools
                .or(assertion_defaults.max_failed_tools),
            notes: self.notes,
        })
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct BenchmarkAssertionDefaults {
    expect_tool: Option<String>,
    expect_tool_sequence: Option<Vec<String>>,
    forbid_tool: Option<String>,
    expect_message_contains: Option<String>,
    expect_tool_output_contains: Option<ToolOutputExpectation>,
    expect_last_tool_output_contains: Option<String>,
    min_tool_calls: Option<usize>,
    max_tool_calls: Option<usize>,
    max_failed_tools: Option<usize>,
}

fn assertion_bundle_defaults(bundle: &str) -> AppResult<BenchmarkAssertionDefaults> {
    let defaults = match bundle {
        "read_only_inspect" => BenchmarkAssertionDefaults {
            expect_tool: Some("list_files".to_string()),
            expect_tool_sequence: Some(vec!["list_files".to_string(), "read_file".to_string()]),
            max_tool_calls: Some(4),
            max_failed_tools: Some(0),
            ..BenchmarkAssertionDefaults::default()
        },
        "read_only_search" => BenchmarkAssertionDefaults {
            expect_tool: Some("search_text".to_string()),
            expect_tool_sequence: Some(vec!["search_text".to_string(), "read_file".to_string()]),
            max_tool_calls: Some(4),
            max_failed_tools: Some(0),
            ..BenchmarkAssertionDefaults::default()
        },
        "recovery_search_fallback" => BenchmarkAssertionDefaults {
            expect_tool: Some("search_text".to_string()),
            expect_tool_sequence: Some(vec!["search_text".to_string(), "list_files".to_string()]),
            max_tool_calls: Some(4),
            max_failed_tools: Some(0),
            ..BenchmarkAssertionDefaults::default()
        },
        "recovery_readback_then_search" => BenchmarkAssertionDefaults {
            expect_tool: Some("read_file".to_string()),
            expect_tool_sequence: Some(vec!["read_file".to_string()]),
            max_tool_calls: Some(2),
            max_failed_tools: Some(0),
            ..BenchmarkAssertionDefaults::default()
        },
        "recovery_diff_then_readback" => BenchmarkAssertionDefaults {
            expect_tool: Some("git_diff".to_string()),
            expect_tool_sequence: Some(vec!["git_diff".to_string(), "read_file".to_string()]),
            max_tool_calls: Some(2),
            max_failed_tools: Some(0),
            ..BenchmarkAssertionDefaults::default()
        },
        "recovery_search_then_readback" => BenchmarkAssertionDefaults {
            expect_tool: Some("search_text".to_string()),
            expect_tool_sequence: Some(vec!["search_text".to_string(), "read_file".to_string()]),
            max_tool_calls: Some(3),
            max_failed_tools: Some(0),
            ..BenchmarkAssertionDefaults::default()
        },
        "write_validate_ok" => BenchmarkAssertionDefaults {
            expect_tool: Some("apply_patch".to_string()),
            expect_tool_sequence: Some(vec![
                "apply_patch".to_string(),
                "git_diff".to_string(),
                "run_shell".to_string(),
            ]),
            expect_last_tool_output_contains: Some("meta.result=ok".to_string()),
            max_tool_calls: Some(3),
            max_failed_tools: Some(0),
            ..BenchmarkAssertionDefaults::default()
        },
        "write_validate_repro_then_fix_ok" => BenchmarkAssertionDefaults {
            expect_tool: Some("run_shell".to_string()),
            expect_tool_sequence: Some(vec![
                "run_shell".to_string(),
                "read_file".to_string(),
                "apply_patch".to_string(),
                "git_diff".to_string(),
                "run_shell".to_string(),
            ]),
            expect_last_tool_output_contains: Some("meta.result=ok".to_string()),
            max_tool_calls: Some(5),
            max_failed_tools: Some(0),
            ..BenchmarkAssertionDefaults::default()
        },
        "write_validate_failure_readback" => BenchmarkAssertionDefaults {
            expect_tool: Some("apply_patch".to_string()),
            expect_tool_sequence: Some(vec![
                "apply_patch".to_string(),
                "git_diff".to_string(),
                "run_shell".to_string(),
                "read_file".to_string(),
            ]),
            expect_tool_output_contains: Some(ToolOutputExpectation {
                tool_name: "run_shell".to_string(),
                needle: "meta.failure_kind=test_failure".to_string(),
            }),
            max_tool_calls: Some(4),
            max_failed_tools: Some(0),
            ..BenchmarkAssertionDefaults::default()
        },
        "write_validate_retry_ok" => BenchmarkAssertionDefaults {
            expect_tool: Some("apply_patch".to_string()),
            expect_tool_sequence: Some(vec![
                "apply_patch".to_string(),
                "git_diff".to_string(),
                "run_shell".to_string(),
                "read_file".to_string(),
                "apply_patch".to_string(),
                "git_diff".to_string(),
                "run_shell".to_string(),
            ]),
            expect_last_tool_output_contains: Some("meta.result=ok".to_string()),
            max_tool_calls: Some(7),
            max_failed_tools: Some(0),
            ..BenchmarkAssertionDefaults::default()
        },
        "planning_todo_only" => BenchmarkAssertionDefaults {
            expect_tool: Some("todo_write".to_string()),
            forbid_tool: Some("apply_patch".to_string()),
            min_tool_calls: Some(1),
            ..BenchmarkAssertionDefaults::default()
        },
        "pr_review_readback" => BenchmarkAssertionDefaults {
            expect_tool: Some("read_file".to_string()),
            expect_tool_sequence: Some(vec!["read_file".to_string()]),
            max_tool_calls: Some(1),
            max_failed_tools: Some(0),
            ..BenchmarkAssertionDefaults::default()
        },
        "pr_fix_recovery" => BenchmarkAssertionDefaults {
            expect_tool: Some("read_file".to_string()),
            expect_tool_sequence: Some(vec!["read_file".to_string()]),
            max_tool_calls: Some(1),
            max_failed_tools: Some(0),
            ..BenchmarkAssertionDefaults::default()
        },
        "pr_patch_readback" => BenchmarkAssertionDefaults {
            expect_tool: Some("read_file".to_string()),
            expect_tool_sequence: Some(vec!["read_file".to_string()]),
            max_tool_calls: Some(1),
            max_failed_tools: Some(0),
            ..BenchmarkAssertionDefaults::default()
        },
        _ => {
            return Err(app_error(format!(
                "unknown benchmark assertion_bundle: {bundle}"
            )))
        }
    };
    Ok(defaults)
}

fn render_report(
    manifest_path: &Path,
    results: &[BenchmarkCaseResult],
    history: &[BenchmarkRunRecord],
    dogfood_snapshot: Option<&DogfoodSnapshot>,
    trend_gate: &TrendGateEvaluation,
    live_gate: &LiveGateEvaluation,
) -> String {
    let passed = results.iter().filter(|result| result.passed).count();
    let total_tool_calls = results
        .iter()
        .map(|result| result.tool_calls)
        .sum::<usize>();
    let total_failed_tools = results
        .iter()
        .map(|result| result.failed_tool_calls)
        .sum::<usize>();
    let previous = history.iter().rev().nth(1);
    let category_stats = collect_category_stats(results);
    let previous_category_stats = previous
        .map(|record| category_stats_by_name(&record.category_stats))
        .unwrap_or_default();
    let mut out = String::new();
    out.push_str("# DeepseekCode Benchmark Report\n\n");
    out.push_str(&format!("- Manifest: `{}`\n", manifest_path.display()));
    out.push_str(&format!("- Cases: {}\n", results.len()));
    out.push_str(&format!(
        "- Passed expectations: {passed}/{}\n",
        results.len()
    ));
    out.push_str(&format!("- Total tool calls: {total_tool_calls}\n"));
    out.push_str(&format!(
        "- Total failed tool calls: {total_failed_tools}\n"
    ));
    if let Some(previous) = previous {
        out.push_str(&format!(
            "- Previous benchmark: {}/{} passed, Δ passed {}, Δ tool calls {}, Δ failed tools {}\n",
            previous.passed,
            previous.cases,
            signed_delta(passed as i64 - previous.passed as i64),
            signed_delta(total_tool_calls as i64 - previous.total_tool_calls as i64),
            signed_delta(total_failed_tools as i64 - previous.total_failed_tool_calls as i64),
        ));
    }
    if let Some(snapshot) = dogfood_snapshot {
        out.push_str(&format!(
            "- Dogfood snapshot: runs={}, success={}, failed={}, stuck={}, manual={}\n",
            snapshot.runs, snapshot.success, snapshot.failed, snapshot.stuck, snapshot.manual
        ));
    }
    out.push_str(&format!("- Trend gate: {}\n", trend_gate.summary_line()));
    out.push_str(&format!("- Live gate: {}\n", live_gate.summary_line()));
    out.push('\n');
    out.push_str("| Case | Workdir | Passed | Budget | Tool Calls | Failed Tools | Duration ms | Notes | Tool Trace | Failure Summary | Final Message |\n");
    out.push_str("| --- | --- | --- | ---: | ---: | ---: | ---: | --- | --- | --- | --- |\n");
    for result in results {
        out.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |\n",
            escape_table(&result.case.name),
            escape_table(&clip(
                &result
                    .workdir
                    .as_deref()
                    .map(|path| path.display().to_string())
                    .unwrap_or_else(|| ".".to_string()),
                48,
            ),),
            if result.passed { "yes" } else { "no" },
            result.case.budget,
            result.tool_calls,
            result.failed_tool_calls,
            result.duration_ms,
            escape_table(&clip(result.case.notes.as_deref().unwrap_or(""), 80)),
            escape_table(&clip(&result.tool_trace, 120)),
            escape_table(&clip(&result.failure_summary, 120)),
            escape_table(&clip(&result.final_message, 120)),
        ));
    }
    if !category_stats.is_empty() {
        out.push_str("\n## Category Slices\n\n");
        out.push_str("| Category | Passed | Cases | Tool Calls | Failed Tools | Previous | Δ Passed | Δ Tool Calls | Trend Gate |\n");
        out.push_str("| --- | ---: | ---: | ---: | ---: | --- | ---: | ---: | --- |\n");
        for stats in &category_stats {
            let previous = previous_category_stats.get(stats.category.as_str());
            let trend_summary = trend_gate
                .category_summaries
                .iter()
                .find(|summary| summary.category == stats.category);
            out.push_str(&format!(
                "| {} | {} | {} | {} | {} | {} | {} | {} | {} |\n",
                escape_table(&stats.category),
                stats.passed,
                stats.cases,
                stats.total_tool_calls,
                stats.total_failed_tool_calls,
                previous
                    .map(|item| format!("{}/{}", item.passed, item.cases))
                    .unwrap_or_else(|| "-".to_string()),
                previous
                    .map(|item| signed_delta(stats.passed as i64 - item.passed as i64))
                    .unwrap_or_else(|| "-".to_string()),
                previous
                    .map(|item| {
                        signed_delta(stats.total_tool_calls as i64 - item.total_tool_calls as i64)
                    })
                    .unwrap_or_else(|| "-".to_string()),
                escape_table(&trend_summary_line(trend_summary)),
            ));
        }
    }
    if dogfood_snapshot.is_some_and(|snapshot| !snapshot.category_stats.is_empty()) {
        out.push_str("\n## Dogfood Slices\n\n");
        out.push_str("| Category | Runs | Success | Failed | Stuck | Manual | Avg Tool Calls |\n");
        out.push_str("| --- | ---: | --- | --- | --- | --- | ---: |\n");
        for stats in &dogfood_snapshot.unwrap().category_stats {
            out.push_str(&format!(
                "| {} | {} | {}/{} | {}/{} | {}/{} | {}/{} | {:.2} |\n",
                escape_table(&stats.category),
                stats.runs,
                stats.success,
                stats.runs,
                stats.failed,
                stats.runs,
                stats.stuck,
                stats.runs,
                stats.manual,
                stats.runs,
                if stats.runs == 0 {
                    0.0
                } else {
                    stats.total_tool_calls as f64 / stats.runs as f64
                },
            ));
        }
    }
    if !history.is_empty() {
        out.push_str("\n## Recent Runs\n\n");
        out.push_str("| Timestamp | Passed | Cases | Tool Calls | Failed Tools | Duration ms | Dogfood Runs | Dogfood Categories |\n");
        out.push_str("| --- | ---: | ---: | ---: | ---: | ---: | ---: | --- |\n");
        for record in history.iter().rev().take(5) {
            out.push_str(&format!(
                "| {} | {} | {} | {} | {} | {} | {} | {} |\n",
                record.timestamp_secs,
                record.passed,
                record.cases,
                record.total_tool_calls,
                record.total_failed_tool_calls,
                record.duration_ms,
                record
                    .dogfood_snapshot
                    .as_ref()
                    .map(|snapshot| snapshot.runs.to_string())
                    .unwrap_or_else(|| "-".to_string()),
                record
                    .dogfood_snapshot
                    .as_ref()
                    .map(|snapshot| {
                        snapshot
                            .category_stats
                            .iter()
                            .map(|stats| format!("{}={}", stats.category, stats.runs))
                            .collect::<Vec<_>>()
                            .join(", ")
                    })
                    .filter(|value| !value.is_empty())
                    .unwrap_or_else(|| "-".to_string()),
            ));
        }
    }
    out
}

fn evaluate_trend_gate(
    current: &BenchmarkRunRecord,
    previous_history: &[BenchmarkRunRecord],
) -> TrendGateEvaluation {
    let comparable = previous_history
        .iter()
        .rev()
        .filter(|record| record.manifest == current.manifest && record.cases == current.cases)
        .take(TREND_GATE_WINDOW)
        .cloned()
        .collect::<Vec<_>>();
    if comparable.len() < TREND_GATE_MIN_HISTORY {
        return TrendGateEvaluation {
            status: TrendGateStatus::InsufficientHistory,
            comparable_runs: comparable.len(),
            best_passed: None,
            median_tool_calls: None,
            tool_call_limit: None,
            median_failed_tool_calls: None,
            reasons: Vec::new(),
            category_summaries: Vec::new(),
        };
    }

    let best_passed = comparable
        .iter()
        .map(|record| record.passed)
        .max()
        .unwrap_or(0);
    let median_tool_calls = median_u64(
        comparable
            .iter()
            .map(|record| record.total_tool_calls)
            .collect::<Vec<_>>(),
    );
    let median_failed_tool_calls = median_u64(
        comparable
            .iter()
            .map(|record| record.total_failed_tool_calls)
            .collect::<Vec<_>>(),
    );
    let tool_call_limit = median_tool_calls + tool_call_tolerance(median_tool_calls);

    let mut reasons = Vec::new();
    if current.passed < best_passed {
        reasons.push(format!(
            "passed {} below comparable best {}",
            current.passed, best_passed
        ));
    }
    if current.total_failed_tool_calls > median_failed_tool_calls {
        reasons.push(format!(
            "failed tool calls {} above comparable median {}",
            current.total_failed_tool_calls, median_failed_tool_calls
        ));
    }
    if current.total_tool_calls > tool_call_limit {
        reasons.push(format!(
            "tool calls {} above comparable median {} + tolerance {}",
            current.total_tool_calls,
            median_tool_calls,
            tool_call_tolerance(median_tool_calls)
        ));
    }
    let mut category_summaries = current
        .category_stats
        .iter()
        .map(|category| evaluate_category_trend(category, current, previous_history))
        .collect::<Vec<_>>();
    category_summaries.sort_by(|left, right| left.category.cmp(&right.category));
    for summary in &category_summaries {
        if matches!(summary.status, TrendGateStatus::Failed) {
            reasons.extend(
                summary
                    .reasons
                    .iter()
                    .map(|reason| format!("category `{}`: {reason}", summary.category)),
            );
        }
    }

    TrendGateEvaluation {
        status: if reasons.is_empty() {
            TrendGateStatus::Passed
        } else {
            TrendGateStatus::Failed
        },
        comparable_runs: comparable.len(),
        best_passed: Some(best_passed),
        median_tool_calls: Some(median_tool_calls),
        tool_call_limit: Some(tool_call_limit),
        median_failed_tool_calls: Some(median_failed_tool_calls),
        category_summaries,
        reasons,
    }
}

fn evaluate_live_gate(
    current: &BenchmarkRunRecord,
    previous_history: &[BenchmarkRunRecord],
) -> LiveGateEvaluation {
    let Some(current_snapshot) = current.dogfood_snapshot.as_ref() else {
        return LiveGateEvaluation {
            status: TrendGateStatus::InsufficientHistory,
            current_runs: None,
            previous_runs: None,
            reasons: Vec::new(),
        };
    };
    let Some(previous_snapshot) = previous_history
        .iter()
        .rev()
        .filter_map(|record| record.dogfood_snapshot.as_ref())
        .find(|snapshot| snapshot.runs <= current_snapshot.runs)
    else {
        return LiveGateEvaluation {
            status: TrendGateStatus::InsufficientHistory,
            current_runs: Some(current_snapshot.runs),
            previous_runs: None,
            reasons: Vec::new(),
        };
    };

    let mut reasons = Vec::new();
    push_counter_regression(
        &mut reasons,
        "failed dogfood records",
        current_snapshot.failed,
        previous_snapshot.failed,
    );
    push_counter_regression(
        &mut reasons,
        "stuck dogfood records",
        current_snapshot.stuck,
        previous_snapshot.stuck,
    );
    push_counter_regression(
        &mut reasons,
        "manual dogfood records",
        current_snapshot.manual,
        previous_snapshot.manual,
    );

    if current_snapshot.runs > previous_snapshot.runs {
        let previous_categories = dogfood_category_stats_by_name(&previous_snapshot.category_stats);
        for current_category in &current_snapshot.category_stats {
            let Some(previous_category) =
                previous_categories.get(current_category.category.as_str())
            else {
                continue;
            };
            push_counter_regression(
                &mut reasons,
                &format!("category `{}` failed records", current_category.category),
                current_category.failed,
                previous_category.failed,
            );
            push_counter_regression(
                &mut reasons,
                &format!("category `{}` stuck records", current_category.category),
                current_category.stuck,
                previous_category.stuck,
            );
            push_counter_regression(
                &mut reasons,
                &format!("category `{}` manual records", current_category.category),
                current_category.manual,
                previous_category.manual,
            );
        }
    }

    LiveGateEvaluation {
        status: if reasons.is_empty() {
            TrendGateStatus::Passed
        } else {
            TrendGateStatus::Failed
        },
        current_runs: Some(current_snapshot.runs),
        previous_runs: Some(previous_snapshot.runs),
        reasons,
    }
}

fn push_counter_regression(reasons: &mut Vec<String>, label: &str, current: u64, previous: u64) {
    if current > previous {
        reasons.push(format!("{label} increased {previous} -> {current}"));
    }
}

fn evaluate_category_trend(
    current_category: &BenchmarkCategoryStats,
    current: &BenchmarkRunRecord,
    previous_history: &[BenchmarkRunRecord],
) -> CategoryTrendSummary {
    let comparable_runs = previous_history
        .iter()
        .rev()
        .filter(|record| record.manifest == current.manifest && record.cases == current.cases)
        .take(TREND_GATE_WINDOW)
        .collect::<Vec<_>>();
    let projection = category_projection_baseline(&current_category.category, &comparable_runs);
    let comparable = comparable_runs
        .into_iter()
        .filter_map(|record| {
            category_stats_for_trend(
                record,
                &current_category.category,
                current_category.cases,
                projection.as_ref(),
            )
        })
        .collect::<Vec<_>>();
    if comparable.len() < TREND_GATE_MIN_HISTORY {
        return CategoryTrendSummary {
            category: current_category.category.clone(),
            status: TrendGateStatus::InsufficientHistory,
            comparable_runs: comparable.len(),
            best_passed: None,
            median_tool_calls: None,
            tool_call_limit: None,
            median_failed_tool_calls: None,
            reasons: Vec::new(),
        };
    }

    let best_passed = comparable
        .iter()
        .map(|record| record.passed)
        .max()
        .unwrap_or(0);
    let median_tool_calls = median_u64(
        comparable
            .iter()
            .map(|record| record.total_tool_calls)
            .collect::<Vec<_>>(),
    );
    let median_failed_tool_calls = median_u64(
        comparable
            .iter()
            .map(|record| record.total_failed_tool_calls)
            .collect::<Vec<_>>(),
    );
    let tool_call_limit = median_tool_calls + tool_call_tolerance(median_tool_calls);

    let mut reasons = Vec::new();
    if current_category.passed < best_passed {
        reasons.push(format!(
            "passed {} below comparable best {}",
            current_category.passed, best_passed
        ));
    }
    if current_category.total_failed_tool_calls > median_failed_tool_calls {
        reasons.push(format!(
            "failed tool calls {} above comparable median {}",
            current_category.total_failed_tool_calls, median_failed_tool_calls
        ));
    }
    if current_category.total_tool_calls > tool_call_limit {
        reasons.push(format!(
            "tool calls {} above comparable median {} + tolerance {}",
            current_category.total_tool_calls,
            median_tool_calls,
            tool_call_tolerance(median_tool_calls)
        ));
    }

    CategoryTrendSummary {
        category: current_category.category.clone(),
        status: if reasons.is_empty() {
            TrendGateStatus::Passed
        } else {
            TrendGateStatus::Failed
        },
        comparable_runs: comparable.len(),
        best_passed: Some(best_passed),
        median_tool_calls: Some(median_tool_calls),
        tool_call_limit: Some(tool_call_limit),
        median_failed_tool_calls: Some(median_failed_tool_calls),
        reasons,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CategoryProjectionBaseline {
    category_cases: u64,
    total_cases: u64,
    category_passed: u64,
    total_passed: u64,
    category_tool_calls: u64,
    total_tool_calls: u64,
    category_failed_tool_calls: u64,
    total_failed_tool_calls: u64,
}

fn category_projection_baseline(
    category: &str,
    comparable_runs: &[&BenchmarkRunRecord],
) -> Option<CategoryProjectionBaseline> {
    let mut baseline = CategoryProjectionBaseline {
        category_cases: 0,
        total_cases: 0,
        category_passed: 0,
        total_passed: 0,
        category_tool_calls: 0,
        total_tool_calls: 0,
        category_failed_tool_calls: 0,
        total_failed_tool_calls: 0,
    };
    let mut found = false;
    for record in comparable_runs {
        let Some(stats) = record
            .category_stats
            .iter()
            .find(|stats| stats.category == category)
        else {
            continue;
        };
        found = true;
        baseline.category_cases += stats.cases;
        baseline.total_cases += record.cases;
        baseline.category_passed += stats.passed;
        baseline.total_passed += record.passed;
        baseline.category_tool_calls += stats.total_tool_calls;
        baseline.total_tool_calls += record.total_tool_calls;
        baseline.category_failed_tool_calls += stats.total_failed_tool_calls;
        baseline.total_failed_tool_calls += record.total_failed_tool_calls;
    }
    if found {
        Some(baseline)
    } else {
        None
    }
}

fn category_stats_for_trend(
    record: &BenchmarkRunRecord,
    category: &str,
    current_cases: u64,
    projection: Option<&CategoryProjectionBaseline>,
) -> Option<BenchmarkCategoryStats> {
    if let Some(stats) = record
        .category_stats
        .iter()
        .find(|stats| stats.category == category)
    {
        if stats.cases != current_cases {
            return None;
        }
        return Some(stats.clone());
    }
    let projection = projection?;
    if projection.total_cases == 0 || projection.category_cases == 0 {
        return None;
    }

    let cases = project_ratio(
        record.cases,
        projection.category_cases,
        projection.total_cases,
    )
    .min(record.cases);
    if cases == 0 {
        return None;
    }
    if cases != current_cases {
        return None;
    }

    let passed = if projection.total_passed == 0 || projection.category_passed == 0 {
        0
    } else {
        project_ratio(
            record.passed,
            projection.category_passed,
            projection.total_passed,
        )
        .min(record.passed)
        .min(cases)
    };
    let total_tool_calls =
        if projection.total_tool_calls == 0 || projection.category_tool_calls == 0 {
            0
        } else {
            project_ratio(
                record.total_tool_calls,
                projection.category_tool_calls,
                projection.total_tool_calls,
            )
            .min(record.total_tool_calls)
        };
    let total_failed_tool_calls =
        if projection.total_failed_tool_calls == 0 || projection.category_failed_tool_calls == 0 {
            0
        } else {
            project_ratio(
                record.total_failed_tool_calls,
                projection.category_failed_tool_calls,
                projection.total_failed_tool_calls,
            )
            .min(record.total_failed_tool_calls)
        };

    Some(BenchmarkCategoryStats {
        category: category.to_string(),
        cases,
        passed,
        total_tool_calls,
        total_failed_tool_calls,
    })
}

fn project_ratio(value: u64, numerator: u64, denominator: u64) -> u64 {
    if value == 0 || numerator == 0 || denominator == 0 {
        return 0;
    }
    ((value as u128 * numerator as u128) + (denominator as u128 / 2)) as u64 / denominator
}

fn median_u64(mut values: Vec<u64>) -> u64 {
    values.sort_unstable();
    let mid = values.len() / 2;
    if values.len() % 2 == 0 {
        (values[mid - 1] + values[mid]) / 2
    } else {
        values[mid]
    }
}

fn tool_call_tolerance(median_tool_calls: u64) -> u64 {
    std::cmp::max(3, (median_tool_calls + 9) / 10)
}

fn signed_delta(delta: i64) -> String {
    if delta > 0 {
        format!("+{delta}")
    } else {
        delta.to_string()
    }
}

fn category_stats_by_name<'a>(
    stats: &'a [BenchmarkCategoryStats],
) -> BTreeMap<&'a str, &'a BenchmarkCategoryStats> {
    stats
        .iter()
        .map(|item| (item.category.as_str(), item))
        .collect()
}

fn dogfood_category_stats_by_name<'a>(
    stats: &'a [DogfoodCategorySnapshot],
) -> BTreeMap<&'a str, &'a DogfoodCategorySnapshot> {
    stats
        .iter()
        .map(|item| (item.category.as_str(), item))
        .collect()
}

fn trend_summary_line(summary: Option<&CategoryTrendSummary>) -> String {
    let Some(summary) = summary else {
        return "-".to_string();
    };
    match summary.status {
        TrendGateStatus::Passed => format!(
            "pass vs {} run{}",
            summary.comparable_runs,
            if summary.comparable_runs == 1 {
                ""
            } else {
                "s"
            }
        ),
        TrendGateStatus::Failed => format!("FAILED: {}", summary.reasons.join("; ")),
        TrendGateStatus::InsufficientHistory => format!(
            "skipped ({}/{})",
            summary.comparable_runs, TREND_GATE_MIN_HISTORY
        ),
    }
}

fn collect_category_stats(results: &[BenchmarkCaseResult]) -> Vec<BenchmarkCategoryStats> {
    let mut grouped = BTreeMap::<String, BenchmarkCategoryStats>::new();
    for result in results {
        let entry = grouped
            .entry(result.case.category.clone())
            .or_insert_with(|| BenchmarkCategoryStats {
                category: result.case.category.clone(),
                cases: 0,
                passed: 0,
                total_tool_calls: 0,
                total_failed_tool_calls: 0,
            });
        entry.cases += 1;
        if result.passed {
            entry.passed += 1;
        }
        entry.total_tool_calls += result.tool_calls as u64;
        entry.total_failed_tool_calls += result.failed_tool_calls as u64;
    }
    grouped.into_values().collect()
}

fn read_category_stats(
    root: &BTreeMap<String, JsonValue>,
) -> AppResult<Vec<BenchmarkCategoryStats>> {
    let Some(items) = root.get("category_stats").and_then(json_as_array) else {
        return Ok(Vec::new());
    };
    let mut stats = Vec::with_capacity(items.len());
    for item in items {
        let object = json_as_object(item).ok_or_else(|| {
            app_error("benchmark history `category_stats` entries must be objects")
        })?;
        stats.push(BenchmarkCategoryStats {
            category: read_string(object, "category")?.to_string(),
            cases: read_u64(object, "cases")?,
            passed: read_u64(object, "passed")?,
            total_tool_calls: read_u64(object, "total_tool_calls")?,
            total_failed_tool_calls: read_u64(object, "total_failed_tool_calls")?,
        });
    }
    Ok(stats)
}

fn infer_case_category(case: &PendingCase) -> String {
    if case.isolate_workdir
        || case.expect_tool.as_deref() == Some("apply_patch")
        || case
            .expect_tool_sequence
            .as_ref()
            .is_some_and(|sequence| sequence.iter().any(|tool| tool == "apply_patch"))
    {
        return "write_validate".to_string();
    }
    if case.expect_tool.as_deref() == Some("todo_write")
        || case.task.as_deref().is_some_and(|task| {
            let lower = task.to_ascii_lowercase();
            lower.contains("plan ") || lower.starts_with("plan ")
        })
    {
        return "planning".to_string();
    }
    if case
        .name
        .as_deref()
        .is_some_and(|name| name.contains("recover"))
        || case
            .seed_observations
            .iter()
            .any(|observation| observation.tool_name == "recovery_hint")
    {
        return "recovery".to_string();
    }
    if case
        .task
        .as_deref()
        .is_some_and(|task| task.contains("dispatch_subagent"))
        || case
            .notes
            .as_deref()
            .is_some_and(|notes| notes.contains("subagent"))
    {
        return "subagent".to_string();
    }
    "read_only".to_string()
}

fn resolve_case_workdir(
    manifest_dir: &Path,
    raw_workdir: Option<&str>,
) -> AppResult<Option<PathBuf>> {
    let Some(raw_workdir) = raw_workdir.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    let path = Path::new(raw_workdir);
    let resolved = if path.is_absolute() {
        path.to_path_buf()
    } else {
        manifest_dir.join(path)
    };
    if !resolved.is_dir() {
        return Err(app_error(format!(
            "benchmark workdir does not exist or is not a directory: {}",
            resolved.display()
        )));
    }
    Ok(Some(resolved))
}

fn prepare_case_workdir(
    workdir: Option<&Path>,
    isolate_workdir: bool,
) -> AppResult<(Option<PathBuf>, Option<PathBuf>)> {
    let Some(workdir) = workdir else {
        return Ok((None, None));
    };
    if !isolate_workdir {
        return Ok((Some(workdir.to_path_buf()), None));
    }

    let temp_root = env::temp_dir().join(format!(
        "deepseek-bench-{}-{}",
        std::process::id(),
        next_temp_suffix()
    ));
    copy_dir_recursive(workdir, &temp_root)?;
    Ok((Some(temp_root.clone()), Some(temp_root)))
}

fn benchmark_cwd_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

fn run_case_in_workdir<T>(
    workdir: Option<&Path>,
    auto_approve: bool,
    f: impl FnOnce() -> AppResult<T>,
) -> AppResult<T> {
    let _cwd_guard = benchmark_cwd_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let previous = env::current_dir()?;
    let previous_auto_approve_writes = env::var_os("DSCODE_AUTO_APPROVE_WRITES");
    let previous_auto_approve_shell = env::var_os("DSCODE_AUTO_APPROVE_SHELL");
    if let Some(workdir) = workdir {
        env::set_current_dir(workdir)?;
    }
    if auto_approve {
        unsafe {
            env::set_var("DSCODE_AUTO_APPROVE_WRITES", "1");
            env::set_var("DSCODE_AUTO_APPROVE_SHELL", "1");
        }
    }
    let result = f();
    let restore_result = env::set_current_dir(previous);
    if auto_approve {
        restore_env_var("DSCODE_AUTO_APPROVE_WRITES", previous_auto_approve_writes);
        restore_env_var("DSCODE_AUTO_APPROVE_SHELL", previous_auto_approve_shell);
    }
    match (result, restore_result) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(error), Ok(())) => Err(error),
        (Ok(_), Err(error)) => Err(app_error(format!(
            "failed to restore benchmark cwd: {error}"
        ))),
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

fn next_temp_suffix() -> u64 {
    static COUNTER: OnceLock<AtomicU64> = OnceLock::new();
    COUNTER
        .get_or_init(|| AtomicU64::new(1))
        .fetch_add(1, Ordering::Relaxed)
}

fn append_history_record(path: &Path, record: &BenchmarkRunRecord) -> AppResult<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    writeln!(file, "{}", record.to_json_line())?;
    Ok(())
}

fn load_history(path: &Path) -> AppResult<Vec<BenchmarkRunRecord>> {
    let file = match fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(app_error(format!(
                "failed to read benchmark history {}: {error}",
                path.display()
            )))
        }
    };
    let reader = BufReader::new(file);
    let mut records = Vec::new();
    for (index, line) in reader.lines().enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let record = BenchmarkRunRecord::from_json_line(&line).map_err(|error| {
            app_error(format!(
                "failed to parse benchmark history line {} in {}: {error}",
                index + 1,
                path.display()
            ))
        })?;
        records.push(record);
    }
    Ok(records)
}

fn load_dogfood_snapshot(path: &Path) -> AppResult<Option<DogfoodSnapshot>> {
    let file = match fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(app_error(format!(
                "failed to read dogfood ledger {}: {error}",
                path.display()
            )))
        }
    };
    let reader = BufReader::new(file);
    let mut snapshot = DogfoodSnapshot {
        runs: 0,
        success: 0,
        failed: 0,
        stuck: 0,
        manual: 0,
        category_stats: Vec::new(),
    };
    let mut categories = BTreeMap::<String, DogfoodCategorySnapshot>::new();
    for (index, line) in reader.lines().enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let root = parse_root_object(&line).map_err(|error| {
            app_error(format!(
                "failed to parse dogfood ledger line {} in {}: {error}",
                index + 1,
                path.display()
            ))
        })?;
        let outcome = read_string(&root, "outcome")?;
        let manual_intervention = read_optional_bool(&root, "manual_intervention").unwrap_or(false);
        let task = read_string(&root, "task")?;
        let tool_trace = read_optional_string(&root, "tool_trace").unwrap_or("");
        let failed_tool_calls = read_optional_u64(&root, "failed_tool_calls").unwrap_or(0);
        let repeated_call_failures =
            read_optional_u64(&root, "repeated_call_failures").unwrap_or(0);
        let used_subagent = read_optional_bool(&root, "used_subagent").unwrap_or(false);
        let benchmark_seed_observations =
            read_optional_string(&root, "benchmark_seed_observations");
        let category = resolved_benchmark_category(
            read_optional_string(&root, "benchmark_category"),
            task,
            tool_trace,
            failed_tool_calls,
            repeated_call_failures,
            used_subagent,
            benchmark_seed_observations,
        )
        .into_owned();
        let tool_calls = read_optional_u64(&root, "tool_calls").unwrap_or(0);
        snapshot.runs += 1;
        match outcome {
            "success" => snapshot.success += 1,
            "failed" => snapshot.failed += 1,
            "stuck" => snapshot.stuck += 1,
            "manual" => snapshot.manual += 1,
            _ => {}
        }
        if manual_intervention && outcome != "manual" {
            snapshot.manual += 1;
        }
        let stats = categories
            .entry(category.clone())
            .or_insert_with(|| DogfoodCategorySnapshot {
                category: category.clone(),
                runs: 0,
                success: 0,
                failed: 0,
                stuck: 0,
                manual: 0,
                total_tool_calls: 0,
            });
        stats.runs += 1;
        stats.total_tool_calls += tool_calls;
        match outcome {
            "success" => stats.success += 1,
            "failed" => stats.failed += 1,
            "stuck" => stats.stuck += 1,
            _ => {}
        }
        if manual_intervention || outcome == "manual" {
            stats.manual += 1;
        }
    }
    snapshot.category_stats = categories.into_values().collect();
    snapshot
        .category_stats
        .sort_by(|left, right| left.category.cmp(&right.category));
    Ok(Some(snapshot))
}

fn read_dogfood_category_stats(
    root: &BTreeMap<String, JsonValue>,
) -> AppResult<Vec<DogfoodCategorySnapshot>> {
    let Some(items) = root.get("dogfood_category_stats").and_then(json_as_array) else {
        return Ok(Vec::new());
    };
    let mut stats = Vec::with_capacity(items.len());
    for item in items {
        let object = json_as_object(item).ok_or_else(|| {
            app_error("benchmark history `dogfood_category_stats` entries must be objects")
        })?;
        stats.push(DogfoodCategorySnapshot {
            category: read_string(object, "category")?.to_string(),
            runs: read_u64(object, "runs")?,
            success: read_u64(object, "success")?,
            failed: read_u64(object, "failed")?,
            stuck: read_u64(object, "stuck")?,
            manual: read_u64(object, "manual")?,
            total_tool_calls: read_u64(object, "total_tool_calls")?,
        });
    }
    Ok(stats)
}

fn contains_tool_sequence(actual: &[&str], expected: &[String]) -> bool {
    if expected.is_empty() {
        return true;
    }
    let mut index = 0usize;
    for tool in actual {
        if *tool == expected[index] {
            index += 1;
            if index == expected.len() {
                return true;
            }
        }
    }
    false
}

fn parse_seed_observations(value: &str) -> AppResult<Vec<crate::model::protocol::Observation>> {
    if value.trim().is_empty() {
        return Ok(Vec::new());
    }

    value
        .split(" || ")
        .map(|entry| {
            let mut parts = entry.splitn(3, ':');
            let tool_name = parts
                .next()
                .map(str::trim)
                .filter(|part| !part.is_empty())
                .ok_or_else(|| app_error(format!("invalid seed observation entry: {entry}")))?;
            let status = parts
                .next()
                .map(str::trim)
                .filter(|part| !part.is_empty())
                .ok_or_else(|| app_error(format!("invalid seed observation entry: {entry}")))?;
            let summary = parts
                .next()
                .map(str::trim)
                .filter(|part| !part.is_empty())
                .ok_or_else(|| app_error(format!("invalid seed observation entry: {entry}")))?;

            match status {
                "ok" => Ok(crate::model::protocol::Observation::ok(tool_name, summary)),
                "failed" => Ok(crate::model::protocol::Observation::failed(
                    tool_name, summary,
                )),
                _ => Err(app_error(format!(
                    "invalid seed observation status `{status}` in `{entry}`"
                ))),
            }
        })
        .collect()
}

pub(crate) fn format_seed_observations(observations: &[Observation]) -> Option<String> {
    if observations.is_empty() {
        return None;
    }
    Some(
        observations
            .iter()
            .map(|observation| {
                format!(
                    "{}:{}:{}",
                    observation.tool_name,
                    match observation.status {
                        crate::model::protocol::ObservationStatus::Ok => "ok",
                        crate::model::protocol::ObservationStatus::Failed => "failed",
                    },
                    observation.summary.replace('\n', "\\n")
                )
            })
            .collect::<Vec<_>>()
            .join(" || "),
    )
}

fn parse_manifest_usize(key: &str, value: &str) -> AppResult<usize> {
    value
        .parse::<usize>()
        .map_err(|_| app_error(format!("invalid value for {key}: {value}")))
}

fn parse_manifest_string_list(key: &str, value: &str) -> AppResult<Vec<String>> {
    let trimmed = value.trim();
    let inner = if trimmed.starts_with('[') || trimmed.ends_with(']') {
        trimmed
            .strip_prefix('[')
            .and_then(|value| value.strip_suffix(']'))
            .ok_or_else(|| app_error(format!("invalid value for {key}: {value}")))?
            .trim()
    } else {
        trimmed
    };
    Ok(inner
        .split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(|part| part.trim_matches('"').trim_matches('\'').to_string())
        .filter(|part| !part.is_empty())
        .collect())
}

fn parse_tool_output_expectation(value: &str) -> AppResult<ToolOutputExpectation> {
    let Some((tool_name, needle)) = value.split_once(':') else {
        return Err(app_error(format!(
            "invalid expect_tool_output_contains value `{value}`; expected `tool_name:needle`"
        )));
    };
    let tool_name = tool_name.trim();
    let needle = needle.trim();
    if tool_name.is_empty() || needle.is_empty() {
        return Err(app_error(format!(
            "invalid expect_tool_output_contains value `{value}`; expected `tool_name:needle`"
        )));
    }
    Ok(ToolOutputExpectation {
        tool_name: tool_name.to_string(),
        needle: needle.to_string(),
    })
}

fn parse_manifest_bool(key: &str, value: &str) -> AppResult<bool> {
    match value.trim() {
        "true" => Ok(true),
        "false" => Ok(false),
        other => Err(app_error(format!("invalid value for {key}: {other}"))),
    }
}

fn unix_now_secs() -> AppResult<u64> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| app_error(format!("system clock error: {error}")))?
        .as_secs())
}

fn read_string<'a>(root: &'a BTreeMap<String, JsonValue>, key: &str) -> AppResult<&'a str> {
    root.get(key)
        .and_then(json_as_string)
        .ok_or_else(|| app_error(format!("missing string `{key}`")))
}

fn read_u64(root: &BTreeMap<String, JsonValue>, key: &str) -> AppResult<u64> {
    root.get(key)
        .and_then(json_as_u64)
        .ok_or_else(|| app_error(format!("missing numeric `{key}`")))
}

fn read_optional_u64(root: &BTreeMap<String, JsonValue>, key: &str) -> Option<u64> {
    root.get(key).and_then(json_as_u64)
}

fn read_optional_string<'a>(root: &'a BTreeMap<String, JsonValue>, key: &str) -> Option<&'a str> {
    root.get(key).and_then(json_as_string)
}

fn read_optional_bool(root: &BTreeMap<String, JsonValue>, key: &str) -> Option<bool> {
    match root.get(key) {
        Some(JsonValue::Bool(value)) => Some(*value),
        _ => None,
    }
}

fn unquote(value: &str) -> String {
    let trimmed = value.trim().trim_matches('"');
    let mut out = String::with_capacity(trimmed.len());
    let mut chars = trimmed.chars();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('\\') => out.push('\\'),
                Some('"') => out.push('"'),
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
                None => out.push('\\'),
            }
        } else {
            out.push(ch);
        }
    }
    out
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

    #[test]
    fn parse_manifest_supports_multiple_cases_and_defaults_budget() {
        let manifest = r#"
name = "inspect"
task = "inspect repository"
category = "read_only"
assertion_bundle = "read_only_inspect"
workdir = "fixtures/rust-cli-mini"
isolate_workdir = true
seed_observations = "search_text:ok:src/lib.rs:1: fn main()"
max_failed_tools = 0

name = "write-tests"
task = "write tests for parser"
category = "planning"
assertion_bundle = "planning_todo_only"
skill = "write-tests"
budget = 12
expect_message_contains = "done"
expect_tool_output_contains = "run_shell:meta.result=ok"
expect_last_tool_output_contains = "meta.result=ok"
max_tool_calls = 5
"#;
        let cases = parse_manifest(manifest).unwrap();
        assert_eq!(cases.len(), 2);
        assert_eq!(cases[0].name, "inspect");
        assert_eq!(cases[0].category, "read_only");
        assert_eq!(cases[0].workdir.as_deref(), Some("fixtures/rust-cli-mini"));
        assert!(cases[0].isolate_workdir);
        assert_eq!(cases[0].budget, 8);
        assert_eq!(cases[0].max_failed_tools, Some(0));
        assert_eq!(cases[0].seed_observations.len(), 1);
        assert_eq!(cases[0].seed_observations[0].tool_name, "search_text");
        assert_eq!(
            cases[0].expect_tool_sequence,
            Some(vec!["list_files".to_string(), "read_file".to_string()])
        );
        assert_eq!(cases[0].expect_tool.as_deref(), Some("list_files"));
        assert_eq!(
            cases[1].expect_last_tool_output_contains.as_deref(),
            Some("meta.result=ok")
        );
        assert_eq!(cases[1].expect_tool.as_deref(), Some("todo_write"));
        assert_eq!(cases[1].forbid_tool.as_deref(), Some("apply_patch"));
        assert_eq!(
            cases[1].expect_tool_output_contains,
            Some(ToolOutputExpectation {
                tool_name: "run_shell".to_string(),
                needle: "meta.result=ok".to_string(),
            })
        );
        assert_eq!(cases[1].category, "planning");
        assert_eq!(cases[1].skill.as_deref(), Some("write-tests"));
        assert_eq!(cases[1].budget, 12);
        assert_eq!(cases[1].max_tool_calls, Some(5));
    }

    #[test]
    fn parse_manifest_bundle_defaults_allow_explicit_overrides() {
        let manifest = r#"
name = "retry-validate"
task = "repair validation failures until tests pass"
assertion_bundle = "write_validate_retry_ok"
expect_tool_output_contains = "read_file:2     a * b"
max_tool_calls = 9
"#;
        let cases = parse_manifest(manifest).unwrap();
        assert_eq!(cases.len(), 1);
        assert_eq!(cases[0].expect_tool.as_deref(), Some("apply_patch"));
        assert_eq!(
            cases[0].expect_tool_sequence,
            Some(vec![
                "apply_patch".to_string(),
                "git_diff".to_string(),
                "run_shell".to_string(),
                "read_file".to_string(),
                "apply_patch".to_string(),
                "git_diff".to_string(),
                "run_shell".to_string(),
            ])
        );
        assert_eq!(
            cases[0].expect_tool_output_contains,
            Some(ToolOutputExpectation {
                tool_name: "read_file".to_string(),
                needle: "2     a * b".to_string(),
            })
        );
        assert_eq!(
            cases[0].expect_last_tool_output_contains.as_deref(),
            Some("meta.result=ok")
        );
        assert_eq!(cases[0].max_tool_calls, Some(9));
    }

    #[test]
    fn parse_manifest_rejects_unknown_assertion_bundle() {
        let error = parse_manifest(
            r#"
name = "bad-bundle"
task = "inspect repository"
assertion_bundle = "does_not_exist"
"#,
        )
        .unwrap_err();
        assert!(error
            .to_string()
            .contains("unknown benchmark assertion_bundle"));
    }

    #[test]
    fn load_manifest_case_summaries_preserves_seed_observations() {
        let root =
            std::env::temp_dir().join(format!("deepseek-benchmark-summary-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let manifest_path = root.join("benchmarks.txt");
        fs::write(
            &manifest_path,
            r#"name = "recover-case"
task = "investigate parser"
category = "recovery"
skill = "debug"
workdir = "fixtures/rust-cli-mini"
isolate_workdir = true
budget = 7
notes = "fixture-backed recovery"
seed_observations = "search_text:failed:no matches || recovery_hint:ok:after=search_text; next=list_files"
"#,
        )
        .unwrap();

        let summaries = load_manifest_case_summaries(&manifest_path).unwrap();
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].name, "recover-case");
        assert_eq!(summaries[0].category, "recovery");
        assert_eq!(summaries[0].skill.as_deref(), Some("debug"));
        assert_eq!(
            summaries[0].workdir.as_deref(),
            Some("fixtures/rust-cli-mini")
        );
        assert!(summaries[0].isolate_workdir);
        assert_eq!(summaries[0].budget, 7);
        assert_eq!(
            summaries[0].notes.as_deref(),
            Some("fixture-backed recovery")
        );
        assert_eq!(
            summaries[0].seed_observations.as_deref(),
            Some(
                "search_text:failed:no matches || recovery_hint:ok:after=search_text; next=list_files"
            )
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn parse_manifest_rejects_unknown_keys() {
        let manifest = "name = \"x\"\ntask = \"y\"\nfoo = \"bar\"\n";
        let error = parse_manifest(manifest).unwrap_err();
        assert!(error.to_string().contains("unknown benchmark manifest key"));
    }

    #[test]
    fn unquote_decodes_newline_escapes() {
        assert_eq!(unquote("\"a\\nb\""), "a\nb");
        assert_eq!(unquote("\"x\\\\y\""), "x\\y");
    }

    #[test]
    fn render_report_includes_summary_table() {
        let trend_gate = TrendGateEvaluation {
            status: TrendGateStatus::InsufficientHistory,
            comparable_runs: 0,
            best_passed: None,
            median_tool_calls: None,
            tool_call_limit: None,
            median_failed_tool_calls: None,
            category_summaries: Vec::new(),
            reasons: Vec::new(),
        };
        let live_gate = LiveGateEvaluation {
            status: TrendGateStatus::InsufficientHistory,
            current_runs: None,
            previous_runs: None,
            reasons: Vec::new(),
        };
        let report = render_report(
            Path::new(".dscode/benchmarks.txt"),
            &[BenchmarkCaseResult {
                case: BenchmarkCase {
                    name: "inspect".to_string(),
                    task: "inspect repository".to_string(),
                    category: "read_only".to_string(),
                    skill: None,
                    workdir: Some("fixtures/rust-cli-mini".to_string()),
                    isolate_workdir: false,
                    budget: 8,
                    seed_observations: Vec::new(),
                    expect_tool: Some("list_files".to_string()),
                    expect_tool_sequence: Some(vec![
                        "list_files".to_string(),
                        "read_file".to_string(),
                    ]),
                    forbid_tool: None,
                    expect_message_contains: None,
                    expect_tool_output_contains: None,
                    expect_last_tool_output_contains: None,
                    min_tool_calls: None,
                    max_tool_calls: None,
                    max_failed_tools: Some(0),
                    notes: None,
                },
                workdir: Some(PathBuf::from("fixtures/rust-cli-mini")),
                duration_ms: 15,
                passed: true,
                failed_tool_calls: 0,
                tool_calls: 2,
                tool_trace: "list_files -> read_file".to_string(),
                final_message: "finished cleanly".to_string(),
                failure_summary: "ok".to_string(),
            }],
            &[],
            None,
            &trend_gate,
            &live_gate,
        );
        assert!(report.contains("# DeepseekCode Benchmark Report"));
        assert!(report.contains("- Total tool calls: 2"));
        assert!(report.contains("Trend gate: skipped"));
        assert!(report.contains("Live gate: skipped"));
        assert!(report.contains("| inspect | fixtures/rust-cli-mini | yes | 8 | 2 | 0 | 15 |  | list_files -> read_file | ok | finished cleanly |"));
    }

    #[test]
    fn benchmark_evaluation_reports_unmet_expectations() {
        let case = BenchmarkCase {
            name: "inspect".to_string(),
            task: "inspect repository".to_string(),
            category: "read_only".to_string(),
            skill: None,
            workdir: None,
            isolate_workdir: false,
            budget: 8,
            seed_observations: Vec::new(),
            expect_tool: Some("search_text".to_string()),
            expect_tool_sequence: Some(vec!["search_text".to_string(), "read_file".to_string()]),
            forbid_tool: Some("list_files".to_string()),
            expect_message_contains: Some("needle".to_string()),
            expect_tool_output_contains: Some(ToolOutputExpectation {
                tool_name: "run_shell".to_string(),
                needle: "meta.result=ok".to_string(),
            }),
            expect_last_tool_output_contains: Some("meta.result=ok".to_string()),
            min_tool_calls: Some(2),
            max_tool_calls: Some(1),
            max_failed_tools: Some(0),
            notes: None,
        };
        let result = RunResult {
            final_message: "finished cleanly".to_string(),
            tool_events: vec![crate::core::loop_runtime::ToolEvent {
                tool_name: "list_files".to_string(),
                input: std::collections::BTreeMap::new(),
                output: "ok".to_string(),
                status: crate::model::protocol::ObservationStatus::Failed,
            }],
            usage: crate::model::protocol::TokenUsage::default(),
        };
        let evaluation = case.evaluate(&result);
        assert!(!evaluation.passed);
        assert!(evaluation
            .failures
            .iter()
            .any(|failure| failure.contains("expected tool `search_text`")));
        assert!(evaluation
            .failures
            .iter()
            .any(|failure| failure.contains("tool sequence `search_text -> read_file`")));
        assert!(evaluation
            .failures
            .iter()
            .any(|failure| failure.contains("forbidden tool `list_files`")));
        assert!(evaluation
            .failures
            .iter()
            .any(|failure| failure.contains(
                "tool `run_shell` output did not contain `meta.result=ok` because the tool was never called"
            )));
        assert!(evaluation.failures.iter().any(|failure| failure
            .contains("last tool `list_files` output did not contain `meta.result=ok`")));
        assert!(evaluation
            .failures
            .iter()
            .any(|failure| failure.contains("failed tool calls 1 exceeded max 0")));
    }

    #[test]
    fn contains_tool_sequence_matches_ordered_subsequence() {
        let actual = ["search_text", "list_files", "read_file"];
        let expected = vec!["search_text".to_string(), "read_file".to_string()];
        assert!(contains_tool_sequence(&actual, &expected));
        let wrong = vec!["read_file".to_string(), "search_text".to_string()];
        assert!(!contains_tool_sequence(&actual, &wrong));
    }

    #[test]
    fn parse_manifest_string_list_supports_bracketed_arrays() {
        assert_eq!(
            parse_manifest_string_list("expect_tool_sequence", "[\"read_file\", \"list_files\"]")
                .unwrap(),
            vec!["read_file".to_string(), "list_files".to_string()]
        );
        assert_eq!(
            parse_manifest_string_list("expect_tool_sequence", "read_file, list_files").unwrap(),
            vec!["read_file".to_string(), "list_files".to_string()]
        );
    }

    #[test]
    fn parse_seed_observations_supports_ok_and_failed_entries() {
        let observations = parse_seed_observations(
            "read_file:failed:No such file || recovery_hint:ok:after=read_file; next=search_text; reason=retry search",
        )
        .unwrap();
        assert_eq!(observations.len(), 2);
        assert_eq!(observations[0].tool_name, "read_file");
        assert!(observations[0].is_failure());
        assert_eq!(observations[1].tool_name, "recovery_hint");
        assert!(!observations[1].is_failure());
    }

    #[test]
    fn resolve_case_workdir_uses_manifest_relative_paths() {
        let manifest_dir = Path::new(".dscode");
        let resolved = resolve_case_workdir(manifest_dir, Some("fixtures")).unwrap();
        assert_eq!(resolved, Some(PathBuf::from(".dscode/fixtures")));
    }

    #[test]
    fn prepare_case_workdir_clones_fixture_when_isolation_enabled() {
        let root = std::env::temp_dir().join(format!(
            "deepseek-bench-fixture-{}-{}",
            std::process::id(),
            next_temp_suffix()
        ));
        let fixture = root.join("fixture");
        fs::create_dir_all(&fixture).unwrap();
        fs::write(fixture.join("note.txt"), "original").unwrap();

        let (execution, cleanup) = prepare_case_workdir(Some(&fixture), true).unwrap();
        let execution = execution.unwrap();
        assert_ne!(execution, fixture);
        assert_eq!(
            fs::read_to_string(execution.join("note.txt")).unwrap(),
            "original"
        );

        fs::write(execution.join("note.txt"), "mutated").unwrap();
        assert_eq!(
            fs::read_to_string(fixture.join("note.txt")).unwrap(),
            "original"
        );

        if let Some(path) = cleanup.as_deref() {
            let _ = fs::remove_dir_all(path);
        }
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn isolated_case_sets_auto_approve_env_temporarily() {
        unsafe {
            env::remove_var("DSCODE_AUTO_APPROVE_WRITES");
            env::remove_var("DSCODE_AUTO_APPROVE_SHELL");
        }

        let snapshot = run_case_in_workdir(None, true, || {
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
    }

    #[test]
    fn benchmark_history_round_trip_preserves_core_metrics() {
        let record = BenchmarkRunRecord {
            version: 3,
            timestamp_secs: 42,
            manifest: ".dscode/benchmarks.txt".to_string(),
            cases: 19,
            passed: 18,
            total_tool_calls: 66,
            total_failed_tool_calls: 0,
            duration_ms: 250,
            category_stats: vec![
                BenchmarkCategoryStats {
                    category: "read_only".to_string(),
                    cases: 10,
                    passed: 10,
                    total_tool_calls: 20,
                    total_failed_tool_calls: 0,
                },
                BenchmarkCategoryStats {
                    category: "write_validate".to_string(),
                    cases: 9,
                    passed: 8,
                    total_tool_calls: 46,
                    total_failed_tool_calls: 0,
                },
            ],
            dogfood_snapshot: Some(DogfoodSnapshot {
                runs: 3,
                success: 2,
                failed: 1,
                stuck: 0,
                manual: 1,
                category_stats: vec![DogfoodCategorySnapshot {
                    category: "read_only".to_string(),
                    runs: 2,
                    success: 1,
                    failed: 1,
                    stuck: 0,
                    manual: 1,
                    total_tool_calls: 6,
                }],
            }),
        };
        let decoded = BenchmarkRunRecord::from_json_line(&record.to_json_line()).unwrap();
        assert_eq!(decoded.timestamp_secs, 42);
        assert_eq!(decoded.passed, 18);
        assert_eq!(decoded.category_stats.len(), 2);
        assert_eq!(decoded.dogfood_snapshot.as_ref().map(|v| v.runs), Some(3));
        assert_eq!(
            decoded
                .dogfood_snapshot
                .as_ref()
                .and_then(|v| v.category_stats.first())
                .map(|v| v.category.as_str()),
            Some("read_only")
        );
    }

    #[test]
    fn render_report_includes_previous_run_delta_and_recent_runs() {
        let result = BenchmarkCaseResult {
            case: BenchmarkCase {
                name: "inspect".to_string(),
                task: "inspect repository".to_string(),
                category: "read_only".to_string(),
                skill: None,
                workdir: Some("fixtures/rust-cli-mini".to_string()),
                isolate_workdir: false,
                budget: 8,
                seed_observations: Vec::new(),
                expect_tool: Some("list_files".to_string()),
                expect_tool_sequence: Some(vec!["list_files".to_string(), "read_file".to_string()]),
                forbid_tool: None,
                expect_message_contains: None,
                expect_tool_output_contains: None,
                expect_last_tool_output_contains: None,
                min_tool_calls: None,
                max_tool_calls: None,
                max_failed_tools: Some(0),
                notes: None,
            },
            workdir: Some(PathBuf::from("fixtures/rust-cli-mini")),
            duration_ms: 15,
            passed: true,
            failed_tool_calls: 0,
            tool_calls: 2,
            tool_trace: "list_files -> read_file".to_string(),
            final_message: "finished cleanly".to_string(),
            failure_summary: "ok".to_string(),
        };
        let history = vec![
            BenchmarkRunRecord {
                version: 2,
                timestamp_secs: 100,
                manifest: ".dscode/benchmarks.txt".to_string(),
                cases: 1,
                passed: 0,
                total_tool_calls: 4,
                total_failed_tool_calls: 1,
                duration_ms: 30,
                category_stats: vec![BenchmarkCategoryStats {
                    category: "read_only".to_string(),
                    cases: 1,
                    passed: 0,
                    total_tool_calls: 4,
                    total_failed_tool_calls: 1,
                }],
                dogfood_snapshot: Some(DogfoodSnapshot {
                    runs: 2,
                    success: 1,
                    failed: 1,
                    stuck: 0,
                    manual: 1,
                    category_stats: vec![DogfoodCategorySnapshot {
                        category: "recovery".to_string(),
                        runs: 2,
                        success: 1,
                        failed: 1,
                        stuck: 0,
                        manual: 1,
                        total_tool_calls: 7,
                    }],
                }),
            },
            BenchmarkRunRecord {
                version: 2,
                timestamp_secs: 101,
                manifest: ".dscode/benchmarks.txt".to_string(),
                cases: 1,
                passed: 1,
                total_tool_calls: 2,
                total_failed_tool_calls: 0,
                duration_ms: 15,
                category_stats: vec![BenchmarkCategoryStats {
                    category: "read_only".to_string(),
                    cases: 1,
                    passed: 1,
                    total_tool_calls: 2,
                    total_failed_tool_calls: 0,
                }],
                dogfood_snapshot: Some(DogfoodSnapshot {
                    runs: 3,
                    success: 2,
                    failed: 1,
                    stuck: 0,
                    manual: 1,
                    category_stats: vec![DogfoodCategorySnapshot {
                        category: "read_only".to_string(),
                        runs: 3,
                        success: 2,
                        failed: 1,
                        stuck: 0,
                        manual: 1,
                        total_tool_calls: 8,
                    }],
                }),
            },
        ];
        let trend_gate = TrendGateEvaluation {
            status: TrendGateStatus::Passed,
            comparable_runs: 3,
            best_passed: Some(1),
            median_tool_calls: Some(2),
            tool_call_limit: Some(5),
            median_failed_tool_calls: Some(0),
            category_summaries: vec![CategoryTrendSummary {
                category: "read_only".to_string(),
                status: TrendGateStatus::Passed,
                comparable_runs: 3,
                best_passed: Some(1),
                median_tool_calls: Some(2),
                tool_call_limit: Some(5),
                median_failed_tool_calls: Some(0),
                reasons: Vec::new(),
            }],
            reasons: Vec::new(),
        };
        let live_gate = evaluate_live_gate(history.last().unwrap(), &history[..history.len() - 1]);
        let report = render_report(
            Path::new(".dscode/benchmarks.txt"),
            &[result],
            &history,
            history
                .last()
                .and_then(|record| record.dogfood_snapshot.as_ref()),
            &trend_gate,
            &live_gate,
        );
        assert!(report.contains("Previous benchmark: 0/1 passed"));
        assert!(report.contains("Δ passed +1"));
        assert!(report.contains("Dogfood snapshot: runs=3, success=2, failed=1, stuck=0, manual=1"));
        assert!(report.contains("Trend gate: pass against 3 comparable runs"));
        assert!(report.contains("Live gate: pass against previous dogfood snapshot"));
        assert!(report.contains("## Category Slices"));
        assert!(report.contains("## Dogfood Slices"));
        assert!(report.contains("| read_only | 3 | 2/3 | 1/3 | 0/3 | 1/3 | 2.67 |"));
        assert!(report.contains("| read_only | 1 | 1 | 2 | 0 | 0/1 | +1 | -2 | pass vs 3 runs |"));
        assert!(report.contains("## Recent Runs"));
        assert!(report.contains("read_only=3"));
    }

    #[test]
    fn benchmark_evaluation_accepts_last_tool_output_expectation() {
        let case = BenchmarkCase {
            name: "validate".to_string(),
            task: "validate changes".to_string(),
            category: "write_validate".to_string(),
            skill: None,
            workdir: None,
            isolate_workdir: false,
            budget: 4,
            seed_observations: Vec::new(),
            expect_tool: Some("run_shell".to_string()),
            expect_tool_sequence: Some(vec!["run_shell".to_string()]),
            forbid_tool: None,
            expect_message_contains: None,
            expect_tool_output_contains: None,
            expect_last_tool_output_contains: Some("meta.result=ok".to_string()),
            min_tool_calls: None,
            max_tool_calls: None,
            max_failed_tools: Some(0),
            notes: None,
        };
        let result = RunResult {
            final_message: "done".to_string(),
            tool_events: vec![crate::core::loop_runtime::ToolEvent {
                tool_name: "run_shell".to_string(),
                input: std::collections::BTreeMap::new(),
                output: "meta.result=ok\nexit_code: 0".to_string(),
                status: crate::model::protocol::ObservationStatus::Ok,
            }],
            usage: crate::model::protocol::TokenUsage::default(),
        };
        let evaluation = case.evaluate(&result);
        assert!(evaluation.passed);
    }

    #[test]
    fn benchmark_evaluation_uses_raw_tool_output_not_trimmed_observation_summary() {
        let case = BenchmarkCase {
            name: "validate".to_string(),
            task: "validate changes".to_string(),
            category: "write_validate".to_string(),
            skill: None,
            workdir: None,
            isolate_workdir: false,
            budget: 4,
            seed_observations: Vec::new(),
            expect_tool: Some("run_shell".to_string()),
            expect_tool_sequence: Some(vec!["run_shell".to_string()]),
            forbid_tool: None,
            expect_message_contains: None,
            expect_tool_output_contains: None,
            expect_last_tool_output_contains: Some("meta.result=ok".to_string()),
            min_tool_calls: None,
            max_tool_calls: None,
            max_failed_tools: Some(0),
            notes: None,
        };
        let long_stdout = (1..=80)
            .map(|n| format!("line{n}"))
            .collect::<Vec<_>>()
            .join("\n");
        let result = RunResult {
            final_message: "done".to_string(),
            tool_events: vec![crate::core::loop_runtime::ToolEvent {
                tool_name: "run_shell".to_string(),
                input: std::collections::BTreeMap::new(),
                output: format!("meta.result=ok\nexit_code: 0\nstdout:\n{long_stdout}"),
                status: crate::model::protocol::ObservationStatus::Ok,
            }],
            usage: crate::model::protocol::TokenUsage::default(),
        };
        let evaluation = case.evaluate(&result);
        assert!(evaluation.passed);
    }

    #[test]
    fn benchmark_evaluation_accepts_named_tool_output_expectation() {
        let case = BenchmarkCase {
            name: "recover".to_string(),
            task: "recover after failed validation".to_string(),
            category: "write_validate".to_string(),
            skill: None,
            workdir: None,
            isolate_workdir: false,
            budget: 6,
            seed_observations: Vec::new(),
            expect_tool: Some("run_shell".to_string()),
            expect_tool_sequence: Some(vec!["run_shell".to_string(), "read_file".to_string()]),
            forbid_tool: None,
            expect_message_contains: None,
            expect_tool_output_contains: Some(ToolOutputExpectation {
                tool_name: "run_shell".to_string(),
                needle: "meta.failure_kind=test_failure".to_string(),
            }),
            expect_last_tool_output_contains: Some("a * b".to_string()),
            min_tool_calls: None,
            max_tool_calls: None,
            max_failed_tools: Some(0),
            notes: None,
        };
        let result = RunResult {
            final_message: "done".to_string(),
            tool_events: vec![
                crate::core::loop_runtime::ToolEvent {
                    tool_name: "run_shell".to_string(),
                    input: std::collections::BTreeMap::new(),
                    output: "meta.result=failed\nmeta.failure_kind=test_failure".to_string(),
                    status: crate::model::protocol::ObservationStatus::Ok,
                },
                crate::core::loop_runtime::ToolEvent {
                    tool_name: "read_file".to_string(),
                    input: std::collections::BTreeMap::new(),
                    output: "2     a * b".to_string(),
                    status: crate::model::protocol::ObservationStatus::Ok,
                },
            ],
            usage: crate::model::protocol::TokenUsage::default(),
        };
        let evaluation = case.evaluate(&result);
        assert!(evaluation.passed);
    }

    #[test]
    fn parse_tool_output_expectation_rejects_invalid_shape() {
        let error = parse_tool_output_expectation("run_shell").unwrap_err();
        assert!(error.to_string().contains("expected `tool_name:needle`"));
    }

    #[test]
    fn evaluate_trend_gate_skips_when_history_is_too_short() {
        let current = BenchmarkRunRecord {
            version: 2,
            timestamp_secs: 200,
            manifest: ".dscode/benchmarks.txt".to_string(),
            cases: 20,
            passed: 20,
            total_tool_calls: 73,
            total_failed_tool_calls: 0,
            duration_ms: 1000,
            category_stats: Vec::new(),
            dogfood_snapshot: None,
        };
        let previous = vec![BenchmarkRunRecord {
            version: 2,
            timestamp_secs: 199,
            manifest: ".dscode/benchmarks.txt".to_string(),
            cases: 20,
            passed: 20,
            total_tool_calls: 73,
            total_failed_tool_calls: 0,
            duration_ms: 950,
            category_stats: Vec::new(),
            dogfood_snapshot: None,
        }];

        let gate = evaluate_trend_gate(&current, &previous);
        assert_eq!(gate.status, TrendGateStatus::InsufficientHistory);
        assert!(!gate.failed());
    }

    #[test]
    fn evaluate_trend_gate_fails_on_pass_regression() {
        let current = BenchmarkRunRecord {
            version: 2,
            timestamp_secs: 300,
            manifest: ".dscode/benchmarks.txt".to_string(),
            cases: 20,
            passed: 19,
            total_tool_calls: 73,
            total_failed_tool_calls: 0,
            duration_ms: 1000,
            category_stats: Vec::new(),
            dogfood_snapshot: None,
        };
        let previous = vec![
            BenchmarkRunRecord {
                version: 2,
                timestamp_secs: 299,
                manifest: ".dscode/benchmarks.txt".to_string(),
                cases: 20,
                passed: 20,
                total_tool_calls: 73,
                total_failed_tool_calls: 0,
                duration_ms: 950,
                category_stats: Vec::new(),
                dogfood_snapshot: None,
            },
            BenchmarkRunRecord {
                version: 2,
                timestamp_secs: 298,
                manifest: ".dscode/benchmarks.txt".to_string(),
                cases: 20,
                passed: 20,
                total_tool_calls: 72,
                total_failed_tool_calls: 0,
                duration_ms: 948,
                category_stats: Vec::new(),
                dogfood_snapshot: None,
            },
            BenchmarkRunRecord {
                version: 2,
                timestamp_secs: 297,
                manifest: ".dscode/benchmarks.txt".to_string(),
                cases: 20,
                passed: 20,
                total_tool_calls: 74,
                total_failed_tool_calls: 0,
                duration_ms: 945,
                category_stats: Vec::new(),
                dogfood_snapshot: None,
            },
        ];

        let gate = evaluate_trend_gate(&current, &previous);
        assert_eq!(gate.status, TrendGateStatus::Failed);
        assert!(gate
            .summary_line()
            .contains("passed 19 below comparable best 20"));
    }

    #[test]
    fn evaluate_trend_gate_fails_on_tool_call_regression() {
        let current = BenchmarkRunRecord {
            version: 2,
            timestamp_secs: 400,
            manifest: ".dscode/benchmarks.txt".to_string(),
            cases: 20,
            passed: 20,
            total_tool_calls: 90,
            total_failed_tool_calls: 0,
            duration_ms: 1000,
            category_stats: Vec::new(),
            dogfood_snapshot: None,
        };
        let previous = vec![
            BenchmarkRunRecord {
                version: 2,
                timestamp_secs: 399,
                manifest: ".dscode/benchmarks.txt".to_string(),
                cases: 20,
                passed: 20,
                total_tool_calls: 73,
                total_failed_tool_calls: 0,
                duration_ms: 950,
                category_stats: Vec::new(),
                dogfood_snapshot: None,
            },
            BenchmarkRunRecord {
                version: 2,
                timestamp_secs: 398,
                manifest: ".dscode/benchmarks.txt".to_string(),
                cases: 20,
                passed: 20,
                total_tool_calls: 72,
                total_failed_tool_calls: 0,
                duration_ms: 948,
                category_stats: Vec::new(),
                dogfood_snapshot: None,
            },
            BenchmarkRunRecord {
                version: 2,
                timestamp_secs: 397,
                manifest: ".dscode/benchmarks.txt".to_string(),
                cases: 20,
                passed: 20,
                total_tool_calls: 74,
                total_failed_tool_calls: 0,
                duration_ms: 945,
                category_stats: Vec::new(),
                dogfood_snapshot: None,
            },
        ];

        let gate = evaluate_trend_gate(&current, &previous);
        assert_eq!(gate.status, TrendGateStatus::Failed);
        assert!(gate
            .summary_line()
            .contains("tool calls 90 above comparable median 73 + tolerance 8"));
    }

    #[test]
    fn evaluate_trend_gate_fails_on_category_regression() {
        let current = BenchmarkRunRecord {
            version: 2,
            timestamp_secs: 500,
            manifest: ".dscode/benchmarks.txt".to_string(),
            cases: 20,
            passed: 20,
            total_tool_calls: 73,
            total_failed_tool_calls: 0,
            duration_ms: 1000,
            category_stats: vec![BenchmarkCategoryStats {
                category: "write_validate".to_string(),
                cases: 3,
                passed: 3,
                total_tool_calls: 20,
                total_failed_tool_calls: 0,
            }],
            dogfood_snapshot: None,
        };
        let previous = vec![
            BenchmarkRunRecord {
                version: 2,
                timestamp_secs: 499,
                manifest: ".dscode/benchmarks.txt".to_string(),
                cases: 20,
                passed: 20,
                total_tool_calls: 73,
                total_failed_tool_calls: 0,
                duration_ms: 950,
                category_stats: vec![BenchmarkCategoryStats {
                    category: "write_validate".to_string(),
                    cases: 3,
                    passed: 3,
                    total_tool_calls: 12,
                    total_failed_tool_calls: 0,
                }],
                dogfood_snapshot: None,
            },
            BenchmarkRunRecord {
                version: 2,
                timestamp_secs: 498,
                manifest: ".dscode/benchmarks.txt".to_string(),
                cases: 20,
                passed: 20,
                total_tool_calls: 72,
                total_failed_tool_calls: 0,
                duration_ms: 948,
                category_stats: vec![BenchmarkCategoryStats {
                    category: "write_validate".to_string(),
                    cases: 3,
                    passed: 3,
                    total_tool_calls: 11,
                    total_failed_tool_calls: 0,
                }],
                dogfood_snapshot: None,
            },
            BenchmarkRunRecord {
                version: 2,
                timestamp_secs: 497,
                manifest: ".dscode/benchmarks.txt".to_string(),
                cases: 20,
                passed: 20,
                total_tool_calls: 74,
                total_failed_tool_calls: 0,
                duration_ms: 945,
                category_stats: vec![BenchmarkCategoryStats {
                    category: "write_validate".to_string(),
                    cases: 3,
                    passed: 3,
                    total_tool_calls: 13,
                    total_failed_tool_calls: 0,
                }],
                dogfood_snapshot: None,
            },
        ];

        let gate = evaluate_trend_gate(&current, &previous);
        assert_eq!(gate.status, TrendGateStatus::Failed);
        assert!(gate.summary_line().contains(
            "category `write_validate`: tool calls 20 above comparable median 12 + tolerance 3"
        ));
    }

    #[test]
    fn evaluate_live_gate_fails_on_new_dogfood_failure() {
        let previous = BenchmarkRunRecord {
            version: 3,
            timestamp_secs: 1,
            manifest: ".dscode/benchmarks.txt".to_string(),
            cases: 1,
            passed: 1,
            total_tool_calls: 1,
            total_failed_tool_calls: 0,
            duration_ms: 10,
            category_stats: Vec::new(),
            dogfood_snapshot: Some(DogfoodSnapshot {
                runs: 3,
                success: 3,
                failed: 0,
                stuck: 0,
                manual: 0,
                category_stats: vec![DogfoodCategorySnapshot {
                    category: "pr_workflow".to_string(),
                    runs: 3,
                    success: 3,
                    failed: 0,
                    stuck: 0,
                    manual: 0,
                    total_tool_calls: 9,
                }],
            }),
        };
        let current = BenchmarkRunRecord {
            timestamp_secs: 2,
            dogfood_snapshot: Some(DogfoodSnapshot {
                runs: 4,
                success: 3,
                failed: 1,
                stuck: 0,
                manual: 0,
                category_stats: vec![DogfoodCategorySnapshot {
                    category: "pr_workflow".to_string(),
                    runs: 4,
                    success: 3,
                    failed: 1,
                    stuck: 0,
                    manual: 0,
                    total_tool_calls: 13,
                }],
            }),
            ..previous.clone()
        };

        let gate = evaluate_live_gate(&current, &[previous]);
        assert_eq!(gate.status, TrendGateStatus::Failed);
        assert!(gate
            .summary_line()
            .contains("failed dogfood records increased 0 -> 1"));
        assert!(gate
            .summary_line()
            .contains("category `pr_workflow` failed records increased 0 -> 1"));
    }

    #[test]
    fn evaluate_live_gate_passes_when_snapshot_has_no_new_failures() {
        let snapshot = DogfoodSnapshot {
            runs: 5,
            success: 4,
            failed: 1,
            stuck: 0,
            manual: 0,
            category_stats: vec![DogfoodCategorySnapshot {
                category: "recovery".to_string(),
                runs: 5,
                success: 4,
                failed: 1,
                stuck: 0,
                manual: 0,
                total_tool_calls: 14,
            }],
        };
        let previous = BenchmarkRunRecord {
            version: 3,
            timestamp_secs: 1,
            manifest: ".dscode/benchmarks.txt".to_string(),
            cases: 1,
            passed: 1,
            total_tool_calls: 1,
            total_failed_tool_calls: 0,
            duration_ms: 10,
            category_stats: Vec::new(),
            dogfood_snapshot: Some(snapshot),
        };
        let current = BenchmarkRunRecord {
            timestamp_secs: 2,
            ..previous.clone()
        };

        let gate = evaluate_live_gate(&current, &[previous]);
        assert_eq!(gate.status, TrendGateStatus::Passed);
        assert!(gate.summary_line().contains("no new dogfood records"));
    }

    #[test]
    fn evaluate_live_gate_ignores_category_recategorization_without_new_runs() {
        let previous = BenchmarkRunRecord {
            version: 3,
            timestamp_secs: 1,
            manifest: ".dscode/benchmarks.txt".to_string(),
            cases: 1,
            passed: 1,
            total_tool_calls: 1,
            total_failed_tool_calls: 0,
            duration_ms: 10,
            category_stats: Vec::new(),
            dogfood_snapshot: Some(DogfoodSnapshot {
                runs: 5,
                success: 4,
                failed: 1,
                stuck: 0,
                manual: 0,
                category_stats: vec![DogfoodCategorySnapshot {
                    category: "write_validate".to_string(),
                    runs: 5,
                    success: 4,
                    failed: 1,
                    stuck: 0,
                    manual: 0,
                    total_tool_calls: 20,
                }],
            }),
        };
        let current = BenchmarkRunRecord {
            timestamp_secs: 2,
            dogfood_snapshot: Some(DogfoodSnapshot {
                runs: 5,
                success: 4,
                failed: 1,
                stuck: 0,
                manual: 0,
                category_stats: vec![DogfoodCategorySnapshot {
                    category: "recovery".to_string(),
                    runs: 5,
                    success: 4,
                    failed: 1,
                    stuck: 0,
                    manual: 0,
                    total_tool_calls: 20,
                }],
            }),
            ..previous.clone()
        };

        let gate = evaluate_live_gate(&current, &[previous]);
        assert_eq!(gate.status, TrendGateStatus::Passed);
        assert!(gate.summary_line().contains("no new dogfood records"));
    }

    #[test]
    fn evaluate_trend_gate_warms_up_category_history_from_v1_runs() {
        let current = BenchmarkRunRecord {
            version: 2,
            timestamp_secs: 600,
            manifest: ".dscode/benchmarks.txt".to_string(),
            cases: 20,
            passed: 20,
            total_tool_calls: 73,
            total_failed_tool_calls: 0,
            duration_ms: 1000,
            category_stats: vec![BenchmarkCategoryStats {
                category: "write_validate".to_string(),
                cases: 3,
                passed: 3,
                total_tool_calls: 14,
                total_failed_tool_calls: 0,
            }],
            dogfood_snapshot: None,
        };
        let previous = vec![
            BenchmarkRunRecord {
                version: 1,
                timestamp_secs: 596,
                manifest: ".dscode/benchmarks.txt".to_string(),
                cases: 20,
                passed: 20,
                total_tool_calls: 72,
                total_failed_tool_calls: 0,
                duration_ms: 940,
                category_stats: Vec::new(),
                dogfood_snapshot: None,
            },
            BenchmarkRunRecord {
                version: 1,
                timestamp_secs: 597,
                manifest: ".dscode/benchmarks.txt".to_string(),
                cases: 20,
                passed: 20,
                total_tool_calls: 74,
                total_failed_tool_calls: 0,
                duration_ms: 941,
                category_stats: Vec::new(),
                dogfood_snapshot: None,
            },
            BenchmarkRunRecord {
                version: 2,
                timestamp_secs: 598,
                manifest: ".dscode/benchmarks.txt".to_string(),
                cases: 20,
                passed: 20,
                total_tool_calls: 73,
                total_failed_tool_calls: 0,
                duration_ms: 942,
                category_stats: vec![BenchmarkCategoryStats {
                    category: "write_validate".to_string(),
                    cases: 3,
                    passed: 3,
                    total_tool_calls: 12,
                    total_failed_tool_calls: 0,
                }],
                dogfood_snapshot: None,
            },
        ];

        let gate = evaluate_trend_gate(&current, &previous);
        let summary = gate
            .category_summaries
            .iter()
            .find(|summary| summary.category == "write_validate")
            .expect("write_validate summary");
        assert_eq!(summary.status, TrendGateStatus::Passed);
        assert_eq!(summary.comparable_runs, 3);
        assert_eq!(summary.best_passed, Some(3));
        assert_eq!(summary.median_tool_calls, Some(12));
    }

    #[test]
    fn evaluate_trend_gate_warmup_still_fails_category_regression() {
        let current = BenchmarkRunRecord {
            version: 2,
            timestamp_secs: 700,
            manifest: ".dscode/benchmarks.txt".to_string(),
            cases: 20,
            passed: 20,
            total_tool_calls: 73,
            total_failed_tool_calls: 0,
            duration_ms: 1000,
            category_stats: vec![BenchmarkCategoryStats {
                category: "write_validate".to_string(),
                cases: 3,
                passed: 3,
                total_tool_calls: 20,
                total_failed_tool_calls: 0,
            }],
            dogfood_snapshot: None,
        };
        let previous = vec![
            BenchmarkRunRecord {
                version: 1,
                timestamp_secs: 696,
                manifest: ".dscode/benchmarks.txt".to_string(),
                cases: 20,
                passed: 20,
                total_tool_calls: 72,
                total_failed_tool_calls: 0,
                duration_ms: 940,
                category_stats: Vec::new(),
                dogfood_snapshot: None,
            },
            BenchmarkRunRecord {
                version: 1,
                timestamp_secs: 697,
                manifest: ".dscode/benchmarks.txt".to_string(),
                cases: 20,
                passed: 20,
                total_tool_calls: 74,
                total_failed_tool_calls: 0,
                duration_ms: 941,
                category_stats: Vec::new(),
                dogfood_snapshot: None,
            },
            BenchmarkRunRecord {
                version: 2,
                timestamp_secs: 698,
                manifest: ".dscode/benchmarks.txt".to_string(),
                cases: 20,
                passed: 20,
                total_tool_calls: 73,
                total_failed_tool_calls: 0,
                duration_ms: 942,
                category_stats: vec![BenchmarkCategoryStats {
                    category: "write_validate".to_string(),
                    cases: 3,
                    passed: 3,
                    total_tool_calls: 12,
                    total_failed_tool_calls: 0,
                }],
                dogfood_snapshot: None,
            },
        ];

        let gate = evaluate_trend_gate(&current, &previous);
        let summary = gate
            .category_summaries
            .iter()
            .find(|summary| summary.category == "write_validate")
            .expect("write_validate summary");
        assert_eq!(summary.status, TrendGateStatus::Failed);
        assert_eq!(summary.comparable_runs, 3);
        assert!(summary.reasons.iter().any(
            |reason| reason.contains("tool calls 20 above comparable median 12 + tolerance 3")
        ));
    }

    #[test]
    fn evaluate_trend_gate_warmup_requires_actual_category_baseline() {
        let current = BenchmarkRunRecord {
            version: 2,
            timestamp_secs: 800,
            manifest: ".dscode/benchmarks.txt".to_string(),
            cases: 20,
            passed: 20,
            total_tool_calls: 73,
            total_failed_tool_calls: 0,
            duration_ms: 1000,
            category_stats: vec![BenchmarkCategoryStats {
                category: "write_validate".to_string(),
                cases: 3,
                passed: 3,
                total_tool_calls: 14,
                total_failed_tool_calls: 0,
            }],
            dogfood_snapshot: None,
        };
        let previous = vec![
            BenchmarkRunRecord {
                version: 1,
                timestamp_secs: 796,
                manifest: ".dscode/benchmarks.txt".to_string(),
                cases: 20,
                passed: 20,
                total_tool_calls: 72,
                total_failed_tool_calls: 0,
                duration_ms: 940,
                category_stats: Vec::new(),
                dogfood_snapshot: None,
            },
            BenchmarkRunRecord {
                version: 1,
                timestamp_secs: 797,
                manifest: ".dscode/benchmarks.txt".to_string(),
                cases: 20,
                passed: 20,
                total_tool_calls: 74,
                total_failed_tool_calls: 0,
                duration_ms: 941,
                category_stats: Vec::new(),
                dogfood_snapshot: None,
            },
            BenchmarkRunRecord {
                version: 1,
                timestamp_secs: 798,
                manifest: ".dscode/benchmarks.txt".to_string(),
                cases: 20,
                passed: 20,
                total_tool_calls: 73,
                total_failed_tool_calls: 0,
                duration_ms: 942,
                category_stats: Vec::new(),
                dogfood_snapshot: None,
            },
        ];

        let gate = evaluate_trend_gate(&current, &previous);
        let summary = gate
            .category_summaries
            .iter()
            .find(|summary| summary.category == "write_validate")
            .expect("write_validate summary");
        assert_eq!(summary.status, TrendGateStatus::InsufficientHistory);
        assert_eq!(summary.comparable_runs, 0);
    }

    #[test]
    fn load_dogfood_snapshot_counts_manual_interventions_without_double_counting_manual_outcome() {
        let root = std::env::temp_dir().join(format!(
            "deepseek-bench-dogfood-{}-{}",
            std::process::id(),
            next_temp_suffix()
        ));
        fs::create_dir_all(&root).unwrap();
        let ledger = root.join("ledger.jsonl");
        fs::write(
            &ledger,
            concat!(
                "{\"task\":\"inspect layout\",\"tool_trace\":\"todo_write -> read_file\",\"outcome\":\"success\",\"manual_intervention\":false,\"benchmark_category\":\"read_only\",\"failed_tool_calls\":0,\"repeated_call_failures\":0,\"used_subagent\":false,\"tool_calls\":2}\n",
                "{\"task\":\"recover from search miss\",\"tool_trace\":\"search_text -> list_files\",\"outcome\":\"failed\",\"manual_intervention\":true,\"benchmark_category\":\"recovery\",\"failed_tool_calls\":1,\"repeated_call_failures\":0,\"used_subagent\":false,\"tool_calls\":3}\n",
                "{\"task\":\"manual follow-up after failure\",\"tool_trace\":\"search_text\",\"outcome\":\"manual\",\"manual_intervention\":true,\"benchmark_category\":\"recovery\",\"failed_tool_calls\":1,\"repeated_call_failures\":0,\"used_subagent\":false,\"tool_calls\":1}\n"
            ),
        )
        .unwrap();
        let snapshot = load_dogfood_snapshot(&ledger).unwrap().unwrap();
        assert_eq!(snapshot.runs, 3);
        assert_eq!(snapshot.success, 1);
        assert_eq!(snapshot.failed, 1);
        assert_eq!(snapshot.manual, 2);
        assert_eq!(snapshot.category_stats.len(), 2);
        assert_eq!(snapshot.category_stats[0].category, "read_only");
        assert_eq!(snapshot.category_stats[1].category, "recovery");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn evaluate_trend_gate_skips_category_runs_with_different_case_counts() {
        let current = BenchmarkRunRecord {
            version: 2,
            timestamp_secs: 900,
            manifest: ".dscode/benchmarks.txt".to_string(),
            cases: 26,
            passed: 26,
            total_tool_calls: 94,
            total_failed_tool_calls: 0,
            duration_ms: 1000,
            category_stats: vec![BenchmarkCategoryStats {
                category: "recovery".to_string(),
                cases: 8,
                passed: 8,
                total_tool_calls: 26,
                total_failed_tool_calls: 0,
            }],
            dogfood_snapshot: None,
        };
        let previous = vec![
            BenchmarkRunRecord {
                version: 2,
                timestamp_secs: 899,
                manifest: ".dscode/benchmarks.txt".to_string(),
                cases: 26,
                passed: 25,
                total_tool_calls: 94,
                total_failed_tool_calls: 0,
                duration_ms: 950,
                category_stats: vec![BenchmarkCategoryStats {
                    category: "recovery".to_string(),
                    cases: 8,
                    passed: 7,
                    total_tool_calls: 26,
                    total_failed_tool_calls: 0,
                }],
                dogfood_snapshot: None,
            },
            BenchmarkRunRecord {
                version: 2,
                timestamp_secs: 898,
                manifest: ".dscode/benchmarks.txt".to_string(),
                cases: 26,
                passed: 25,
                total_tool_calls: 94,
                total_failed_tool_calls: 0,
                duration_ms: 948,
                category_stats: vec![BenchmarkCategoryStats {
                    category: "recovery".to_string(),
                    cases: 7,
                    passed: 7,
                    total_tool_calls: 22,
                    total_failed_tool_calls: 0,
                }],
                dogfood_snapshot: None,
            },
            BenchmarkRunRecord {
                version: 2,
                timestamp_secs: 897,
                manifest: ".dscode/benchmarks.txt".to_string(),
                cases: 26,
                passed: 25,
                total_tool_calls: 94,
                total_failed_tool_calls: 0,
                duration_ms: 946,
                category_stats: vec![BenchmarkCategoryStats {
                    category: "recovery".to_string(),
                    cases: 7,
                    passed: 7,
                    total_tool_calls: 22,
                    total_failed_tool_calls: 0,
                }],
                dogfood_snapshot: None,
            },
            BenchmarkRunRecord {
                version: 2,
                timestamp_secs: 896,
                manifest: ".dscode/benchmarks.txt".to_string(),
                cases: 26,
                passed: 25,
                total_tool_calls: 94,
                total_failed_tool_calls: 0,
                duration_ms: 944,
                category_stats: vec![BenchmarkCategoryStats {
                    category: "recovery".to_string(),
                    cases: 7,
                    passed: 7,
                    total_tool_calls: 22,
                    total_failed_tool_calls: 0,
                }],
                dogfood_snapshot: None,
            },
        ];

        let gate = evaluate_trend_gate(&current, &previous);
        let summary = gate
            .category_summaries
            .iter()
            .find(|summary| summary.category == "recovery")
            .expect("recovery summary");
        assert_eq!(summary.status, TrendGateStatus::InsufficientHistory);
        assert_eq!(summary.comparable_runs, 1);
    }
}
