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
    let cli = match cli::app::Cli::parse() {
        Ok(cli) => cli,
        Err(message) => {
            eprintln!("error: {message}");
            std::process::exit(2);
        }
    };
    cli::run(cli)
}
