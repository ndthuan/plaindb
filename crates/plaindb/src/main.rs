mod cli;
mod mcp;

use std::process::ExitCode;

use clap::Parser;
use tracing_subscriber::EnvFilter;

const LOG_VARIABLE: &str = "PLAINDB_LOG";

fn init_logging() {
    let filter = EnvFilter::try_from_env(LOG_VARIABLE).unwrap_or_else(|_| EnvFilter::new("warn"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();
}

fn main() -> ExitCode {
    let parsed = cli::Cli::parse();
    init_logging();
    cli::execute(parsed)
}
