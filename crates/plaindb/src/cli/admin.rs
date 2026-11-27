use std::path::PathBuf;

use clap::{Args, ValueEnum};
use plaindb_core::config::{SETTING_KEYS, StoreConfig};
use plaindb_core::{
    CommitMode, Error, ErrorCode, Format, IndexSpec, PushMode, Result, Store, cache,
};
use serde_json::{Map, Value, json};

use super::Context;
use super::output::Output;
use super::read::{CollectionArgs, DbArgs, invalid_entries};
use super::write::read_json;

#[derive(Clone, Copy, ValueEnum)]
pub enum PushArg {
    Off,
    #[value(name = "on-commit")]
    OnCommit,
    Background,
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
    #[arg(
        long,
        conflicts_with_all = ["file", "clear"],
        required_unless_present_any = ["file", "clear"],
        help = "The schema as JSON"
    )]
    pub data: Option<String>,
    #[arg(
        long,
        conflicts_with = "clear",
        required_unless_present_any = ["data", "clear"],
        help = "File with the schema as JSON; - reads standard input"
    )]
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

fn setting_output(key: Option<&str>, value: Value) -> Output {
    let text = match &value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    };
    Output::text(json!({ "key": key, "value": value }), text)
}

fn refuse_git_options_without_git(args: &InitArgs) -> Result<()> {
    if args.git {
        return Ok(());
    }
    let needs_git = [
        (args.remote.is_some(), "--remote"),
        (
            matches!(args.push, Some(PushArg::OnCommit | PushArg::Background)),
            "--push",
        ),
        (matches!(args.commit, Some(CommitArg::Manual)), "--commit"),
    ];
    match needs_git.iter().find(|(given, _)| *given) {
        Some((_, flag)) => Err(Error::new(
            ErrorCode::InvalidConfig,
            format!("{flag} needs --git"),
        )),
        None => Ok(()),
    }
}

pub fn init(context: &Context, args: &InitArgs) -> Result<Output> {
    refuse_git_options_without_git(args)?;
    let defaults = StoreConfig::default();
    let config = StoreConfig {
        git: args.git,
        commit: match args.commit {
            Some(CommitArg::Each) | None => defaults.commit,
            Some(CommitArg::Manual) => CommitMode::Manual,
        },
        push: match args.push {
            Some(PushArg::OnCommit) => PushMode::OnCommit,
            Some(PushArg::Background) => PushMode::Background,
            Some(PushArg::Off) | None => defaults.push,
        },
        ..defaults
    };
    let store = Store::init_with(&context.store, config)?;
    if let Some(url) = &args.remote {
        store.set_remote(url)?;
    }
    let settings = store.config();
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

fn listing(config: &StoreConfig) -> Result<(Value, Vec<String>)> {
    let defaults = StoreConfig::default();
    let mut entries = Map::new();
    let mut lines = Vec::new();
    for key in SETTING_KEYS {
        let value = config.setting(key)?;
        let is_default = value == defaults.setting(key)?;
        lines.push(format!(
            "{key} = {value}{}",
            if is_default { " (default)" } else { "" }
        ));
        entries.insert(
            (*key).to_owned(),
            json!({ "value": value, "default": is_default }),
        );
    }
    Ok((Value::Object(entries), lines))
}

pub fn config_get(store: &Store, args: &ConfigGetArgs) -> Result<Output> {
    let config = store.config();
    match &args.key {
        Some(key) => Ok(setting_output(Some(key), config.setting(key)?)),
        None => {
            let (value, lines) = listing(&config)?;
            Ok(Output::lines(json!({ "key": null, "value": value }), lines))
        }
    }
}

pub fn config_set(context: &Context, args: &ConfigSetArgs) -> Result<Output> {
    let store = context.open()?;
    store.set_setting(&args.key, &args.value)?;
    Ok(setting_output(
        Some(&args.key),
        store.config().setting(&args.key)?,
    ))
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
    let schema = if args.clear {
        None
    } else {
        Some(read_json(
            args.data.as_deref(),
            args.file.as_deref(),
            "the schema",
        )?)
    };
    set_schema_value(store, &args.db, &args.coll, schema)
}

pub fn set_schema_value(
    store: &Store,
    db: &str,
    coll: &str,
    schema: Option<Value>,
) -> Result<Output> {
    let has_schema = schema.is_some();
    store.set_schema(db, coll, schema)?;
    let collection = CollectionArgs {
        db: db.to_owned(),
        coll: coll.to_owned(),
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
