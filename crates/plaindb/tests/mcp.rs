use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;

use rmcp::model::{CallToolRequestParams, JsonObject};
use rmcp::service::RunningService;
use rmcp::transport::TokioChildProcess;
use rmcp::{RoleClient, ServiceExt};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::process::Command;

const READ_TOOLS: [&str; 11] = [
    "plaindb_status",
    "plaindb_doctor",
    "plaindb_list_dbs",
    "plaindb_list_collections",
    "plaindb_get_schema",
    "plaindb_list_indexes",
    "plaindb_get",
    "plaindb_find",
    "plaindb_count",
    "plaindb_distinct",
    "plaindb_history",
];

const WRITE_TOOLS: [&str; 12] = [
    "plaindb_insert",
    "plaindb_put",
    "plaindb_patch",
    "plaindb_delete",
    "plaindb_tx",
    "plaindb_create_db",
    "plaindb_create_collection",
    "plaindb_set_schema",
    "plaindb_add_index",
    "plaindb_drop_index",
    "plaindb_commit",
    "plaindb_push",
];

const DROP_TOOLS: [&str; 2] = ["plaindb_drop_db", "plaindb_drop_collection"];

const INSTRUCTIONS: &str = "plaindb stores documents as files in a store directory. Use these tools for every read and write. Never read, edit, create or delete files inside the store directly: changes made outside the tools may be refused or lost.";

type Session = RunningService<RoleClient, ()>;

struct Reply {
    is_error: bool,
    value: Value,
}

fn schema_validator(relative: &str) -> jsonschema::Validator {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("schemas")
        .join(relative);
    let text = fs::read_to_string(path).expect("the schema exists");
    jsonschema::validator_for(&serde_json::from_str(&text).expect("the schema parses"))
        .expect("the schema compiles")
}

fn assert_matches_schema(kind: &str, value: &Value) {
    let validator = schema_validator(&format!("results/{kind}.schema.json"));
    let errors: Vec<String> = validator
        .iter_errors(value)
        .map(|error| error.to_string())
        .collect();
    assert!(errors.is_empty(), "{errors:?} in {value}");
}

fn cli(store: &Path, args: &[&str]) -> Value {
    let output = StdCommand::new(env!("CARGO_BIN_EXE_plaindb"))
        .arg("--store")
        .arg(store)
        .arg("--json")
        .args(args)
        .output()
        .expect("the binary runs");
    serde_json::from_slice(&output.stdout).expect("stdout is JSON")
}

fn new_store() -> (TempDir, PathBuf) {
    let dir = TempDir::new().expect("a temporary directory");
    let store = dir.path().join("store");
    assert_eq!(cli(&store, &["init"])["ok"], true);
    (dir, store)
}

async fn start(store: &Path, flags: &[&str]) -> Session {
    let mut command = Command::new(env!("CARGO_BIN_EXE_plaindb"));
    command.arg("--store").arg(store).arg("mcp").args(flags);
    ().serve(TokioChildProcess::new(command).expect("the server starts"))
        .await
        .expect("the handshake succeeds")
}

fn object(value: Value) -> JsonObject {
    match value {
        Value::Object(map) => map,
        other => panic!("not an object: {other}"),
    }
}

async fn call(session: &Session, tool: &str, arguments: Value) -> Reply {
    let result = session
        .call_tool(CallToolRequestParams::new(tool.to_owned()).with_arguments(object(arguments)))
        .await
        .expect("the call is answered");
    let text = result
        .content
        .first()
        .and_then(|block| block.as_text())
        .expect("one text block")
        .text
        .clone();
    Reply {
        is_error: result.is_error.unwrap_or(false),
        value: serde_json::from_str(&text).expect("the text is JSON"),
    }
}

async fn succeed(session: &Session, tool: &str, arguments: Value) -> Value {
    let reply = call(session, tool, arguments).await;
    assert!(!reply.is_error, "{tool} failed: {}", reply.value);
    reply.value
}

async fn tool_names(session: &Session) -> Vec<String> {
    let mut names: Vec<String> = session
        .list_all_tools()
        .await
        .expect("tools are listed")
        .into_iter()
        .map(|tool| tool.name.to_string())
        .collect();
    names.sort();
    names
}

fn sorted(groups: &[&[&str]]) -> Vec<String> {
    let mut names: Vec<String> = groups
        .iter()
        .flat_map(|group| group.iter().map(|name| (*name).to_owned()))
        .collect();
    names.sort();
    names
}

async fn prepare(session: &Session) {
    succeed(session, "plaindb_create_db", json!({"db": "app"})).await;
    succeed(
        session,
        "plaindb_create_collection",
        json!({"db": "app", "collection": "notes", "format": "md"}),
    )
    .await;
}

#[tokio::test]
async fn default_server_offers_read_and_write_tools_without_drop() {
    let (_dir, store) = new_store();
    let session = start(&store, &[]).await;
    assert_eq!(
        tool_names(&session).await,
        sorted(&[&READ_TOOLS, &WRITE_TOOLS])
    );
    session.cancel().await.expect("the server stops");
}

#[tokio::test]
async fn allow_drop_adds_the_drop_tools() {
    let (_dir, store) = new_store();
    let session = start(&store, &["--allow-drop"]).await;
    assert_eq!(
        tool_names(&session).await,
        sorted(&[&READ_TOOLS, &WRITE_TOOLS, &DROP_TOOLS])
    );
    prepare(&session).await;
    succeed(
        &session,
        "plaindb_drop_collection",
        json!({"db": "app", "collection": "notes"}),
    )
    .await;
    succeed(&session, "plaindb_drop_db", json!({"db": "app"})).await;
    assert_eq!(
        succeed(&session, "plaindb_list_dbs", json!({})).await,
        json!({"dbs": []})
    );
    session.cancel().await.expect("the server stops");
}

#[tokio::test]
async fn read_only_server_offers_only_read_tools() {
    let (_dir, store) = new_store();
    for flags in [&["--read-only"][..], &["--read-only", "--allow-drop"][..]] {
        let session = start(&store, flags).await;
        assert_eq!(tool_names(&session).await, sorted(&[&READ_TOOLS]));
        let refused = session
            .call_tool(
                CallToolRequestParams::new("plaindb_create_db")
                    .with_arguments(object(json!({"db": "app"}))),
            )
            .await;
        assert!(refused.is_err());
        session.cancel().await.expect("the server stops");
    }
    assert_eq!(cli(&store, &["db", "list"])["result"], json!({"dbs": []}));
}

#[tokio::test]
async fn server_names_itself_and_tells_agents_to_use_the_tools() {
    let (_dir, store) = new_store();
    let session = start(&store, &["--allow-drop"]).await;
    let info = session.peer_info().expect("the server introduces itself");
    let server = info.server_info.as_ref().expect("a server name");
    assert_eq!(server.name, "plaindb");
    assert_eq!(server.version, env!("CARGO_PKG_VERSION"));
    assert_eq!(info.instructions.as_deref(), Some(INSTRUCTIONS));
    for tool in session.list_all_tools().await.expect("tools are listed") {
        let description = tool.description.as_deref().expect("a description");
        assert!(
            description.ends_with("Do not touch store files directly."),
            "{}: {description}",
            tool.name
        );
        assert!(
            description.find('.').is_some_and(|end| end > 10),
            "{}",
            tool.name
        );
        assert_eq!(tool.input_schema["type"], "object", "{}", tool.name);
        assert!(tool.input_schema.get("title").is_none(), "{}", tool.name);
    }
    session.cancel().await.expect("the server stops");
}

#[tokio::test]
async fn insert_then_find_through_tools_matches_the_command_line() {
    let (_dir, store) = new_store();
    let session = start(&store, &[]).await;
    prepare(&session).await;
    let written = succeed(
        &session,
        "plaindb_insert",
        json!({"db": "app", "collection": "notes", "id": "hue", "doc": {"city": "Hue", "rank": 2}}),
    )
    .await;
    assert_matches_schema("write", &written);
    succeed(
        &session,
        "plaindb_insert",
        json!({"db": "app", "collection": "notes", "id": "hanoi", "doc": {"city": "Hanoi", "rank": 1}}),
    )
    .await;

    let found = succeed(
        &session,
        "plaindb_find",
        json!({"db": "app", "collection": "notes", "filter": {"rank": {"$gte": 1}}, "sort": ["rank"]}),
    )
    .await;
    assert_matches_schema("find", &found);
    let ids: Vec<&str> = found["docs"]
        .as_array()
        .expect("a list")
        .iter()
        .map(|entry| entry["doc"]["_id"].as_str().expect("an id"))
        .collect();
    assert_eq!(ids, ["hanoi", "hue"]);
    let from_cli = cli(
        &store,
        &[
            "find",
            "app",
            "notes",
            "--filter",
            r#"{"rank":{"$gte":1}}"#,
            "--sort",
            "rank",
        ],
    );
    assert_eq!(from_cli["result"], found);

    let one = succeed(
        &session,
        "plaindb_get",
        json!({"db": "app", "collection": "notes", "id": "hue"}),
    )
    .await;
    assert_matches_schema("get", &one);
    assert_eq!(cli(&store, &["get", "app", "notes", "hue"])["result"], one);
    let counted = succeed(
        &session,
        "plaindb_count",
        json!({"db": "app", "collection": "notes"}),
    )
    .await;
    assert_eq!(counted, json!({"count": 2}));
    session.cancel().await.expect("the server stops");
}

#[tokio::test]
async fn failures_carry_the_command_line_error_object() {
    let (_dir, store) = new_store();
    let session = start(&store, &[]).await;
    prepare(&session).await;
    let reply = call(
        &session,
        "plaindb_get",
        json!({"db": "app", "collection": "notes", "id": "missing"}),
    )
    .await;
    assert!(reply.is_error);
    assert_eq!(reply.value["code"], "not_found");
    assert_eq!(
        cli(&store, &["get", "app", "notes", "missing"])["error"],
        reply.value
    );

    let written = succeed(
        &session,
        "plaindb_insert",
        json!({"db": "app", "collection": "notes", "id": "a", "doc": {"n": 1}}),
    )
    .await;
    let stale = call(
        &session,
        "plaindb_patch",
        json!({"db": "app", "collection": "notes", "id": "a", "patch": {"n": 2}, "if_version": "0000000000000000000000000000000000000000"}),
    )
    .await;
    assert!(stale.is_error);
    assert_eq!(stale.value["code"], "version_conflict");
    let fresh = succeed(
        &session,
        "plaindb_patch",
        json!({"db": "app", "collection": "notes", "id": "a", "patch": {"n": 2}, "if_version": written["results"][0]["version"]}),
    )
    .await;
    assert_eq!(
        fresh["results"][0]["previous_version"],
        written["results"][0]["version"]
    );
    session.cancel().await.expect("the server stops");
}

#[tokio::test]
async fn malformed_arguments_are_refused() {
    let (_dir, store) = new_store();
    let session = start(&store, &[]).await;
    let missing = session
        .call_tool(
            CallToolRequestParams::new("plaindb_get").with_arguments(object(json!({"db": "app"}))),
        )
        .await;
    assert!(missing.is_err());
    let both = session
        .call_tool(
            CallToolRequestParams::new("plaindb_add_index").with_arguments(object(
                json!({"db": "app", "collection": "notes", "field": "a", "text": ["b"]}),
            )),
        )
        .await;
    assert!(both.is_err());
    session.cancel().await.expect("the server stops");
}

#[tokio::test]
async fn transaction_applies_every_operation_or_none() {
    let (_dir, store) = new_store();
    let session = start(&store, &[]).await;
    prepare(&session).await;
    let done = succeed(
        &session,
        "plaindb_tx",
        json!({"ops": [
            {"op": "insert", "db": "app", "collection": "notes", "id": "a", "doc": {"n": 1}},
            {"op": "put", "db": "app", "collection": "notes", "id": "b", "doc": {"n": 2}},
        ]}),
    )
    .await;
    assert_eq!(done["results"].as_array().expect("a list").len(), 2);
    let failed = call(
        &session,
        "plaindb_tx",
        json!({"ops": [
            {"op": "delete", "db": "app", "collection": "notes", "id": "a"},
            {"op": "patch", "db": "app", "collection": "notes", "id": "absent", "patch": {"n": 3}},
        ]}),
    )
    .await;
    assert!(failed.is_error);
    assert_eq!(
        succeed(
            &session,
            "plaindb_count",
            json!({"db": "app", "collection": "notes"})
        )
        .await,
        json!({"count": 2})
    );
    session.cancel().await.expect("the server stops");
}

#[tokio::test]
async fn administration_tools_change_schema_and_indexes() {
    let (_dir, store) = new_store();
    let session = start(&store, &[]).await;
    prepare(&session).await;
    let schema = json!({"type": "object", "required": ["city"]});
    succeed(
        &session,
        "plaindb_set_schema",
        json!({"db": "app", "collection": "notes", "schema": schema}),
    )
    .await;
    assert_eq!(
        succeed(
            &session,
            "plaindb_get_schema",
            json!({"db": "app", "collection": "notes"})
        )
        .await,
        json!({"schema": schema})
    );
    let rejected = call(
        &session,
        "plaindb_insert",
        json!({"db": "app", "collection": "notes", "doc": {"rank": 1}}),
    )
    .await;
    assert!(rejected.is_error);
    assert_eq!(rejected.value["code"], "schema_violation");
    succeed(
        &session,
        "plaindb_add_index",
        json!({"db": "app", "collection": "notes", "field": "city", "unique": true}),
    )
    .await;
    let listed = succeed(
        &session,
        "plaindb_list_indexes",
        json!({"db": "app", "collection": "notes"}),
    )
    .await;
    assert_eq!(listed["indexes"].as_array().expect("a list").len(), 1);
    succeed(
        &session,
        "plaindb_drop_index",
        json!({"db": "app", "collection": "notes", "target": "city"}),
    )
    .await;
    session.cancel().await.expect("the server stops");
}

#[tokio::test]
async fn two_servers_write_to_one_store_without_loss() {
    let (_dir, store) = new_store();
    let first = start(&store, &[]).await;
    let second = start(&store, &[]).await;
    prepare(&first).await;

    async fn insert_all(session: &Session, prefix: &str) {
        for number in 0..15 {
            succeed(
                session,
                "plaindb_insert",
                json!({"db": "app", "collection": "notes", "id": format!("{prefix}-{number}"), "doc": {"n": number}}),
            )
            .await;
        }
    }
    tokio::join!(insert_all(&first, "a"), insert_all(&second, "b"));

    for session in [&first, &second] {
        assert_eq!(
            succeed(
                session,
                "plaindb_count",
                json!({"db": "app", "collection": "notes"})
            )
            .await,
            json!({"count": 30})
        );
    }
    assert_eq!(
        cli(&store, &["validate"])["ok"],
        true,
        "the store stays valid"
    );
    first.cancel().await.expect("the server stops");
    second.cancel().await.expect("the server stops");
}

fn without_disk(mut report: Value) -> Value {
    if let Some(checks) = report["checks"].as_array_mut() {
        checks.retain(|check| check["id"] != "disk_space");
    }
    report
}

#[tokio::test]
async fn doctor_is_offered_read_only_and_matches_the_command_line() {
    let (_dir, store) = new_store();
    let session = start(&store, &["--read-only"]).await;
    let reported = succeed(&session, "plaindb_doctor", json!({"offline": true})).await;
    assert_matches_schema("doctor", &reported);
    let direct = cli(&store, &["doctor", "--offline"])["result"].clone();
    assert_eq!(without_disk(reported), without_disk(direct));
    session.cancel().await.expect("the server stops");
}

async fn refused_message(session: &Session, tool: &str, arguments: Value) -> String {
    let error = session
        .call_tool(CallToolRequestParams::new(tool.to_owned()).with_arguments(object(arguments)))
        .await
        .expect_err("the call is refused");
    let text = format!("{error:?}");
    assert!(text.contains("-32602"), "{tool}: {text}");
    text
}

#[tokio::test]
async fn an_unknown_argument_is_refused_by_name_and_changes_nothing() {
    let (_dir, store) = new_store();
    let session = start(&store, &[]).await;
    prepare(&session).await;
    let written = succeed(
        &session,
        "plaindb_insert",
        json!({"db": "app", "collection": "notes", "id": "a", "doc": {"n": 1}}),
    )
    .await;
    let version = written["results"][0]["version"].clone();
    let message = refused_message(
        &session,
        "plaindb_put",
        json!({"db": "app", "collection": "notes", "id": "a", "doc": {"n": 2}, "ifVersion": version}),
    )
    .await;
    assert!(message.contains("ifVersion"), "{message}");
    let message = refused_message(
        &session,
        "plaindb_delete",
        json!({"db": "app", "collection": "notes", "id": "a", "version": version}),
    )
    .await;
    assert!(message.contains("version"), "{message}");
    let message = refused_message(
        &session,
        "plaindb_count",
        json!({"db": "app", "collection": "notes", "filters": {"n": 1}}),
    )
    .await;
    assert!(message.contains("filters"), "{message}");
    let read = succeed(
        &session,
        "plaindb_get",
        json!({"db": "app", "collection": "notes", "id": "a"}),
    )
    .await;
    assert_eq!(read["doc"]["n"], 1, "{read}");
    assert_eq!(read["version"], version, "{read}");
    session.cancel().await.expect("the server stops");
}

#[tokio::test]
async fn every_tool_schema_refuses_unknown_arguments() {
    let (_dir, store) = new_store();
    let session = start(&store, &["--allow-drop"]).await;
    let tools = session.list_all_tools().await.expect("tools are listed");
    assert!(!tools.is_empty());
    for tool in tools {
        assert_eq!(
            tool.input_schema.get("additionalProperties"),
            Some(&Value::Bool(false)),
            "{}",
            tool.name
        );
    }
    session.cancel().await.expect("the server stops");
}

#[tokio::test]
async fn push_takes_no_argument_that_clears_the_remote_ahead_mark() {
    let (_dir, store) = new_store();
    let session = start(&store, &[]).await;
    let message = refused_message(
        &session,
        "plaindb_push",
        json!({"reset_remote_ahead": true}),
    )
    .await;
    assert!(message.contains("reset_remote_ahead"), "{message}");
    session.cancel().await.expect("the server stops");
}

#[tokio::test]
async fn status_hides_the_credentials_of_an_https_remote() {
    let dir = TempDir::new().expect("a temporary directory");
    let store = dir.path().join("store");
    assert_eq!(
        cli(
            &store,
            &[
                "init",
                "--git",
                "--remote",
                "https://user:s3cret@example.com/r.git"
            ]
        )["ok"],
        true
    );
    let session = start(&store, &[]).await;
    let status = succeed(&session, "plaindb_status", json!({})).await;
    assert_eq!(
        status["git"]["remote_url"], "https://***@example.com/r.git",
        "{status}"
    );
    assert!(!status.to_string().contains("s3cret"), "{status}");
    let doctor = succeed(&session, "plaindb_doctor", json!({"offline": true})).await;
    assert!(!doctor.to_string().contains("s3cret"), "{doctor}");
    let cli_status = cli(&store, &["status"]);
    assert_eq!(
        cli_status["result"]["git"]["remote_url"],
        "https://***@example.com/r.git"
    );
    assert!(!cli_status.to_string().contains("s3cret"));
    session.cancel().await.expect("the server stops");
}

fn set_tree_writable(path: &Path, writable: bool) {
    use std::os::unix::fs::PermissionsExt;
    let metadata = fs::symlink_metadata(path).expect("the path exists");
    if metadata.file_type().is_symlink() {
        return;
    }
    let mut permissions = metadata.permissions();
    let mode = permissions.mode();
    permissions.set_mode(if writable {
        mode | 0o200
    } else {
        mode & !0o222
    });
    if writable {
        fs::set_permissions(path, permissions.clone()).expect("permissions change");
    }
    if metadata.is_dir() {
        for entry in fs::read_dir(path).expect("the directory reads") {
            set_tree_writable(&entry.expect("an entry").path(), writable);
        }
    }
    if !writable {
        fs::set_permissions(path, permissions).expect("permissions change");
    }
}

fn recording_git(dir: &Path) -> (PathBuf, PathBuf) {
    use std::os::unix::fs::PermissionsExt;
    let real = StdCommand::new("sh")
        .args(["-c", "command -v git"])
        .output()
        .expect("sh runs");
    let real = String::from_utf8(real.stdout)
        .expect("text")
        .trim()
        .to_owned();
    let log = dir.join("git-calls.log");
    let script = dir.join("recording-git");
    fs::write(
        &script,
        format!(
            "#!/bin/sh\necho \"$*\" >> '{}'\nexec '{}' \"$@\"\n",
            log.display(),
            real
        ),
    )
    .expect("the script is written");
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).expect("the script runs");
    (script, log)
}

#[tokio::test]
async fn a_read_only_server_answers_on_a_read_only_store_without_the_network() {
    let dir = TempDir::new().expect("a temporary directory");
    let store = dir.path().join("store");
    assert_eq!(cli(&store, &["init", "--git"])["ok"], true);
    assert_eq!(cli(&store, &["db", "create", "app"])["ok"], true);
    assert_eq!(
        cli(
            &store,
            &["collection", "create", "app", "notes", "--format", "json"]
        )["ok"],
        true
    );
    assert_eq!(
        cli(
            &store,
            &["insert", "app", "notes", "a", "--data", r#"{"n":1}"#]
        )["ok"],
        true
    );
    let remote = dir.path().join("remote.git");
    let remote_url = remote.to_string_lossy().into_owned();
    assert_eq!(
        cli(&store, &["git", "remote", "set", &remote_url])["ok"],
        true
    );
    let (git, log) = recording_git(dir.path());
    set_tree_writable(&store, false);
    if fs::write(store.join("probe"), b"x").is_ok() {
        set_tree_writable(&store, true);
        return;
    }
    let mut command = Command::new(env!("CARGO_BIN_EXE_plaindb"));
    command
        .arg("--store")
        .arg(&store)
        .arg("mcp")
        .arg("--read-only")
        .env("PLAINDB_GIT", &git);
    let session =
        ().serve(TokioChildProcess::new(command).expect("the server starts"))
            .await
            .expect("the handshake succeeds");
    let found = succeed(
        &session,
        "plaindb_find",
        json!({"db": "app", "collection": "notes"}),
    )
    .await;
    assert_eq!(found["docs"].as_array().map(Vec::len), Some(1), "{found}");
    succeed(&session, "plaindb_doctor", json!({})).await;
    session.cancel().await.expect("the server stops");
    set_tree_writable(&store, true);
    let calls = fs::read_to_string(&log).unwrap_or_default();
    assert!(
        !calls.contains("ls-remote") && !calls.contains("fetch"),
        "{calls}"
    );
}
