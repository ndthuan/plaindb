use clap::Args;
use plaindb_core::doctor::{Check, CheckStatus, DoctorOptions, Group, inspect};

use super::Context;
use super::output::Output;

#[derive(Args)]
pub struct DoctorArgs {
    #[arg(long, help = "Do not contact the remote")]
    pub offline: bool,
}

const GROUPS: [Group; 3] = [Group::Git, Group::Store, Group::Environment];

fn tag(status: CheckStatus) -> &'static str {
    match status {
        CheckStatus::Ok => "ok",
        CheckStatus::Warn => "warn",
        CheckStatus::Error => "error",
    }
}

fn lines(checks: &[Check]) -> Vec<String> {
    let mut lines = Vec::new();
    for group in GROUPS {
        let mut members: Vec<&Check> = checks.iter().filter(|check| check.group == group).collect();
        members.sort_by_key(|check| std::cmp::Reverse(check.status));
        if members.is_empty() {
            continue;
        }
        lines.push(format!("{}:", group.as_str()));
        for check in members {
            lines.push(format!(
                "  [{}] {}: {}",
                tag(check.status),
                check.id,
                check.message
            ));
            if let Some(fix) = &check.fix {
                lines.push(format!("        fix: {fix}"));
            }
        }
    }
    lines
}

pub fn run(context: &Context, args: &DoctorArgs) -> Output {
    let report = inspect(
        &context.store,
        DoctorOptions {
            offline: args.offline,
        },
    );
    let exit_code = u8::from(report.errors() > 0);
    Output::lines(report.to_json(), lines(&report.checks)).with_exit_code(exit_code)
}
