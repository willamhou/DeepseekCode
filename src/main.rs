mod cli;
mod config;
mod core;
mod error;
mod integrations;
mod language;
mod model;
mod skills;
mod tools;
mod ui;
mod util;

use error::AppResult;

fn main() -> AppResult<()> {
    let cli = cli::app::Cli::parse();
    cli::run(cli)
}
