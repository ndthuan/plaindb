pub mod admin;
pub mod doctor;
pub mod git;
pub mod output;
pub mod read;
pub mod transfer;
pub mod write;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use plaindb_core::{Error, ErrorCode, Op, Result, Store, Tx};
use serde_json::{Value, json};

use crate::mcp;
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
    #[command(about = "Check the store and its environment, and say how to fix what is wrong")]
    Doctor(doctor::DoctorArgs),
    #[command(about = "Create a store")]
    Init(admin::InitArgs),
    #[command(subcommand, about = "Read or change the settings of the store")]
    Config(ConfigCommand),
    #[command(about = "Add a document that does not exist yet")]
    Insert(write::InsertArgs),
    #[command(about = "Write a whole document, replacing any earlier one")]
    Put(write::PutArgs),
    #[command(about = "Change a document with a JSON merge patch")]
    Patch(write::PatchArgs),
    #[command(about = "Remove a document")]
    Delete(write::DeleteArgs),
    #[command(about = "Apply a list of operations as one atomic transaction")]
    Tx(write::TxArgs),
    #[command(subcommand, about = "Version control of the store")]
    Git(GitCommand),
    #[command(about = "Commit the pending changes")]
    Commit(git::CommitArgs),
    #[command(about = "Push the commits to the remote")]
    Push(git::PushArgs),
    #[command(about = "Check every document against its schema")]
    Validate,
    #[command(about = "Add documents from a JSONL file or a directory of files")]
    Import(transfer::ImportArgs),
    #[command(about = "Write every document of a collection as JSONL")]
    Export(transfer::ExportArgs),
    #[command(about = "Look for changes made to the files by hand")]
    Rescan,
    #[command(about = "Drop the index cache and read every file again")]
    Reindex,
    #[command(subcommand, about = "Databases")]
    Db(DbCommand),
    #[command(subcommand, about = "Collections")]
    Collection(CollectionCommand),
    #[command(subcommand, about = "Indexes")]
    Index(IndexCommand),
    #[command(about = "Serve the store to AI agents over the Model Context Protocol")]
    Mcp(mcp::McpArgs),
}

#[derive(Subcommand)]
pub enum ConfigCommand {
    #[command(about = "Print one setting or all of them")]
    Get(admin::ConfigGetArgs),
    #[command(about = "Change a setting; an empty value clears a limit")]
    Set(admin::ConfigSetArgs),
}

#[derive(Subcommand)]
pub enum GitCommand {
    #[command(about = "Put the store under version control")]
    Enable,
    #[command(subcommand, about = "The remote that receives pushes")]
    Remote(RemoteCommand),
}

#[derive(Subcommand)]
pub enum RemoteCommand {
    #[command(about = "Set the URL of the remote")]
    Set(git::RemoteSetArgs),
}

#[derive(Subcommand)]
pub enum DbCommand {
    #[command(about = "List the databases")]
    List,
    #[command(about = "Create a database")]
    Create(read::DbArgs),
    #[command(about = "Drop a database with everything in it")]
    Drop(read::DbArgs),
}

#[derive(Subcommand)]
pub enum CollectionCommand {
    #[command(about = "List the collections of a database")]
    List(read::DbArgs),
    #[command(about = "Create a collection")]
    Create(admin::CollectionCreateArgs),
    #[command(about = "Drop a collection with every document in it")]
    Drop(read::CollectionArgs),
    #[command(subcommand, about = "The schema of a collection")]
    Schema(SchemaCommand),
}

#[derive(Subcommand)]
pub enum SchemaCommand {
    #[command(about = "Print the schema of a collection")]
    Get(read::CollectionArgs),
    #[command(about = "Set or clear the schema of a collection")]
    Set(admin::SchemaSetArgs),
}

#[derive(Subcommand)]
pub enum IndexCommand {
    #[command(about = "List the indexes of a collection")]
    List(read::CollectionArgs),
    #[command(about = "Add an index")]
    Add(admin::IndexAddArgs),
    #[command(about = "Drop an index by its field, or the text index by the word text")]
    Drop(admin::IndexDropArgs),
}

#[derive(Clone)]
pub struct Context {
    pub store: PathBuf,
    pub actor: Option<String>,
    pub tx_id: Option<String>,
}

impl Context {
    pub fn open(&self) -> Result<Store> {
        Store::open(&self.store)
    }

    pub fn open_reader(&self) -> Result<Store> {
        Store::open_reader(&self.store)
    }

    pub fn transaction(&self, ops: Vec<Op>) -> Tx {
        Tx {
            id: self.tx_id.clone(),
            actor: self.actor.clone(),
            ops,
            keep_timestamps: false,
        }
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
        Command::Get(args) => read::get(&context.open_reader()?, &args),
        Command::Find(args) => read::find(&context.open_reader()?, &args),
        Command::Count(args) => read::count(&context.open_reader()?, &args),
        Command::Distinct(args) => read::distinct(&context.open_reader()?, &args),
        Command::History(args) => read::history(&context.open_reader()?, &args),
        Command::Status => read::status(&context.open_reader()?),
        Command::Db(DbCommand::List) => read::list_dbs(&context.open_reader()?),
        Command::Collection(CollectionCommand::List(args)) => {
            read::list_collections(&context.open_reader()?, &args)
        }
        Command::Collection(CollectionCommand::Schema(SchemaCommand::Get(args))) => {
            read::get_schema(&context.open_reader()?, &args)
        }
        Command::Index(IndexCommand::List(args)) => {
            read::list_indexes(&context.open_reader()?, &args)
        }
        Command::Doctor(args) => Ok(doctor::run(context, &args)),
        Command::Init(args) => admin::init(context, &args),
        Command::Config(ConfigCommand::Get(args)) => {
            admin::config_get(&context.open_reader()?, &args)
        }
        Command::Config(ConfigCommand::Set(args)) => admin::config_set(context, &args),
        Command::Insert(args) => write::insert(context, &args),
        Command::Put(args) => write::put(context, &args),
        Command::Patch(args) => write::patch(context, &args),
        Command::Delete(args) => write::delete(context, &args),
        Command::Tx(args) => write::transaction(context, &args),
        Command::Git(GitCommand::Enable) => git::enable(&context.open()?),
        Command::Git(GitCommand::Remote(RemoteCommand::Set(args))) => {
            git::set_remote(&context.open()?, &args)
        }
        Command::Commit(args) => git::commit(context, &args),
        Command::Push(args) if args.after_write => {
            plaindb_core::push_in_background(&context.store)?;
            Ok(Output::streamed())
        }
        Command::Push(args) => git::push(&context.open()?, &args),
        Command::Import(args) => transfer::import(context, &args),
        Command::Export(args) => transfer::export(context, &args),
        Command::Validate => admin::validate(&context.open_reader()?),
        Command::Rescan => admin::rescan(&context.open()?),
        Command::Reindex => admin::reindex(context),
        Command::Db(DbCommand::Create(args)) => admin::create_db(&context.open()?, &args),
        Command::Db(DbCommand::Drop(args)) => admin::drop_db(&context.open()?, &args),
        Command::Collection(CollectionCommand::Create(args)) => {
            admin::create_collection(&context.open()?, &args)
        }
        Command::Collection(CollectionCommand::Drop(args)) => {
            admin::drop_collection(&context.open()?, &args)
        }
        Command::Collection(CollectionCommand::Schema(SchemaCommand::Set(args))) => {
            admin::set_schema(&context.open()?, &args)
        }
        Command::Index(IndexCommand::Add(args)) => admin::add_index(&context.open()?, &args),
        Command::Index(IndexCommand::Drop(args)) => admin::drop_index(&context.open()?, &args),
        Command::Mcp(args) => mcp::serve(context, &args),
    }
}

pub fn execute(cli: Cli) -> ExitCode {
    let context = Context {
        store: cli.store,
        actor: cli.actor,
        tx_id: cli.tx_id,
    };
    tracing::debug!(store = %context.store.display(), "running a command");
    match run(&context, cli.command) {
        Ok(output) => {
            print_success(cli.json, &output);
            ExitCode::from(output.exit_code)
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
