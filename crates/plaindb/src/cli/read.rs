use std::path::Path;

use clap::Args;
use plaindb_core::codec::Doc;
use plaindb_core::{
    At, DEFAULT_HISTORY_LIMIT, Dir, Error, ErrorCode, Query, Result, Store, Timestamp, Version,
};
use serde_json::{Value, json};

use super::output::Output;
use super::{names_result, parse_json};

#[derive(Args)]
pub struct DbArgs {
    pub db: String,
}

#[derive(Args)]
pub struct CollectionArgs {
    pub db: String,
    pub coll: String,
}

#[derive(Args)]
pub struct GetArgs {
    pub db: String,
    pub coll: String,
    pub id: String,
    #[arg(long, help = "Read the document as of a commit id or an RFC 3339 time")]
    pub at: Option<String>,
}

#[derive(Args)]
pub struct FindArgs {
    pub db: String,
    pub coll: String,
    #[arg(long, help = "Filter as a JSON object")]
    pub filter: Option<String>,
    #[arg(long, help = "Sort key as field or field:desc; repeat for more keys")]
    pub sort: Vec<String>,
    #[arg(long)]
    pub limit: Option<u64>,
    #[arg(long, help = "The next_cursor of the previous page")]
    pub cursor: Option<String>,
    #[arg(
        long,
        value_delimiter = ',',
        help = "Fields to return, comma separated"
    )]
    pub fields: Option<Vec<String>>,
}

#[derive(Args)]
pub struct CountArgs {
    pub db: String,
    pub coll: String,
    #[arg(long, help = "Filter as a JSON object")]
    pub filter: Option<String>,
}

#[derive(Args)]
pub struct DistinctArgs {
    pub db: String,
    pub coll: String,
    pub field: String,
    #[arg(long, help = "Filter as a JSON object")]
    pub filter: Option<String>,
}

#[derive(Args)]
pub struct HistoryArgs {
    pub db: String,
    pub coll: String,
    pub id: String,
    #[arg(long)]
    pub limit: Option<u32>,
}

fn filter_value(text: Option<&str>) -> Result<Value> {
    match text {
        Some(text) => parse_json(ErrorCode::InvalidQuery, "the filter", text),
        None => Ok(Value::Null),
    }
}

fn parse_sort(text: &str) -> Result<(String, Dir)> {
    let (field, dir) = match text.rsplit_once(':') {
        Some((field, "desc")) => (field, Dir::Desc),
        Some((field, "asc")) => (field, Dir::Asc),
        _ => (text, Dir::Asc),
    };
    if field.is_empty() {
        return Err(
            Error::new(ErrorCode::InvalidQuery, "a sort key needs a field")
                .with_detail("reason", "bad_sort"),
        );
    }
    Ok((field.to_owned(), dir))
}

fn at_value(text: &str) -> At {
    match text.parse::<Timestamp>() {
        Ok(time) => At::Time(time),
        Err(_) => At::Commit(text.to_owned()),
    }
}

fn doc_entry(doc: Doc, version: Version) -> Value {
    json!({ "doc": Value::Object(doc), "version": version })
}

pub fn get(store: &Store, args: &GetArgs) -> Result<Output> {
    let found = match &args.at {
        Some(at) => store.get_at(&args.db, &args.coll, &args.id, at_value(at))?,
        None => store.get(&args.db, &args.coll, &args.id)?,
    };
    let (doc, version) = found.ok_or_else(|| Error::not_found("document", &args.id))?;
    let doc = Value::Object(doc);
    Ok(Output::text(
        json!({ "doc": doc, "version": version }),
        serde_json::to_string_pretty(&doc).unwrap_or_default(),
    ))
}

pub fn find(store: &Store, args: &FindArgs) -> Result<Output> {
    let sort = args
        .sort
        .iter()
        .map(|text| parse_sort(text))
        .collect::<Result<Vec<_>>>()?;
    let found = store.find(Query {
        db: args.db.clone(),
        coll: args.coll.clone(),
        filter: filter_value(args.filter.as_deref())?,
        sort,
        limit: args.limit,
        cursor: args.cursor.clone(),
        fields: args.fields.clone(),
    })?;
    let lines = found
        .docs
        .iter()
        .map(|(doc, _)| Value::Object(doc.clone()).to_string())
        .collect();
    let docs: Vec<Value> = found
        .docs
        .into_iter()
        .map(|(doc, version)| doc_entry(doc, version))
        .collect();
    Ok(Output::lines(
        json!({ "docs": docs, "next_cursor": found.next_cursor }),
        lines,
    ))
}

pub fn count(store: &Store, args: &CountArgs) -> Result<Output> {
    let count = store.count(&args.db, &args.coll, &filter_value(args.filter.as_deref())?)?;
    Ok(Output::text(json!({ "count": count }), count.to_string()))
}

pub fn distinct(store: &Store, args: &DistinctArgs) -> Result<Output> {
    let values = store.distinct(
        &args.db,
        &args.coll,
        &args.field,
        &filter_value(args.filter.as_deref())?,
    )?;
    let lines = values.iter().map(Value::to_string).collect();
    Ok(Output::lines(json!({ "values": values }), lines))
}

pub fn history(store: &Store, args: &HistoryArgs) -> Result<Output> {
    let entries = store.history(
        &args.db,
        &args.coll,
        &args.id,
        args.limit.unwrap_or(DEFAULT_HISTORY_LIMIT),
    )?;
    let entries: Vec<Value> = entries
        .into_iter()
        .map(|entry| {
            json!({
                "commit": entry.commit,
                "time": entry.time.to_string(),
                "actor": entry.actor,
                "tx_ids": entry.tx_ids,
                "version": entry.version,
            })
        })
        .collect();
    let lines = entries
        .iter()
        .map(|entry| {
            format!(
                "{} {} {}",
                entry["commit"].as_str().unwrap_or_default(),
                entry["time"].as_str().unwrap_or_default(),
                entry["actor"].as_str().unwrap_or("-"),
            )
        })
        .collect();
    Ok(Output::lines(json!({ "entries": entries }), lines))
}

fn relative(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .into_owned()
}

pub fn status(store: &Store) -> Result<Output> {
    let status = store.status()?;
    let root = store.root();
    let invalid: Vec<Value> = status
        .invalid
        .iter()
        .map(|file| {
            json!({
                "db": file.db,
                "collection": file.collection,
                "id": file.id,
                "path": relative(root, &file.path),
                "reason": file.reason,
            })
        })
        .collect();
    let warnings: Vec<Value> = status
        .warnings
        .iter()
        .map(|warning| {
            json!({
                "code": warning.code,
                "db": warning.db,
                "collection": warning.collection,
                "path": relative(root, &warning.path),
                "details": warning.details,
            })
        })
        .collect();
    let git = &status.git;
    let result = json!({
        "documents": status.documents,
        "store_bytes": status.store_bytes,
        "invalid": invalid,
        "warnings": warnings,
        "git": {
            "enabled": git.enabled,
            "commit": serde_json::to_value(git.commit).unwrap_or_default(),
            "branch": git.branch,
            "head": git.head,
            "pending_txs": git.pending_txs,
            "pending_paths": git.pending_paths,
            "push": serde_json::to_value(git.push).unwrap_or_default(),
            "remote": git.remote,
            "remote_url": git.remote_url,
            "unpushed": git.unpushed,
            "last_push_at": git.last_push_at,
            "last_push_error": git.last_push_error,
            "remote_ahead": git.remote_ahead,
            "git_bytes": git.git_bytes,
        },
    });
    Ok(Output::pretty(result))
}

pub fn list_dbs(store: &Store) -> Result<Output> {
    let dbs = store.list_dbs()?;
    Ok(Output::lines(names_result("dbs", &dbs), dbs))
}

pub fn list_collections(store: &Store, args: &DbArgs) -> Result<Output> {
    let collections = store.list_collections(&args.db)?;
    Ok(Output::lines(
        names_result("collections", &collections),
        collections,
    ))
}

pub fn get_schema(store: &Store, args: &CollectionArgs) -> Result<Output> {
    let meta = store.collection_meta(&args.db, &args.coll)?;
    Ok(Output::pretty(json!({ "schema": meta.schema })))
}

pub fn list_indexes(store: &Store, args: &CollectionArgs) -> Result<Output> {
    let meta = store.collection_meta(&args.db, &args.coll)?;
    let indexes = serde_json::to_value(&meta.indexes).unwrap_or_default();
    let lines = meta
        .indexes
        .iter()
        .map(|spec| serde_json::to_string(spec).unwrap_or_default())
        .collect();
    Ok(Output::lines(json!({ "indexes": indexes }), lines))
}
