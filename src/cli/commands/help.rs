use crate::cli::app::HelpArgs;
use crate::error::AppResult;

pub fn run(args: HelpArgs) -> AppResult<()> {
    println!("{}", render_help(&args.topics));
    Ok(())
}

fn render_help(topics: &[String]) -> String {
    match topics.first().map(String::as_str) {
        None => global_help().to_string(),
        Some("dogfood") => dogfood_help(topics.get(1).map(String::as_str)).to_string(),
        Some("tui") => tui_help().to_string(),
        Some("run") => run_help().to_string(),
        Some("exec") => exec_help().to_string(),
        Some("help") => global_help().to_string(),
        Some(other) => format!(
            "{}\n\nUnknown help topic `{}`. Use `deepseek --help` for the command list.",
            global_help(),
            other
        ),
    }
}

fn global_help() -> &'static str {
    concat!(
        "DeepSeekCode\n",
        "\n",
        "Usage:\n",
        "  deepseek                         Start the interactive coding agent REPL (requires a TTY)\n",
        "  deepseek tui                     Start the full-screen terminal workbench\n",
        "  deepseek run \"<task>\"             Run one coding task and exit\n",
        "  deepseek exec run \"<task>\"        Run a durable one-shot agent task\n",
        "  deepseek dogfood <action>        Run self-verification and release evidence commands\n",
        "  deepseek help [topic]            Show command help\n",
        "  deepseek --version               Show version\n",
        "\n",
        "Common commands:\n",
        "  chat, repl, interactive          Explicit aliases for the interactive REPL\n",
        "  tui                              Terminal workbench with sessions, tools, and approvals\n",
        "  run                              One-shot coding task\n",
        "  exec                             Durable exec/resume task runner\n",
        "  agents                           Durable runtime, service, and shell supervisor tools\n",
        "  mcp                              MCP client/server configuration tools\n",
        "  pr                               GitHub PR review/fix/patch helpers\n",
        "  dogfood                          Project self-test and release evidence workflow\n",
        "\n",
        "Examples:\n",
        "  deepseek\n",
        "  deepseek tui\n",
        "  deepseek run \"fix the failing tests and summarize the diff\"\n",
        "  deepseek dogfood live-plan --limit 10\n",
        "\n",
        "More help:\n",
        "  deepseek help tui\n",
        "  deepseek help run\n",
        "  deepseek help dogfood\n",
        "  deepseek help dogfood replay-benchmark"
    )
}

fn tui_help() -> &'static str {
    concat!(
        "DeepSeekCode TUI\n",
        "\n",
        "Usage:\n",
        "  deepseek tui [--demo] [--once] [--runtime-url <url>]\n",
        "\n",
        "Options:\n",
        "  --demo                 Render deterministic demo state instead of local runtime state\n",
        "  --once                 Render one snapshot and exit; useful for CI and README captures\n",
        "  --runtime-url <url>    Connect to a running DeepSeekCode HTTP runtime"
    )
}

fn run_help() -> &'static str {
    concat!(
        "DeepSeekCode run\n",
        "\n",
        "Usage:\n",
        "  deepseek run [--skill <name>] [--budget <1..200>] [--benchmark-gate] \"<task>\"\n",
        "\n",
        "Runs one coding-agent task and exits. Use bare `deepseek` for the interactive\n",
        "terminal REPL or `deepseek tui` for the full-screen workbench."
    )
}

fn exec_help() -> &'static str {
    concat!(
        "DeepSeekCode exec\n",
        "\n",
        "Usage:\n",
        "  deepseek exec run [--skill <name>] [--budget <1..200>] [--image <path>] [--json] \"<task>\"\n",
        "  deepseek exec resume [session-id] [--skill <name>] [--budget <1..200>] [--image <path>] [--json] [task]\n",
        "\n",
        "Runs or resumes durable coding-agent tasks with structured output support."
    )
}

fn dogfood_help(topic: Option<&str>) -> &'static str {
    match topic {
        Some("run") => dogfood_run_help(),
        Some("external-fixture") | Some("external-write-fixture") => {
            dogfood_external_fixture_help()
        }
        Some("replay-benchmark") | Some("replay-bench") => dogfood_replay_help(),
        Some("live-plan") | Some("plan-live") => dogfood_live_plan_help(),
        Some("report") => dogfood_report_help(),
        Some("export-benchmark") | Some("export-bench") => dogfood_export_help(),
        Some("promote-benchmark") | Some("promote-bench") => dogfood_promote_help(),
        _ => concat!(
            "DeepSeekCode dogfood\n",
            "\n",
            "Usage:\n",
            "  deepseek dogfood run \"<task>\"\n",
            "  deepseek dogfood run --from-benchmark <case> [--manifest <path>]\n",
            "  deepseek dogfood external-fixture --workdir <path> \"<task>\"\n",
            "  deepseek dogfood replay-benchmark [--manifest <path>] [--category <name>] [--limit <n>]\n",
            "  deepseek dogfood live-plan [--limit <n>] [--json]\n",
            "  deepseek dogfood report [requirements]\n",
            "  deepseek dogfood export-benchmark [--out <path>]\n",
            "  deepseek dogfood promote-benchmark [--dry-run]\n",
            "\n",
            "Dogfood commands are for self-verification, benchmark evidence, and release\n",
            "gates. Normal product use is `deepseek`, `deepseek tui`, or `deepseek run`.\n",
            "\n",
            "More help:\n",
            "  deepseek help dogfood replay-benchmark\n",
            "  deepseek help dogfood live-plan\n",
            "  deepseek help dogfood report"
        ),
    }
}

fn dogfood_run_help() -> &'static str {
    concat!(
        "DeepSeekCode dogfood run\n",
        "\n",
        "Usage:\n",
        "  deepseek dogfood run [--skill <name>] [--budget <1..200>] [--workdir <path>] [--isolate-workdir] [--benchmark-gate] [--notes <text>] \"<task>\"\n",
        "  deepseek dogfood run --from-benchmark <case> [--manifest <path>] [--budget <1..200>] [--benchmark-gate] [--notes <text>]\n",
        "\n",
        "Runs a coding-agent task and records the outcome in the dogfood ledger."
    )
}

fn dogfood_external_fixture_help() -> &'static str {
    concat!(
        "DeepSeekCode dogfood external-fixture\n",
        "\n",
        "Usage:\n",
        "  deepseek dogfood external-fixture --workdir <path> [--budget <1..200>] [--benchmark-gate] [--dry-run] [--notes <text>] \"<task>\"\n",
        "\n",
        "Runs an isolated write fixture from an external repository workdir."
    )
}

fn dogfood_replay_help() -> &'static str {
    concat!(
        "DeepSeekCode dogfood replay-benchmark\n",
        "\n",
        "Usage:\n",
        "  deepseek dogfood replay-benchmark [--manifest <path>] [--category <name>] [--limit <1..200>] [--benchmark-gate]\n",
        "\n",
        "Replays selected benchmark cases through dogfood recording. This can make real\n",
        "model calls when the configured provider is online."
    )
}

fn dogfood_live_plan_help() -> &'static str {
    concat!(
        "DeepSeekCode dogfood live-plan\n",
        "\n",
        "Usage:\n",
        "  deepseek dogfood live-plan [--manifest <path>] [--target-live-runs <n>] [--target-live-success-rate <percent>] [--target-category <category>:<min-runs>:<min-success-percent>] [--limit <n>] [--json]\n",
        "\n",
        "Shows a zero-side-effect plan for collecting model-backed dogfood evidence."
    )
}

fn dogfood_report_help() -> &'static str {
    concat!(
        "DeepSeekCode dogfood report\n",
        "\n",
        "Usage:\n",
        "  deepseek dogfood report [--out <path>] [--limit <n>] [--require-min-runs <n>] [--require-success-rate <percent>] [--require-live-runs <n>] [--require-live-success-rate <percent>] [--require-category <category>:<min-runs>:<min-success-percent>] [--require-live-category <category>:<min-runs>:<min-success-percent>]\n",
        "\n",
        "Renders dogfood ledger stats and optionally enforces release gates."
    )
}

fn dogfood_export_help() -> &'static str {
    concat!(
        "DeepSeekCode dogfood export-benchmark\n",
        "\n",
        "Usage:\n",
        "  deepseek dogfood export-benchmark [--out <path>] [--limit <n>] [--outcome success|failed|stuck|manual]\n",
        "\n",
        "Exports eligible dogfood records as benchmark seed candidates."
    )
}

fn dogfood_promote_help() -> &'static str {
    concat!(
        "DeepSeekCode dogfood promote-benchmark\n",
        "\n",
        "Usage:\n",
        "  deepseek dogfood promote-benchmark [--manifest <path>] [--limit <n>] [--outcome success|failed|stuck|manual] [--dry-run]\n",
        "\n",
        "Promotes eligible dogfood records into the benchmark manifest."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn global_help_mentions_interactive_entrypoint() {
        let help = render_help(&[]);
        assert!(help.contains("deepseek"));
        assert!(help.contains("interactive coding agent REPL"));
        assert!(help.contains("deepseek tui"));
    }

    #[test]
    fn dogfood_replay_help_warns_about_model_calls() {
        let topics = vec!["dogfood".to_string(), "replay-benchmark".to_string()];
        let help = render_help(&topics);
        assert!(help.contains("dogfood replay-benchmark"));
        assert!(help.contains("real\nmodel calls") || help.contains("real model calls"));
    }
}
