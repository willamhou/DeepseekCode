use std::env;

#[derive(Debug)]
pub struct Cli {
    pub command: Option<Command>,
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
    ReplayBenchmark(DogfoodReplayArgs),
    Report(DogfoodReportArgs),
    ExportBenchmark(DogfoodExportArgs),
    PromoteBenchmark(DogfoodPromoteArgs),
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

#[derive(Debug, Default)]
pub struct DogfoodReplayArgs {
    pub manifest: Option<String>,
    pub category: Option<String>,
    pub limit: Option<usize>,
    pub benchmark_gate: bool,
}

#[derive(Debug, Default)]
pub struct DogfoodReportArgs {
    pub out: Option<String>,
    pub limit: Option<usize>,
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
        .ok_or_else(|| "pr requires a sub-action: review|fix|patch".to_string())?;
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
            "unknown pr sub-action `{other}`; expected review|fix|patch"
        )),
    }
}

impl Cli {
    pub fn parse() -> Result<Self, String> {
        let argv = env::args().skip(1).collect::<Vec<_>>();
        Self::from_argv(argv)
    }

    pub fn from_argv(mut args: Vec<String>) -> Result<Self, String> {
        if args.is_empty() {
            return Ok(Self {
                command: Some(Command::Chat(ChatArgs::default())),
            });
        }

        if args.len() == 1 && matches!(args[0].as_str(), "--version" | "-V") {
            return Ok(Self {
                command: Some(Command::Version),
            });
        }

        let first = args.remove(0);
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
            "diff" => Command::Diff(DiffArgs {}),
            "resume" => Command::Resume(ResumeArgs { session: None }),
            "config" => Command::Config(parse_config_args(args)?),
            "doctor" => Command::Doctor(DoctorArgs {}),
            "smoke" => Command::Smoke(parse_smoke_args(args)),
            "pr" => Command::Pr(parse_pr_subcommand(args)?),
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
    Diff(DiffArgs),
    Resume(ResumeArgs),
    Config(ConfigArgs),
    Doctor(DoctorArgs),
    Smoke(SmokeArgs),
    Pr(PrAction),
    Version,
}

impl Default for Command {
    fn default() -> Self {
        Self::Chat(ChatArgs::default())
    }
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
}

#[derive(Debug)]
pub struct DoctorArgs {}

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

fn parse_config_args(args: Vec<String>) -> Result<ConfigArgs, String> {
    let mut parsed = ConfigArgs {
        print_default: false,
        init: false,
        force: false,
    };

    for arg in args {
        match arg.as_str() {
            "--print-default" => parsed.print_default = true,
            "init" => parsed.init = true,
            "--force" | "-f" => parsed.force = true,
            other => {
                return Err(format!(
                    "unknown config argument `{other}`; expected init|--force|--print-default"
                ));
            }
        }
    }

    if parsed.print_default && parsed.init {
        return Err("config init cannot be combined with --print-default".to_string());
    }
    if parsed.force && !parsed.init {
        return Err("config --force requires init".to_string());
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

fn parse_dogfood_subcommand(args: Vec<String>) -> Result<DogfoodAction, String> {
    let mut iter = args.into_iter();
    let action = iter
        .next()
        .ok_or_else(|| {
            "dogfood requires a sub-action: run|replay-benchmark|report|export-benchmark|promote-benchmark"
                .to_string()
        })?;
    let rest: Vec<String> = iter.collect();
    match action.as_str() {
        "run" => parse_dogfood_run_args(rest).map(DogfoodAction::Run),
        "replay-benchmark" | "replay-bench" => {
            Ok(DogfoodAction::ReplayBenchmark(parse_dogfood_replay_args(rest)))
        }
        "report" => Ok(DogfoodAction::Report(parse_dogfood_report_args(rest))),
        "export-benchmark" | "export-bench" => {
            Ok(DogfoodAction::ExportBenchmark(parse_dogfood_export_args(rest)))
        }
        "promote-benchmark" | "promote-bench" => {
            Ok(DogfoodAction::PromoteBenchmark(parse_dogfood_promote_args(
                rest,
            )))
        }
        other => Err(format!(
            "unknown dogfood sub-action `{other}`; expected run|replay-benchmark|report|export-benchmark|promote-benchmark"
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

fn parse_dogfood_report_args(args: Vec<String>) -> DogfoodReportArgs {
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
            _ => {}
        }
        index += 1;
    }

    report
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
            DogfoodAction::ReplayBenchmark(_) => panic!("expected dogfood run args"),
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
    fn parses_dogfood_report_subcommand() {
        let parsed = parse_dogfood_subcommand(vec![
            "report".to_string(),
            "--out".to_string(),
            "dogfood.md".to_string(),
            "--limit".to_string(),
            "50".to_string(),
        ])
        .unwrap();

        match parsed {
            DogfoodAction::Report(args) => {
                assert_eq!(args.out.as_deref(), Some("dogfood.md"));
                assert_eq!(args.limit, Some(50));
            }
            DogfoodAction::Run(_) => panic!("expected dogfood report args"),
            DogfoodAction::ReplayBenchmark(_) => panic!("expected dogfood report args"),
            DogfoodAction::ExportBenchmark(_) => panic!("expected dogfood report args"),
            DogfoodAction::PromoteBenchmark(_) => panic!("expected dogfood report args"),
        }
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
            DogfoodAction::Report(_) => panic!("expected replay args"),
            DogfoodAction::ExportBenchmark(_) => panic!("expected replay args"),
            DogfoodAction::PromoteBenchmark(_) => panic!("expected replay args"),
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
