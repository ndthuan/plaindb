use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::cli::admin::{CollectionCreateArgs, FormatArg, IndexAddArgs, IndexDropArgs};
use crate::cli::git::{CommitArgs, PushArgs};
use crate::cli::read::{
    CollectionArgs, CountArgs, DbArgs, DistinctArgs, FindArgs, GetArgs, HistoryArgs,
};

#[derive(Deserialize, JsonSchema)]
pub struct NoInput {}

#[derive(Deserialize, JsonSchema)]
pub struct DbInput {
    #[schemars(description = "Name of the database")]
    pub db: String,
}

impl From<DbInput> for DbArgs {
    fn from(input: DbInput) -> Self {
        Self { db: input.db }
    }
}

#[derive(Deserialize, JsonSchema)]
pub struct CollectionInput {
    #[schemars(description = "Name of the database")]
    pub db: String,
    #[schemars(description = "Name of the collection")]
    pub collection: String,
}

impl From<CollectionInput> for CollectionArgs {
    fn from(input: CollectionInput) -> Self {
        Self {
            db: input.db,
            coll: input.collection,
        }
    }
}

#[derive(Deserialize, JsonSchema)]
pub struct GetInput {
    pub db: String,
    pub collection: String,
    #[schemars(description = "Id of the document")]
    pub id: String,
    #[schemars(description = "A commit id or an RFC 3339 time to read the document as of")]
    pub at: Option<String>,
}

impl From<GetInput> for GetArgs {
    fn from(input: GetInput) -> Self {
        Self {
            db: input.db,
            coll: input.collection,
            id: input.id,
            at: input.at,
        }
    }
}

fn filter_text(filter: Option<Value>) -> Option<String> {
    filter.map(|value| value.to_string())
}

#[derive(Deserialize, JsonSchema)]
pub struct FindInput {
    pub db: String,
    pub collection: String,
    #[schemars(
        description = "Filter object such as {\"city\": \"Hue\"} or {\"age\": {\"$gt\": 3}}; omit for every document"
    )]
    pub filter: Option<Value>,
    #[schemars(description = "Sort keys, each a field or field:desc")]
    #[serde(default)]
    pub sort: Vec<String>,
    #[schemars(description = "Most documents to return")]
    pub limit: Option<u64>,
    #[schemars(description = "The next_cursor of the previous page")]
    pub cursor: Option<String>,
    #[schemars(description = "Fields to return; omit for whole documents")]
    pub fields: Option<Vec<String>>,
}

impl From<FindInput> for FindArgs {
    fn from(input: FindInput) -> Self {
        Self {
            db: input.db,
            coll: input.collection,
            filter: filter_text(input.filter),
            sort: input.sort,
            limit: input.limit,
            cursor: input.cursor,
            fields: input.fields,
        }
    }
}

#[derive(Deserialize, JsonSchema)]
pub struct CountInput {
    pub db: String,
    pub collection: String,
    #[schemars(description = "Filter object; omit to count every document")]
    pub filter: Option<Value>,
}

impl From<CountInput> for CountArgs {
    fn from(input: CountInput) -> Self {
        Self {
            db: input.db,
            coll: input.collection,
            filter: filter_text(input.filter),
        }
    }
}

#[derive(Deserialize, JsonSchema)]
pub struct DistinctInput {
    pub db: String,
    pub collection: String,
    #[schemars(description = "Field whose distinct values are listed")]
    pub field: String,
    #[schemars(description = "Filter object; omit to look at every document")]
    pub filter: Option<Value>,
}

impl From<DistinctInput> for DistinctArgs {
    fn from(input: DistinctInput) -> Self {
        Self {
            db: input.db,
            coll: input.collection,
            field: input.field,
            filter: filter_text(input.filter),
        }
    }
}

#[derive(Deserialize, JsonSchema)]
pub struct HistoryInput {
    pub db: String,
    pub collection: String,
    pub id: String,
    #[schemars(description = "Most entries to return")]
    pub limit: Option<u32>,
}

impl From<HistoryInput> for HistoryArgs {
    fn from(input: HistoryInput) -> Self {
        Self {
            db: input.db,
            coll: input.collection,
            id: input.id,
            limit: input.limit,
        }
    }
}

#[derive(Deserialize, JsonSchema)]
pub struct InsertInput {
    pub db: String,
    pub collection: String,
    #[schemars(description = "Id of the new document; omit to get a generated one")]
    pub id: Option<String>,
    #[schemars(description = "The document, a JSON object")]
    pub doc: Map<String, Value>,
}

#[derive(Deserialize, JsonSchema)]
pub struct PutInput {
    pub db: String,
    pub collection: String,
    pub id: String,
    #[schemars(description = "The whole new document, a JSON object")]
    pub doc: Map<String, Value>,
    #[schemars(description = "Write only when the document is at this version")]
    pub if_version: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct PatchInput {
    pub db: String,
    pub collection: String,
    pub id: String,
    #[schemars(description = "A JSON merge patch; a null value removes a field")]
    pub patch: Value,
    #[schemars(description = "Write only when the document is at this version")]
    pub if_version: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct DeleteInput {
    pub db: String,
    pub collection: String,
    pub id: String,
    #[schemars(description = "Write only when the document is at this version")]
    pub if_version: Option<String>,
}

#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum TxKind {
    Insert,
    Put,
    Patch,
    Delete,
}

#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TxOp {
    pub op: TxKind,
    pub db: String,
    pub collection: String,
    #[schemars(description = "Needed by put, patch and delete; optional for insert")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[schemars(description = "The document, for insert and put")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub doc: Option<Map<String, Value>>,
    #[schemars(description = "The merge patch, for patch")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub patch: Option<Value>,
    #[schemars(description = "Apply only when the document is at this version")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub if_version: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct TxInput {
    #[schemars(description = "Operations applied together: all of them or none")]
    pub ops: Vec<TxOp>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum FormatInput {
    Json,
    Yaml,
    Md,
}

#[derive(Deserialize, JsonSchema)]
pub struct CreateCollectionInput {
    pub db: String,
    pub collection: String,
    #[schemars(description = "File format of the documents")]
    pub format: FormatInput,
}

impl From<CreateCollectionInput> for CollectionCreateArgs {
    fn from(input: CreateCollectionInput) -> Self {
        Self {
            db: input.db,
            coll: input.collection,
            format: match input.format {
                FormatInput::Json => FormatArg::Json,
                FormatInput::Yaml => FormatArg::Yaml,
                FormatInput::Md => FormatArg::Md,
            },
        }
    }
}

#[derive(Deserialize, JsonSchema)]
pub struct SetSchemaInput {
    pub db: String,
    pub collection: String,
    #[schemars(description = "A JSON Schema every document must satisfy; omit to remove it")]
    pub schema: Option<Value>,
}

#[derive(Deserialize, JsonSchema)]
pub struct AddIndexInput {
    pub db: String,
    pub collection: String,
    #[schemars(description = "Field to index; give this or text")]
    pub field: Option<String>,
    #[schemars(description = "Refuse a second document with the same value")]
    #[serde(default)]
    pub unique: bool,
    #[schemars(description = "Index every element of an array field")]
    #[serde(default)]
    pub multi: bool,
    #[schemars(description = "Fields of a text index; give this or field")]
    pub text: Option<Vec<String>>,
}

impl AddIndexInput {
    pub fn is_well_formed(&self) -> bool {
        self.field.is_some() != self.text.is_some()
            && (self.field.is_some() || !(self.unique || self.multi))
    }
}

impl From<AddIndexInput> for IndexAddArgs {
    fn from(input: AddIndexInput) -> Self {
        Self {
            db: input.db,
            coll: input.collection,
            field: input.field,
            unique: input.unique,
            multi: input.multi,
            text: input.text,
        }
    }
}

#[derive(Deserialize, JsonSchema)]
pub struct DropIndexInput {
    pub db: String,
    pub collection: String,
    #[schemars(description = "The indexed field, or text for the text index")]
    pub target: String,
}

impl From<DropIndexInput> for IndexDropArgs {
    fn from(input: DropIndexInput) -> Self {
        Self {
            db: input.db,
            coll: input.collection,
            target: input.target,
        }
    }
}

#[derive(Deserialize, JsonSchema)]
pub struct CommitInput {
    #[schemars(description = "Message of the commit")]
    pub message: Option<String>,
}

impl From<CommitInput> for CommitArgs {
    fn from(input: CommitInput) -> Self {
        Self {
            message: input.message,
        }
    }
}

#[derive(Deserialize, JsonSchema)]
pub struct PushInput {
    #[schemars(description = "Clear the remote_ahead flag before pushing")]
    #[serde(default)]
    pub reset_remote_ahead: bool,
}

impl From<PushInput> for PushArgs {
    fn from(input: PushInput) -> Self {
        Self {
            reset_remote_ahead: input.reset_remote_ahead,
        }
    }
}
