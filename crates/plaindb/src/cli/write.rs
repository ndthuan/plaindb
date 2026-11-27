use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use clap::Args;
use plaindb_core::codec::Doc;
use plaindb_core::transfer::decode_file_bytes;
use plaindb_core::{Error, ErrorCode, Op, Result, TxResult};
use serde_json::{Map, Value, json};

use super::output::Output;
use super::{Context, parse_json};

const STDIN_NAME: &str = "-";

#[derive(Args)]
pub struct DocumentSource {
    #[arg(long, conflicts_with = "file", help = "The document as a JSON object")]
    pub data: Option<String>,
    #[arg(
        long,
        help = "File with the document: .json, .yaml, .yml or .md; - reads standard input"
    )]
    pub file: Option<PathBuf>,
}

#[derive(Args)]
#[group(required = true, multiple = false)]
pub struct JsonSource {
    #[arg(long, help = "The value as JSON")]
    pub data: Option<String>,
    #[arg(long, help = "File with the value as JSON; - reads standard input")]
    pub file: Option<PathBuf>,
}

impl JsonSource {
    pub fn read(&self, what: &str) -> Result<Value> {
        read_json(self.data.as_deref(), self.file.as_deref(), what)
    }
}

pub fn read_json(data: Option<&str>, file: Option<&Path>, what: &str) -> Result<Value> {
    let text = match (data, file) {
        (Some(data), _) => data.to_owned(),
        (None, Some(path)) => String::from_utf8(read_bytes(path)?)
            .map_err(|source| Error::invalid_document("not_utf8", source.to_string()))?,
        (None, None) => unreachable!("clap requires a source"),
    };
    parse_json(ErrorCode::InvalidDocument, what, &text)
}

#[derive(Args)]
pub struct InsertArgs {
    pub db: String,
    pub coll: String,
    #[arg(help = "Id of the document; a new one is made when left out")]
    pub id: Option<String>,
    #[command(flatten)]
    pub source: DocumentSource,
}

#[derive(Args)]
pub struct PutArgs {
    pub db: String,
    pub coll: String,
    pub id: String,
    #[command(flatten)]
    pub source: DocumentSource,
    #[arg(long, help = "Write only when the document is at this version")]
    pub if_version: Option<String>,
}

#[derive(Args)]
pub struct PatchArgs {
    pub db: String,
    pub coll: String,
    pub id: String,
    #[command(flatten)]
    pub source: JsonSource,
    #[arg(long, help = "Write only when the document is at this version")]
    pub if_version: Option<String>,
}

#[derive(Args)]
pub struct DeleteArgs {
    pub db: String,
    pub coll: String,
    pub id: String,
    #[arg(long, help = "Write only when the document is at this version")]
    pub if_version: Option<String>,
}

#[derive(Args)]
pub struct TxArgs {
    #[command(flatten)]
    pub source: JsonSource,
}

fn read_bytes(path: &Path) -> Result<Vec<u8>> {
    if path == Path::new(STDIN_NAME) {
        let mut bytes = Vec::new();
        std::io::stdin().read_to_end(&mut bytes)?;
        Ok(bytes)
    } else {
        Ok(fs::read(path)?)
    }
}

pub fn into_doc(value: Value) -> Result<Doc> {
    match value {
        Value::Object(map) => Ok(map),
        _ => Err(Error::invalid_document(
            "not_an_object",
            "a document must be a JSON object",
        )),
    }
}

fn read_doc(source: &DocumentSource) -> Result<Doc> {
    if let Some(data) = &source.data {
        return into_doc(parse_json(ErrorCode::InvalidDocument, "the data", data)?);
    }
    match &source.file {
        Some(path) if path != Path::new(STDIN_NAME) => decode_file_bytes(path, &fs::read(path)?),
        other => {
            let path = other.as_deref().unwrap_or(Path::new(STDIN_NAME));
            let bytes = read_bytes(path)?;
            let text = String::from_utf8(bytes)
                .map_err(|source| Error::invalid_document("not_utf8", source.to_string()))?;
            into_doc(parse_json(
                ErrorCode::InvalidDocument,
                "standard input",
                &text,
            )?)
        }
    }
}

pub fn write_output(result: &TxResult) -> Output {
    let results: Vec<Value> = result
        .results
        .iter()
        .map(|entry| {
            json!({
                "id": entry.id,
                "version": entry.version,
                "previous_version": entry.previous_version,
            })
        })
        .collect();
    let mut value = json!({ "tx_id": result.tx_id, "results": results });
    if let Some(push) = result.push {
        value["push"] = serde_json::to_value(push).unwrap_or_default();
    }
    if let Some(deferred) = &result.deferred {
        value["deferred"] = json!({ "step": deferred.step, "message": deferred.message });
    }
    let lines = result
        .results
        .iter()
        .map(|entry| format!("{} {}", entry.id, entry.version.as_deref().unwrap_or("-")))
        .collect();
    let output = Output::lines(value, lines);
    match &result.deferred {
        Some(deferred) => output.with_warning(format!(
            "the write is committed; {} will finish on the next write ({})",
            deferred.step, deferred.message
        )),
        None => output,
    }
}

pub fn apply_tx(context: &Context, ops: Vec<Op>) -> Result<TxResult> {
    context.open()?.apply(context.transaction(ops))
}

pub fn apply(context: &Context, ops: Vec<Op>) -> Result<Output> {
    Ok(write_output(&apply_tx(context, ops)?))
}

pub fn insert(context: &Context, args: &InsertArgs) -> Result<Output> {
    let doc = read_doc(&args.source)?;
    apply(
        context,
        vec![Op::Insert {
            db: args.db.clone(),
            coll: args.coll.clone(),
            id: args.id.clone(),
            doc,
        }],
    )
}

pub fn put(context: &Context, args: &PutArgs) -> Result<Output> {
    let doc = read_doc(&args.source)?;
    apply(
        context,
        vec![Op::Put {
            db: args.db.clone(),
            coll: args.coll.clone(),
            id: args.id.clone(),
            doc,
            if_version: args.if_version.clone(),
        }],
    )
}

pub fn patch(context: &Context, args: &PatchArgs) -> Result<Output> {
    let patch = args.source.read("the patch")?;
    apply(
        context,
        vec![Op::Patch {
            db: args.db.clone(),
            coll: args.coll.clone(),
            id: args.id.clone(),
            patch,
            if_version: args.if_version.clone(),
        }],
    )
}

pub fn delete(context: &Context, args: &DeleteArgs) -> Result<Output> {
    apply(
        context,
        vec![Op::Delete {
            db: args.db.clone(),
            coll: args.coll.clone(),
            id: args.id.clone(),
            if_version: args.if_version.clone(),
        }],
    )
}

fn invalid_op(index: usize, message: impl Into<String>) -> Error {
    Error::invalid_document("invalid_op", message).with_detail("index", index)
}

fn text_field(entry: &mut Map<String, Value>, index: usize, key: &str) -> Result<Option<String>> {
    match entry.remove(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) => Ok(Some(text)),
        Some(_) => Err(invalid_op(index, format!("{key} must be a string"))),
    }
}

fn need(index: usize, value: Option<String>, key: &str) -> Result<String> {
    value.ok_or_else(|| invalid_op(index, format!("operation {index} needs {key}")))
}

fn required_text(entry: &mut Map<String, Value>, index: usize, key: &str) -> Result<String> {
    need(index, text_field(entry, index, key)?, key)
}

pub fn parse_op(index: usize, value: Value) -> Result<Op> {
    let Value::Object(mut entry) = value else {
        return Err(invalid_op(
            index,
            format!("operation {index} is not an object"),
        ));
    };
    let kind = required_text(&mut entry, index, "op")?;
    let db = required_text(&mut entry, index, "db")?;
    let coll = required_text(&mut entry, index, "collection")?;
    let id = text_field(&mut entry, index, "id")?;
    let if_version = text_field(&mut entry, index, "if_version")?;
    let doc = entry.remove("doc");
    let patch = entry.remove("patch");
    if let Some(extra) = entry.keys().next() {
        return Err(invalid_op(
            index,
            format!("operation {index} has an unknown field {extra}"),
        ));
    }
    let op = match kind.as_str() {
        "insert" => Op::Insert {
            db,
            coll,
            id,
            doc: into_doc(doc.ok_or_else(|| invalid_op(index, "insert needs doc"))?)?,
        },
        "put" => Op::Put {
            db,
            coll,
            id: need(index, id, "id")?,
            doc: into_doc(doc.ok_or_else(|| invalid_op(index, "put needs doc"))?)?,
            if_version,
        },
        "patch" => Op::Patch {
            db,
            coll,
            id: need(index, id, "id")?,
            patch: patch.ok_or_else(|| invalid_op(index, "patch needs patch"))?,
            if_version,
        },
        "delete" => Op::Delete {
            db,
            coll,
            id: need(index, id, "id")?,
            if_version,
        },
        other => return Err(invalid_op(index, format!("unknown operation {other}"))),
    };
    Ok(op)
}

pub fn parse_ops(value: Value) -> Result<Vec<Op>> {
    let Value::Array(entries) = value else {
        return Err(Error::invalid_document(
            "invalid_op",
            "a transaction file holds a list of operations",
        ));
    };
    entries
        .into_iter()
        .enumerate()
        .map(|(index, entry)| parse_op(index, entry))
        .collect()
}

pub fn transaction(context: &Context, args: &TxArgs) -> Result<Output> {
    let ops = parse_ops(args.source.read("the transaction")?)?;
    apply(context, ops)
}
