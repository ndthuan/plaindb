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
    let json = std::env::args_os()
        .skip(1)
        .take_while(|arg| arg != "--")
        .any(|arg| arg == "--json");
    let parsed = match cli::Cli::try_parse() {
        Ok(parsed) => parsed,
        Err(error) => return cli::usage_error(json, &error),
    };
    init_logging();
    plaindb_core::set_push_program(std::env::current_exe().ok());
    cli::execute(parsed)
}
