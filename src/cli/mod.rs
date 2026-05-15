pub mod app;
pub mod commands;

use crate::error::AppResult;

pub fn run(cli: app::Cli) -> AppResult<()> {
    match cli.command.unwrap_or_default() {
        app::Command::Benchmark(args) => commands::benchmark::run(args),
        app::Command::Dogfood(action) => commands::dogfood::run(action),
        app::Command::Chat(args) => commands::chat::run(args),
        app::Command::Completion(shell) => commands::completion::run(shell),
        app::Command::Run(args) => commands::run::run(args),
        app::Command::Exec(action) => commands::exec::run(action),
        app::Command::Agents(action) => commands::agents::run(action),
        app::Command::Diagnostics(args) => commands::diagnostics::run(args),
        app::Command::Diff(args) => commands::diff::run(args),
        app::Command::Resume(args) => commands::resume::run(args),
        app::Command::Restore(action) => commands::restore::run(action),
        app::Command::Config(args) => commands::config::run(args),
        app::Command::Doctor(args) => commands::doctor::run(args),
        app::Command::Serve(args) => commands::serve::run(args),
        app::Command::Tui(args) => commands::tui::run(args),
        app::Command::Update(args) => commands::update::run(args),
        app::Command::Smoke(args) => commands::smoke::run(args),
        app::Command::Pr(action) => commands::pr::run(action),
        app::Command::Mcp(action) => commands::mcp::run(action),
        app::Command::Help(args) => commands::help::run(args),
        app::Command::Version => commands::version::run(),
    }
}
