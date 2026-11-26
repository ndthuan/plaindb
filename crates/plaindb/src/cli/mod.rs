mod output;
mod read;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use plaindb_core::{Error, ErrorCode, Result, Store};
use serde_json::{Value, json};

use output::{Output, print_failure, print_success};

#[derive(Parser)]
#[command(
    name = "plaindb",
    version,
    about = "A document database made of plain text files"
)]
pub struct Cli {
    #[arg(
        long,
        global = true,
        env = "PLAINDB_STORE",
        default_value = ".",
        help = "Directory of the store"
    )]
    pub store: PathBuf,
    #[arg(long, global = true, help = "Print one JSON object per command")]
    pub json: bool,
    #[arg(long, global = true, help = "Name recorded as the author of a change")]
    pub actor: Option<String>,
    #[arg(
        long,
        global = true,
        help = "ULID naming the transaction of a write; repeating it is safe"
    )]
    pub tx_id: Option<String>,
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    #[command(about = "Read one document")]
    Get(read::GetArgs),
    #[command(about = "Find documents")]
    Find(read::FindArgs),
    #[command(about = "Count documents")]
    Count(read::CountArgs),
    #[command(about = "List the distinct values of a field")]
    Distinct(read::DistinctArgs),
    #[command(about = "List the commits that changed a document")]
    History(read::HistoryArgs),
    #[command(about = "Show the state of the store")]
    Status,
    #[command(subcommand, about = "Databases")]
    Db(DbCommand),
    #[command(subcommand, about = "Collections")]
    Collection(CollectionCommand),
    #[command(subcommand, about = "Indexes")]
    Index(IndexCommand),
}

#[derive(Subcommand)]
pub enum DbCommand {
    #[command(about = "List the databases")]
    List,
}

#[derive(Subcommand)]
pub enum CollectionCommand {
    #[command(about = "List the collections of a database")]
    List(read::DbArgs),
    #[command(subcommand, about = "The schema of a collection")]
    Schema(SchemaCommand),
}

#[derive(Subcommand)]
pub enum SchemaCommand {
    #[command(about = "Print the schema of a collection")]
    Get(read::CollectionArgs),
}

#[derive(Subcommand)]
pub enum IndexCommand {
    #[command(about = "List the indexes of a collection")]
    List(read::CollectionArgs),
}

pub struct Context {
    pub store: PathBuf,
}

impl Context {
    pub fn open(&self) -> Result<Store> {
        Store::open(&self.store)
    }
}

pub fn bad_json(code: ErrorCode, what: &str, source: &serde_json::Error) -> Error {
    Error::new(code, format!("{what} is not valid JSON: {source}"))
        .with_detail("reason", "bad_json")
}

pub fn parse_json(code: ErrorCode, what: &str, text: &str) -> Result<Value> {
    serde_json::from_str(text).map_err(|source| bad_json(code, what, &source))
}

fn run(context: &Context, command: Command) -> Result<Output> {
    match command {
        Command::Get(args) => read::get(&context.open()?, &args),
        Command::Find(args) => read::find(&context.open()?, &args),
        Command::Count(args) => read::count(&context.open()?, &args),
        Command::Distinct(args) => read::distinct(&context.open()?, &args),
        Command::History(args) => read::history(&context.open()?, &args),
        Command::Status => read::status(&context.open()?),
        Command::Db(DbCommand::List) => read::list_dbs(&context.open()?),
        Command::Collection(CollectionCommand::List(args)) => {
            read::list_collections(&context.open()?, &args)
        }
        Command::Collection(CollectionCommand::Schema(SchemaCommand::Get(args))) => {
            read::get_schema(&context.open()?, &args)
        }
        Command::Index(IndexCommand::List(args)) => read::list_indexes(&context.open()?, &args),
    }
}

pub fn execute(cli: Cli) -> ExitCode {
    let context = Context { store: cli.store };
    tracing::debug!(
        store = %context.store.display(),
        actor = ?cli.actor,
        tx_id = ?cli.tx_id,
        "running a command"
    );
    match run(&context, cli.command) {
        Ok(output) => {
            print_success(cli.json, &output);
            ExitCode::SUCCESS
        }
        Err(error) => {
            print_failure(cli.json, &error);
            ExitCode::from(1)
        }
    }
}

pub fn names_result(key: &str, names: &[String]) -> Value {
    json!({ key: names })
}
