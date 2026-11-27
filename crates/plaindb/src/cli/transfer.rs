use std::fs::{File, OpenOptions};
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
        help = "File to write; standard output when left out or -"
    )]
    pub out: Option<PathBuf>,
}

fn report_json(report: &ImportReport) -> Value {
    let omitted: Vec<Value> = report
        .omitted
        .iter()
        .map(|entry| json!({ "file": entry.file, "reason": entry.reason }))
        .collect();
    let failed: Vec<Value> = report
        .failed
        .iter()
        .map(|entry| {
            let mut value = json!({
                "code": entry.code,
                "message": entry.message,
                "details": entry.details,
            });
            match &entry.origin {
                Origin::Line(line) => value["line"] = json!(line),
                Origin::File(file) => value["file"] = json!(file),
            }
            value
        })
        .collect();
    let mut value = json!({
        "imported": report.imported,
        "omitted": omitted,
        "files_dropped": report.files_dropped,
        "failed": failed,
    });
    if let Some(aborted) = &report.aborted {
        value["aborted"] = json!({ "code": aborted.code, "message": aborted.message });
    }
    value
}

fn failure_message(report: &ImportReport) -> Option<String> {
    if let Some(aborted) = &report.aborted {
        return Some(format!("the import stopped: {}", aborted.message));
    }
    (report.imported == 0 && !report.failed.is_empty())
        .then(|| format!("no document was imported, {} failed", report.failed.len()))
}

fn report_output(report: &ImportReport) -> Output {
    let mut lines = vec![format!(
        "imported {}, omitted {}, failed {}",
        report.imported,
        report.omitted.len(),
        report.failed.len()
    )];
    for entry in &report.omitted {
        lines.push(format!("omitted {}: {}", entry.file, entry.reason));
    }
    for id in &report.files_dropped {
        lines.push(format!("dropped the attached files of {id}"));
    }
    for entry in &report.failed {
        let place = match &entry.origin {
            Origin::Line(line) => format!("line {line}"),
            Origin::File(file) => file.clone(),
        };
        lines.push(format!("failed {place}: {}", entry.code));
    }
    let result = report_json(report);
    let output = Output::lines(result.clone(), lines);
    match failure_message(report) {
        Some(message) => output.with_exit_code(1).with_failure(
            "import_failed",
            message,
            json!({ "report": result }),
        ),
        None => output,
    }
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

struct TempFile {
    path: PathBuf,
    armed: bool,
}

impl Drop for TempFile {
    fn drop(&mut self) {
        if self.armed {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

struct WatchedPipe<W> {
    inner: W,
    broken: bool,
}

impl<W: Write> Write for WatchedPipe<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.inner.write(buf).inspect_err(|error| {
            self.broken |= error.kind() == std::io::ErrorKind::BrokenPipe;
        })
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush().inspect_err(|error| {
            self.broken |= error.kind() == std::io::ErrorKind::BrokenPipe;
        })
    }
}

fn temp_path_beside(target: &Path) -> PathBuf {
    let name = target
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let directory = match target.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    };
    directory.join(format!(".{name}.tmp-{}", plaindb_core::new_id()))
}

fn export_through_temp_file(store: &Store, args: &ExportArgs, target: &Path) -> Result<u64> {
    let mut temp = TempFile {
        path: temp_path_beside(target),
        armed: false,
    };
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp.path)?;
    temp.armed = true;
    let mut writer = BufWriter::new(file);
    let exported = store.export_jsonl(&args.db, &args.coll, &mut writer)?;
    writer.flush()?;
    writer
        .into_inner()
        .map_err(|error| Error::from(error.into_error()))?
        .sync_all()?;
    std::fs::rename(&temp.path, target)?;
    temp.armed = false;
    Ok(exported)
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
    store.collection_meta(&args.db, &args.coll)?;
    match args
        .out
        .as_deref()
        .filter(|path| *path != Path::new(STDIN_NAME))
    {
        Some(path) => {
            let exported = export_through_temp_file(&store, args, path)?;
            Ok(Output::text(
                json!({ "exported": exported, "out": path.to_string_lossy() }),
                format!("exported {exported} documents"),
            ))
        }
        None => {
            let mut out = BufWriter::new(WatchedPipe {
                inner: std::io::stdout().lock(),
                broken: false,
            });
            let written = store
                .export_jsonl(&args.db, &args.coll, &mut out)
                .and_then(|_| out.flush().map_err(Error::from));
            match written {
                Err(_) if out.get_ref().broken => Ok(Output::streamed()),
                other => other.map(|()| Output::streamed()),
            }
        }
    }
}
