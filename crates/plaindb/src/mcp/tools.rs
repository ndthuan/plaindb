use std::sync::Arc;

use plaindb_core::{Error, Op};
use rmcp::model::JsonObject;
use schemars::JsonSchema;
use schemars::generate::SchemaSettings;
use serde::de::DeserializeOwned;
use serde_json::Value;

use super::inputs::{
    AddIndexInput, CollectionInput, CommitInput, CountInput, CreateCollectionInput, DbInput,
    DeleteInput, DistinctInput, DoctorInput, DropIndexInput, FindInput, GetInput, HistoryInput,
    InsertInput, NoInput, PatchInput, PushInput, PutInput, SetSchemaInput, TxInput,
};
use crate::cli::output::Output;
use crate::cli::{Context, admin, doctor, git, read, write};

const NOTICE: &str = "Do not touch store files directly.";

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Access {
    Read,
    Write,
    Drop,
}

pub enum Failure {
    Arguments(String),
    Store(Error),
}

impl From<Error> for Failure {
    fn from(error: Error) -> Self {
        Self::Store(error)
    }
}

type Run = Box<dyn Fn(&Context, JsonObject) -> Result<Output, Failure> + Send + Sync>;

pub struct Tool {
    pub name: &'static str,
    pub access: Access,
    pub description: String,
    pub schema: Arc<JsonObject>,
    run: Run,
}

impl Tool {
    pub fn call(&self, context: &Context, arguments: JsonObject) -> Result<Output, Failure> {
        (self.run)(context, arguments)
    }
}

fn input_schema<T: JsonSchema>() -> Arc<JsonObject> {
    let mut settings = SchemaSettings::draft2020_12();
    settings.inline_subschemas = true;
    let schema = settings.into_generator().into_root_schema_for::<T>();
    match serde_json::to_value(schema) {
        Ok(Value::Object(mut object)) => {
            object.remove("title");
            Arc::new(object)
        }
        _ => Arc::new(JsonObject::new()),
    }
}

fn tool<T>(
    access: Access,
    name: &'static str,
    summary: &str,
    run: fn(&Context, T) -> Result<Output, Failure>,
) -> Tool
where
    T: DeserializeOwned + JsonSchema + 'static,
{
    Tool {
        name,
        access,
        description: format!("{summary} {NOTICE}"),
        schema: input_schema::<T>(),
        run: Box::new(move |context, arguments| {
            let input = serde_json::from_value(Value::Object(arguments))
                .map_err(|source| Failure::Arguments(source.to_string()))?;
            run(context, input)
        }),
    }
}

pub fn catalog() -> Vec<Tool> {
    use Access::{Drop, Read, Write};
    vec![
        tool(
            Read,
            "plaindb_status",
            "Shows the state of the store: document counts, invalid files, warnings and git.",
            |context, _: NoInput| Ok(read::status(&context.open()?)?),
        ),
        tool(
            Read,
            "plaindb_doctor",
            "Checks the store and its environment and lists every problem with the command that fixes it. Changes nothing.",
            |context, input: DoctorInput| Ok(doctor::run(context, &input.into())),
        ),
        tool(
            Read,
            "plaindb_list_dbs",
            "Lists the databases of the store.",
            |context, _: NoInput| Ok(read::list_dbs(&context.open()?)?),
        ),
        tool(
            Read,
            "plaindb_list_collections",
            "Lists the collections of a database.",
            |context, input: DbInput| Ok(read::list_collections(&context.open()?, &input.into())?),
        ),
        tool(
            Read,
            "plaindb_get_schema",
            "Returns the JSON Schema of a collection, or null when it has none.",
            |context, input: CollectionInput| {
                Ok(read::get_schema(&context.open()?, &input.into())?)
            },
        ),
        tool(
            Read,
            "plaindb_list_indexes",
            "Lists the indexes of a collection.",
            |context, input: CollectionInput| {
                Ok(read::list_indexes(&context.open()?, &input.into())?)
            },
        ),
        tool(
            Read,
            "plaindb_get",
            "Reads one document with its version, now or as of a commit or time.",
            |context, input: GetInput| Ok(read::get(&context.open()?, &input.into())?),
        ),
        tool(
            Read,
            "plaindb_find",
            "Finds documents matching a filter, with sorting and paging.",
            |context, input: FindInput| Ok(read::find(&context.open()?, &input.into())?),
        ),
        tool(
            Read,
            "plaindb_count",
            "Counts the documents matching a filter.",
            |context, input: CountInput| Ok(read::count(&context.open()?, &input.into())?),
        ),
        tool(
            Read,
            "plaindb_distinct",
            "Lists the distinct values of a field among the documents matching a filter.",
            |context, input: DistinctInput| Ok(read::distinct(&context.open()?, &input.into())?),
        ),
        tool(
            Read,
            "plaindb_history",
            "Lists the commits that changed a document, newest first; needs git.",
            |context, input: HistoryInput| Ok(read::history(&context.open()?, &input.into())?),
        ),
        tool(
            Write,
            "plaindb_insert",
            "Adds a document that does not exist yet and returns its id and version.",
            |context, input: InsertInput| {
                Ok(write::apply(
                    context,
                    vec![Op::Insert {
                        db: input.db,
                        coll: input.collection,
                        id: input.id,
                        doc: input.doc,
                    }],
                )?)
            },
        ),
        tool(
            Write,
            "plaindb_put",
            "Writes a whole document, replacing any earlier one; if_version guards against lost updates.",
            |context, input: PutInput| {
                Ok(write::apply(
                    context,
                    vec![Op::Put {
                        db: input.db,
                        coll: input.collection,
                        id: input.id,
                        doc: input.doc,
                        if_version: input.if_version,
                    }],
                )?)
            },
        ),
        tool(
            Write,
            "plaindb_patch",
            "Changes a document with a JSON merge patch; if_version guards against lost updates.",
            |context, input: PatchInput| {
                Ok(write::apply(
                    context,
                    vec![Op::Patch {
                        db: input.db,
                        coll: input.collection,
                        id: input.id,
                        patch: input.patch,
                        if_version: input.if_version,
                    }],
                )?)
            },
        ),
        tool(
            Write,
            "plaindb_delete",
            "Removes a document; if_version guards against lost updates.",
            |context, input: DeleteInput| {
                Ok(write::apply(
                    context,
                    vec![Op::Delete {
                        db: input.db,
                        coll: input.collection,
                        id: input.id,
                        if_version: input.if_version,
                    }],
                )?)
            },
        ),
        tool(
            Write,
            "plaindb_tx",
            "Applies a list of insert, put, patch and delete operations as one atomic transaction.",
            |context, input: TxInput| {
                let ops = serde_json::to_value(input.ops)
                    .map_err(|source| Failure::Arguments(source.to_string()))?;
                Ok(write::apply(context, write::parse_ops(ops)?)?)
            },
        ),
        tool(
            Write,
            "plaindb_create_db",
            "Creates a database.",
            |context, input: DbInput| Ok(admin::create_db(&context.open()?, &input.into())?),
        ),
        tool(
            Write,
            "plaindb_create_collection",
            "Creates a collection that stores its documents as json, yaml or md files.",
            |context, input: CreateCollectionInput| {
                Ok(admin::create_collection(&context.open()?, &input.into())?)
            },
        ),
        tool(
            Write,
            "plaindb_set_schema",
            "Sets the JSON Schema of a collection, or removes it when none is given.",
            |context, input: SetSchemaInput| {
                Ok(admin::set_schema_value(
                    &context.open()?,
                    &input.db,
                    &input.collection,
                    input.schema,
                )?)
            },
        ),
        tool(
            Write,
            "plaindb_add_index",
            "Adds an index on a field, or a text index over several fields.",
            |context, input: AddIndexInput| {
                if !input.is_well_formed() {
                    return Err(Failure::Arguments(
                        "give either field, with optional unique and multi, or text".to_owned(),
                    ));
                }
                Ok(admin::add_index(&context.open()?, &input.into())?)
            },
        ),
        tool(
            Write,
            "plaindb_drop_index",
            "Drops an index by its field, or the text index by the word text.",
            |context, input: DropIndexInput| {
                Ok(admin::drop_index(&context.open()?, &input.into())?)
            },
        ),
        tool(
            Write,
            "plaindb_commit",
            "Commits the pending changes to git.",
            |context, input: CommitInput| Ok(git::commit(context, &input.into())?),
        ),
        tool(
            Write,
            "plaindb_push",
            "Pushes the commits to the remote.",
            |context, input: PushInput| Ok(git::push(&context.open()?, &input.into())?),
        ),
        tool(
            Drop,
            "plaindb_drop_db",
            "Drops a database with everything in it; this cannot be undone without git.",
            |context, input: DbInput| Ok(admin::drop_db(&context.open()?, &input.into())?),
        ),
        tool(
            Drop,
            "plaindb_drop_collection",
            "Drops a collection with every document in it; this cannot be undone without git.",
            |context, input: CollectionInput| {
                Ok(admin::drop_collection(&context.open()?, &input.into())?)
            },
        ),
    ]
}
