use std::process::ExitCode;

use clap::Parser;

#[derive(Parser)]
#[command(name = "plaindb", disable_version_flag = true)]
struct Cli {
    #[arg(long)]
    version: bool,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    if cli.version {
        println!("plaindb {}", env!("CARGO_PKG_VERSION"));
        ExitCode::SUCCESS
    } else {
        ExitCode::from(2)
    }
}
