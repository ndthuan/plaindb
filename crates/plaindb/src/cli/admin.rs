use std::fs;
use std::path::PathBuf;
use std::time::Duration;

use clap::{Args, ValueEnum};
use plaindb_core::config::{StoreConfig, from_file_bytes, to_file_bytes};
use plaindb_core::store::STORE_FILE;
use plaindb_core::{
    CommitMode, Error, ErrorCode, Format, IndexSpec, PushMode, Result, Store, WriteLock, cache,
    write_atomic,
};
use serde_json::{Value, json};

use super::output::Output;
use super::read::{CollectionArgs, DbArgs, invalid_entries};
use super::{Context, parse_json};

#[derive(Clone, Copy, ValueEnum)]
pub enum PushArg {
    Off,
    #[value(name = "on-commit")]
    OnCommit,
}

#[derive(Clone, Copy, ValueEnum)]
pub enum CommitArg {
    Each,
    Manual,
}

#[derive(Clone, Copy, ValueEnum)]
pub enum FormatArg {
    Json,
    Yaml,
    Md,
}

impl From<FormatArg> for Format {
    fn from(value: FormatArg) -> Self {
        match value {
            FormatArg::Json => Self::Json,
            FormatArg::Yaml => Self::Yaml,
            FormatArg::Md => Self::Md,
        }
    }
}

#[derive(Args)]
pub struct InitArgs {
    #[arg(long, help = "Keep the history of the store in git")]
    pub git: bool,
    #[arg(long, help = "URL that receives pushes; needs --git")]
    pub remote: Option<String>,
    #[arg(long, value_enum, help = "Push after every commit, or never")]
    pub push: Option<PushArg>,
    #[arg(
        long,
        value_enum,
        help = "Commit after every write, or only on request"
    )]
    pub commit: Option<CommitArg>,
}

#[derive(Args)]
pub struct ConfigGetArgs {
    #[arg(help = "A setting such as commit or limits.max_results")]
    pub key: Option<String>,
}

#[derive(Args)]
pub struct ConfigSetArgs {
    pub key: String,
    #[arg(allow_hyphen_values = true)]
    pub value: String,
}

#[derive(Args)]
pub struct CollectionCreateArgs {
    pub db: String,
    pub coll: String,
    #[arg(long, value_enum)]
    pub format: FormatArg,
}

#[derive(Args)]
pub struct SchemaSetArgs {
    pub db: String,
    pub coll: String,
    #[arg(long, conflicts_with = "clear", required_unless_present = "clear")]
    pub file: Option<PathBuf>,
    #[arg(long, help = "Remove the schema")]
    pub clear: bool,
}

#[derive(Args)]
pub struct IndexAddArgs {
    pub db: String,
    pub coll: String,
    #[arg(long, conflicts_with = "text", required_unless_present = "text")]
    pub field: Option<String>,
    #[arg(
        long,
        requires = "field",
        help = "Refuse a second document with the same value"
    )]
    pub unique: bool,
    #[arg(long, requires = "field", help = "Index every element of an array")]
    pub multi: bool,
    #[arg(long, value_delimiter = ',', help = "Text index over these fields")]
    pub text: Option<Vec<String>>,
}

#[derive(Args)]
pub struct IndexDropArgs {
    pub db: String,
    pub coll: String,
    #[arg(help = "A field name, or text for the text index")]
    pub target: String,
}

const SETTING_NAMES: &str = "commit, push, remote, branch, lock_timeout_ms, limits.max_document_bytes, limits.max_results, limits.max_scan_ms";

pub fn read_settings(store: &Store) -> Result<StoreConfig> {
    from_file_bytes(&fs::read(store.root().join(STORE_FILE))?)
}

fn unknown_setting(key: &str) -> Error {
    Error::invalid_name("setting", key).with_detail("known", SETTING_NAMES)
}

fn invalid_value(key: &str, expected: &str) -> Error {
    Error::invalid_document("invalid_value", format!("{key} takes {expected}"))
        .with_detail("key", key)
}

fn parse_name(key: &str, value: &str) -> Result<String> {
    let valid =
        !value.is_empty() && !value.starts_with('-') && !value.contains(char::is_whitespace);
    if valid {
        Ok(value.to_owned())
    } else {
        Err(invalid_value(
            key,
            "a name without spaces that does not start with -",
        ))
    }
}

fn parse_count(key: &str, value: &str) -> Result<u64> {
    value
        .parse()
        .map_err(|_| invalid_value(key, "a whole number"))
}

fn parse_limit(key: &str, value: &str) -> Result<Option<u64>> {
    if value.is_empty() {
        Ok(None)
    } else {
        parse_count(key, value).map(Some)
    }
}

fn apply_setting(config: &mut StoreConfig, key: &str, value: &str) -> Result<()> {
    match key {
        "remote" => config.remote = parse_name(key, value)?,
        "branch" => config.branch = parse_name(key, value)?,
        "lock_timeout_ms" => config.lock_timeout_ms = parse_count(key, value)?,
        "limits.max_document_bytes" => {
            config.limits.max_document_bytes = parse_limit(key, value)?;
        }
        "limits.max_results" => config.limits.max_results = parse_limit(key, value)?,
        "limits.max_scan_ms" => config.limits.max_scan_ms = parse_limit(key, value)?,
        _ => return Err(unknown_setting(key)),
    }
    Ok(())
}

fn setting_value(config: &StoreConfig, key: &str) -> Result<Value> {
    let mut value = serde_json::to_value(config).unwrap_or_default();
    for part in key.split('.') {
        value = value
            .get(part)
            .cloned()
            .ok_or_else(|| unknown_setting(key))?;
    }
    Ok(value)
}

fn setting_output(key: Option<&str>, value: Value) -> Output {
    let text = match &value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    };
    let pretty = serde_json::to_string_pretty(&value).unwrap_or_default();
    let human = if key.is_some() { text } else { pretty };
    Output::text(json!({ "key": key, "value": value }), human)
}

pub fn init(context: &Context, args: &InitArgs) -> Result<Output> {
    let defaults = StoreConfig::default();
    let config = StoreConfig {
        git: args.git,
        commit: match args.commit {
            Some(CommitArg::Each) | None => defaults.commit,
            Some(CommitArg::Manual) => CommitMode::Manual,
        },
        push: match args.push {
            Some(PushArg::OnCommit) => PushMode::OnCommit,
            Some(PushArg::Off) | None => defaults.push,
        },
        ..defaults
    };
    let store = Store::init_with(&context.store, config)?;
    if let Some(url) = &args.remote {
        store.set_remote(url)?;
    }
    let settings = read_settings(&store)?;
    Ok(Output::text(
        json!({
            "git": settings.git,
            "commit": serde_json::to_value(settings.commit).unwrap_or_default(),
            "push": serde_json::to_value(settings.push).unwrap_or_default(),
            "remote": args.remote,
        }),
        format!("store created in {}", context.store.display()),
    ))
}

pub fn config_get(store: &Store, args: &ConfigGetArgs) -> Result<Output> {
    let settings = read_settings(store)?;
    let value = match &args.key {
        Some(key) => setting_value(&settings, key)?,
        None => serde_json::to_value(&settings).unwrap_or_default(),
    };
    Ok(setting_output(args.key.as_deref(), value))
}

fn commit_setting(store: &Store, context: &Context, key: &str) -> Result<()> {
    let settings = read_settings(store)?;
    if settings.git && settings.commit == CommitMode::Each {
        store.commit(
            Some(&format!("meta store: set {key}")),
            context.actor.as_deref(),
        )?;
    }
    Ok(())
}

fn write_setting(store: &Store, context: &Context, key: &str, value: &str) -> Result<()> {
    let lock = WriteLock::acquire(
        store.root(),
        Duration::from_millis(store.config().lock_timeout_ms),
    )?;
    let before = read_settings(store)?;
    let mut config = before.clone();
    apply_setting(&mut config, key, value)?;
    if config == before {
        return Ok(());
    }
    write_atomic(&store.root().join(STORE_FILE), &to_file_bytes(&config))?;
    drop(lock);
    commit_setting(store, context, key)
}

pub fn config_set(context: &Context, args: &ConfigSetArgs) -> Result<Output> {
    let store = context.open()?;
    match args.key.as_str() {
        "commit" => store.set_commit_mode(match args.value.as_str() {
            "each" => CommitMode::Each,
            "manual" => CommitMode::Manual,
            _ => return Err(invalid_value("commit", "each or manual")),
        })?,
        "push" => store.set_push(match args.value.as_str() {
            "off" => PushMode::Off,
            "on-commit" => PushMode::OnCommit,
            _ => return Err(invalid_value("push", "off or on-commit")),
        })?,
        key => write_setting(&store, context, key, &args.value)?,
    }
    let value = setting_value(&read_settings(&store)?, &args.key)?;
    Ok(setting_output(Some(&args.key), value))
}

fn collection_ack(args: &CollectionArgs, extra: Value) -> Value {
    let mut value = json!({ "db": args.db, "collection": args.coll });
    if let (Some(target), Value::Object(extra)) = (value.as_object_mut(), extra) {
        target.extend(extra);
    }
    value
}

pub fn create_db(store: &Store, args: &DbArgs) -> Result<Output> {
    store.create_db(&args.db)?;
    Ok(Output::text(
        json!({ "db": args.db }),
        format!("created {}", args.db),
    ))
}

pub fn drop_db(store: &Store, args: &DbArgs) -> Result<Output> {
    store.drop_db(&args.db)?;
    Ok(Output::text(
        json!({ "db": args.db }),
        format!("dropped {}", args.db),
    ))
}

pub fn create_collection(store: &Store, args: &CollectionCreateArgs) -> Result<Output> {
    let format = Format::from(args.format);
    store.create_collection(&args.db, &args.coll, format)?;
    Ok(Output::text(
        json!({ "db": args.db, "collection": args.coll, "format": format.extension() }),
        format!("created {}/{}", args.db, args.coll),
    ))
}

pub fn drop_collection(store: &Store, args: &CollectionArgs) -> Result<Output> {
    store.drop_collection(&args.db, &args.coll)?;
    Ok(Output::text(
        json!({ "db": args.db, "collection": args.coll }),
        format!("dropped {}/{}", args.db, args.coll),
    ))
}

pub fn set_schema(store: &Store, args: &SchemaSetArgs) -> Result<Output> {
    let schema = match &args.file {
        Some(path) => {
            let text = fs::read_to_string(path)?;
            Some(parse_json(ErrorCode::InvalidDocument, "the schema", &text)?)
        }
        None => None,
    };
    let has_schema = schema.is_some();
    store.set_schema(&args.db, &args.coll, schema)?;
    let collection = CollectionArgs {
        db: args.db.clone(),
        coll: args.coll.clone(),
    };
    Ok(Output::text(
        collection_ack(&collection, json!({ "has_schema": has_schema })),
        if has_schema {
            "schema set"
        } else {
            "schema cleared"
        },
    ))
}

fn index_spec(args: &IndexAddArgs) -> IndexSpec {
    match (&args.field, &args.text) {
        (Some(field), _) => IndexSpec::Field {
            field: field.clone(),
            unique: args.unique,
            multi: args.multi,
        },
        (None, text) => IndexSpec::Text {
            text: text.clone().unwrap_or_default(),
        },
    }
}

fn same_target(spec: &IndexSpec, wanted: &IndexSpec) -> bool {
    match (spec, wanted) {
        (IndexSpec::Field { field: left, .. }, IndexSpec::Field { field: right, .. }) => {
            left == right
        }
        (IndexSpec::Text { .. }, IndexSpec::Text { .. }) => true,
        _ => false,
    }
}

fn index_ack(args: &CollectionArgs, indexes: &[IndexSpec], message: &str) -> Output {
    Output::text(
        collection_ack(
            args,
            json!({ "indexes": serde_json::to_value(indexes).unwrap_or_default() }),
        ),
        message,
    )
}

pub fn add_index(store: &Store, args: &IndexAddArgs) -> Result<Output> {
    let wanted = index_spec(args);
    let mut indexes = store.collection_meta(&args.db, &args.coll)?.indexes;
    if indexes.iter().any(|spec| same_target(spec, &wanted)) {
        return Err(
            Error::new(ErrorCode::AlreadyExists, "the index already exists")
                .with_detail("kind", "index")
                .with_detail("reason", "exists"),
        );
    }
    indexes.push(wanted);
    store.set_indexes(&args.db, &args.coll, indexes.clone())?;
    let collection = CollectionArgs {
        db: args.db.clone(),
        coll: args.coll.clone(),
    };
    Ok(index_ack(&collection, &indexes, "index added"))
}

pub fn drop_index(store: &Store, args: &IndexDropArgs) -> Result<Output> {
    let mut indexes = store.collection_meta(&args.db, &args.coll)?.indexes;
    let before = indexes.len();
    indexes.retain(|spec| match spec {
        IndexSpec::Text { .. } => args.target != "text",
        IndexSpec::Field { field, .. } => *field != args.target,
    });
    if indexes.len() == before {
        return Err(Error::not_found("index", &args.target));
    }
    store.set_indexes(&args.db, &args.coll, indexes.clone())?;
    let collection = CollectionArgs {
        db: args.db.clone(),
        coll: args.coll.clone(),
    };
    Ok(index_ack(&collection, &indexes, "index dropped"))
}

pub fn validate(store: &Store) -> Result<Output> {
    store.rescan()?;
    let status = store.status()?;
    let invalid = invalid_entries(store.root(), &status.invalid);
    let lines = status
        .invalid
        .iter()
        .map(|file| {
            format!(
                "{}/{}/{}: {}",
                file.db, file.collection, file.id, file.reason
            )
        })
        .collect();
    Ok(Output::lines(
        json!({
            "valid": invalid.is_empty(),
            "documents": status.documents,
            "invalid": invalid,
        }),
        lines,
    ))
}

fn scan_output(store: &Store) -> Result<Output> {
    let status = store.status()?;
    Ok(Output::text(
        json!({ "documents": status.documents, "invalid_files": status.invalid.len() }),
        format!(
            "{} documents, {} invalid files",
            status.documents,
            status.invalid.len()
        ),
    ))
}

pub fn rescan(store: &Store) -> Result<Output> {
    store.rescan()?;
    scan_output(store)
}

pub fn reindex(context: &Context) -> Result<Output> {
    cache::discard(&context.store);
    scan_output(&context.open()?)
}
