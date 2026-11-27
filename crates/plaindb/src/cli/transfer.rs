use std::fs::File;
use std::io::{BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};

use clap::Args;
use plaindb_core::transfer::{ImportOptions, ImportReport, Origin};
use plaindb_core::{Error, Result, Store};
use serde_json::{Value, json};

use super::Context;
use super::output::Output;

const STDIN_NAME: &str = "-";

#[derive(Args)]
pub struct ImportArgs {
    pub db: String,
    pub coll: String,
    #[arg(
        long,
        value_name = "PATH",
        conflicts_with = "dir",
        required_unless_present = "dir",
        help = "File with one JSON object per line; - reads standard input"
    )]
    pub jsonl: Option<PathBuf>,
    #[arg(
        long,
        value_name = "PATH",
        help = "Directory of .json, .yaml, .yml and .md files; the file name is the id"
    )]
    pub dir: Option<PathBuf>,
    #[arg(long, help = "Overwrite documents that already exist")]
    pub replace: bool,
}

#[derive(Args)]
pub struct ExportArgs {
    pub db: String,
    pub coll: String,
    #[arg(long, help = "Write one JSON object per line, sorted by id")]
    pub jsonl: bool,
    #[arg(
        long,
        value_name = "PATH",
        help = "File to write; standard output when left out"
    )]
    pub out: Option<PathBuf>,
}

fn report_output(report: &ImportReport) -> Output {
    let omitted: Vec<Value> = report
        .omitted
        .iter()
        .map(|entry| json!({ "file": entry.file, "reason": entry.reason }))
        .collect();
    let failed: Vec<Value> = report
        .failed
        .iter()
        .map(|entry| {
            let mut value = json!({ "code": entry.code.as_str(), "message": entry.message });
            match &entry.origin {
                Origin::Line(line) => value["line"] = json!(line),
                Origin::File(file) => value["file"] = json!(file),
            }
            value
        })
        .collect();
    let mut lines = vec![format!(
        "imported {}, omitted {}, failed {}",
        report.imported,
        report.omitted.len(),
        report.failed.len()
    )];
    for entry in &report.omitted {
        lines.push(format!("omitted {}: {}", entry.file, entry.reason));
    }
    for entry in &report.failed {
        let place = match &entry.origin {
            Origin::Line(line) => format!("line {line}"),
            Origin::File(file) => file.clone(),
        };
        lines.push(format!("failed {place}: {}", entry.code));
    }
    Output::lines(
        json!({ "imported": report.imported, "omitted": omitted, "failed": failed }),
        lines,
    )
}

pub fn import(context: &Context, args: &ImportArgs) -> Result<Output> {
    let store = context.open()?;
    let options = ImportOptions {
        replace: args.replace,
        actor: context.actor.clone(),
    };
    let report = match (&args.dir, &args.jsonl) {
        (Some(dir), _) => store.import_dir(&args.db, &args.coll, dir, &options)?,
        (None, Some(path)) if path == Path::new(STDIN_NAME) => {
            store.import_jsonl(&args.db, &args.coll, std::io::stdin().lock(), &options)?
        }
        (None, Some(path)) => {
            let reader = BufReader::new(File::open(path)?);
            store.import_jsonl(&args.db, &args.coll, reader, &options)?
        }
        (None, None) => unreachable!("clap requires a source"),
    };
    Ok(report_output(&report))
}

fn export_to(store: &Store, args: &ExportArgs, out: impl Write) -> Result<u64> {
    store.export_jsonl(&args.db, &args.coll, BufWriter::new(out))
}

pub fn export(context: &Context, args: &ExportArgs) -> Result<Output> {
    if !args.jsonl {
        return Err(Error::new(
            plaindb_core::ErrorCode::InvalidQuery,
            "export needs --jsonl, the only format there is",
        )
        .with_detail("reason", "no_format"));
    }
    let store = context.open_reader()?;
    match &args.out {
        Some(path) => {
            let exported = export_to(&store, args, File::create(path)?)?;
            Ok(Output::text(
                json!({ "exported": exported, "out": path.to_string_lossy() }),
                format!("exported {exported} documents"),
            ))
        }
        None => {
            export_to(&store, args, std::io::stdout().lock())?;
            Ok(Output::streamed())
        }
    }
}
