use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use clap::Args;
use plaindb_core::{FileMeta, Op, Result};
use serde_json::{Value, json};

use super::Context;
use super::output::Output;
use super::write::{apply_tx, write_output};

const STDOUT_NAME: &str = "-";

#[derive(Args)]
pub struct FilePutArgs {
    pub db: String,
    pub coll: String,
    pub id: String,
    #[arg(long, help = "Name of the file within the document")]
    pub name: String,
    #[arg(long, help = "Path of the file to attach; - reads standard input")]
    pub from: PathBuf,
    #[arg(long, help = "MIME type; guessed from the name when left out")]
    pub mime: Option<String>,
    #[arg(long, help = "Write only when the document is at this version")]
    pub if_version: Option<String>,
}

#[derive(Args)]
pub struct FileGetArgs {
    pub db: String,
    pub coll: String,
    pub id: String,
    #[arg(long, help = "Name of the file within the document")]
    pub name: String,
    #[arg(
        long,
        help = "Path to write the file to; without it the bytes go to standard output"
    )]
    pub out: Option<PathBuf>,
}

#[derive(Args)]
pub struct FileListArgs {
    pub db: String,
    pub coll: String,
    pub id: String,
}

#[derive(Args)]
pub struct FileRmArgs {
    pub db: String,
    pub coll: String,
    pub id: String,
    #[arg(long, help = "Name of the file within the document")]
    pub name: String,
    #[arg(long, help = "Write only when the document is at this version")]
    pub if_version: Option<String>,
}

fn read_source(path: &Path) -> Result<Vec<u8>> {
    if path == Path::new(STDOUT_NAME) {
        let mut bytes = Vec::new();
        std::io::stdin().read_to_end(&mut bytes)?;
        Ok(bytes)
    } else {
        Ok(fs::read(path)?)
    }
}

pub fn described(name: &str, meta: &FileMeta) -> Value {
    json!({ "name": name, "size": meta.size, "sha256": meta.sha256, "mime": meta.mime })
}

pub struct Attachment {
    pub db: String,
    pub coll: String,
    pub id: String,
    pub name: String,
    pub bytes: Vec<u8>,
    pub mime: Option<String>,
    pub if_version: Option<String>,
}

pub fn attach(context: &Context, attachment: Attachment) -> Result<Output> {
    let meta = FileMeta::of(
        &attachment.name,
        &attachment.bytes,
        attachment.mime.as_deref(),
    );
    let described = described(&attachment.name, &meta);
    let result = apply_tx(
        context,
        vec![Op::AttachFile {
            db: attachment.db,
            coll: attachment.coll,
            id: attachment.id,
            name: attachment.name,
            bytes: attachment.bytes,
            mime: attachment.mime,
            if_version: attachment.if_version,
        }],
    )?;
    let mut output = write_output(&result);
    output.result["file"] = described;
    Ok(output)
}

pub fn put(context: &Context, args: &FilePutArgs) -> Result<Output> {
    attach(
        context,
        Attachment {
            db: args.db.clone(),
            coll: args.coll.clone(),
            id: args.id.clone(),
            name: args.name.clone(),
            bytes: read_source(&args.from)?,
            mime: args.mime.clone(),
            if_version: args.if_version.clone(),
        },
    )
}

pub fn get(context: &Context, args: &FileGetArgs, json: bool) -> Result<Output> {
    let to_stdout = args
        .out
        .as_deref()
        .is_none_or(|out| out == Path::new(STDOUT_NAME));
    if to_stdout && json {
        return Ok(Output::streamed()
            .with_failure(
                "usage",
                "file get --json needs --out <path>, so the bytes are not mixed with JSON"
                    .to_owned(),
                json!({}),
            )
            .with_exit_code(2));
    }
    let store = context.open_reader()?;
    let (meta, bytes) = store.read_file(&args.db, &args.coll, &args.id, &args.name)?;
    if to_stdout {
        return Ok(Output::bytes(bytes));
    }
    let out = args.out.as_deref().unwrap_or(Path::new(STDOUT_NAME));
    plaindb_core::write_atomic(&std::path::absolute(out)?, &bytes)?;
    let value = described(&args.name, &meta);
    let line = format!("{} {} {}", args.name, meta.size, meta.sha256);
    Ok(Output::lines(value, vec![line]))
}

pub fn list(context: &Context, args: &FileListArgs) -> Result<Output> {
    let store = context.open_reader()?;
    let files = store.list_files(&args.db, &args.coll, &args.id)?;
    let lines = files
        .iter()
        .map(|(name, meta)| format!("{name} {} {}", meta.size, meta.mime))
        .collect();
    let described: serde_json::Map<String, Value> = files
        .iter()
        .map(|(name, meta)| (name.clone(), meta.to_value()))
        .collect();
    Ok(Output::lines(json!({ "files": described }), lines))
}

pub fn rm(context: &Context, args: &FileRmArgs) -> Result<Output> {
    super::write::apply(
        context,
        vec![Op::DetachFile {
            db: args.db.clone(),
            coll: args.coll.clone(),
            id: args.id.clone(),
            name: args.name.clone(),
            if_version: args.if_version.clone(),
        }],
    )
}
