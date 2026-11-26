use clap::Args;
use plaindb_core::{Result, Store};
use serde_json::json;

use super::Context;
use super::output::Output;

#[derive(Args)]
pub struct RemoteSetArgs {
    pub url: String,
}

#[derive(Args)]
pub struct CommitArgs {
    #[arg(short, long, help = "Message of the commit")]
    pub message: Option<String>,
}

#[derive(Args)]
pub struct PushArgs {
    #[arg(long, help = "Clear the remote_ahead flag before pushing")]
    pub reset_remote_ahead: bool,
}

pub fn enable(store: &Store) -> Result<Output> {
    store.enable_git()?;
    Ok(Output::text(json!({ "git": true }), "git enabled"))
}

pub fn set_remote(store: &Store, args: &RemoteSetArgs) -> Result<Output> {
    store.set_remote(&args.url)?;
    let remote = super::admin::read_settings(store)?.remote;
    Ok(Output::text(
        json!({ "remote": remote, "url": args.url }),
        format!("{remote} is {}", args.url),
    ))
}

pub fn commit(context: &Context, args: &CommitArgs) -> Result<Output> {
    let store = context.open()?;
    let commit = store.commit(args.message.as_deref(), context.actor.as_deref())?;
    let text = commit
        .clone()
        .unwrap_or_else(|| "nothing to commit".to_owned());
    Ok(Output::text(json!({ "commit": commit }), text))
}

pub fn push(store: &Store, args: &PushArgs) -> Result<Output> {
    if args.reset_remote_ahead {
        store.reset_remote_ahead()?;
    }
    let outcome = store.push()?;
    let outcome = serde_json::to_value(outcome).unwrap_or_default();
    let text = outcome.as_str().unwrap_or_default().to_owned();
    Ok(Output::text(json!({ "outcome": outcome }), text))
}
