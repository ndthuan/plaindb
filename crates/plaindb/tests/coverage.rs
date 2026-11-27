use std::collections::BTreeSet;
use std::process::Command as StdCommand;

use plaindb_core::ops::OPERATIONS;
use rmcp::ServiceExt;
use rmcp::transport::TokioChildProcess;
use tokio::process::Command;

struct Coverage {
    operation: &'static str,
    commands: &'static [&'static str],
    tools: &'static [&'static str],
}

const fn covered(
    operation: &'static str,
    commands: &'static [&'static str],
    tools: &'static [&'static str],
) -> Coverage {
    Coverage {
        operation,
        commands,
        tools,
    }
}

const TABLE: &[Coverage] = &[
    covered("get", &["get"], &["plaindb_get"]),
    covered("get_at", &["get"], &["plaindb_get"]),
    covered("find", &["find"], &["plaindb_find"]),
    covered("count", &["count"], &["plaindb_count"]),
    covered("distinct", &["distinct"], &["plaindb_distinct"]),
    covered("history", &["history"], &["plaindb_history"]),
    covered("status", &["status"], &["plaindb_status"]),
    covered("doctor", &["doctor"], &["plaindb_doctor"]),
    covered("list_dbs", &["db list"], &["plaindb_list_dbs"]),
    covered(
        "list_collections",
        &["collection list"],
        &["plaindb_list_collections"],
    ),
    covered("insert", &["insert"], &["plaindb_insert"]),
    covered("put", &["put"], &["plaindb_put"]),
    covered("patch", &["patch"], &["plaindb_patch"]),
    covered("delete", &["delete"], &["plaindb_delete"]),
    covered("tx", &["tx"], &["plaindb_tx"]),
    covered("put_file", &["file put"], &["plaindb_put_file"]),
    covered("get_file", &["file get"], &["plaindb_get_file"]),
    covered("list_files", &["file list"], &["plaindb_get"]),
    covered("delete_file", &["file rm"], &["plaindb_delete_file"]),
    covered("create_db", &["db create"], &["plaindb_create_db"]),
    covered("drop_db", &["db drop"], &["plaindb_drop_db"]),
    covered(
        "create_collection",
        &["collection create"],
        &["plaindb_create_collection"],
    ),
    covered(
        "drop_collection",
        &["collection drop"],
        &["plaindb_drop_collection"],
    ),
    covered(
        "set_schema",
        &["collection schema set"],
        &["plaindb_set_schema"],
    ),
    covered(
        "get_schema",
        &["collection schema get"],
        &["plaindb_get_schema"],
    ),
    covered(
        "set_indexes",
        &["index add", "index drop"],
        &["plaindb_add_index", "plaindb_drop_index"],
    ),
    covered("list_indexes", &["index list"], &["plaindb_list_indexes"]),
    covered("enable_git", &["git enable"], &[]),
    covered("set_remote", &["git remote set"], &[]),
    covered("commit", &["commit"], &["plaindb_commit"]),
    covered("push", &["push"], &["plaindb_push"]),
    covered("validate", &["validate"], &[]),
    covered("rescan", &["rescan"], &[]),
    covered("reindex", &["reindex"], &[]),
    covered("import_jsonl", &["import"], &[]),
    covered("import_dir", &["import"], &[]),
    covered("export_jsonl", &["export"], &[]),
];

fn listed_tools() -> BTreeSet<String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a runtime");
    runtime.block_on(async {
        let dir = tempfile::tempdir().expect("a temporary directory");
        let initialised = StdCommand::new(env!("CARGO_BIN_EXE_plaindb"))
            .arg("--store")
            .arg(dir.path())
            .arg("init")
            .output()
            .expect("the binary runs");
        assert!(initialised.status.success());
        let mut command = Command::new(env!("CARGO_BIN_EXE_plaindb"));
        command
            .arg("--store")
            .arg(dir.path())
            .arg("mcp")
            .arg("--allow-drop");
        let session =
            ().serve(TokioChildProcess::new(command).expect("the server starts"))
                .await
                .expect("the handshake succeeds");
        let names = session
            .list_all_tools()
            .await
            .expect("tools are listed")
            .into_iter()
            .map(|tool| tool.name.to_string())
            .collect();
        session.cancel().await.expect("the server stops");
        names
    })
}

fn help_succeeds(command: &str) -> bool {
    StdCommand::new(env!("CARGO_BIN_EXE_plaindb"))
        .args(command.split_whitespace())
        .arg("--help")
        .output()
        .expect("the binary runs")
        .status
        .success()
}

#[test]
fn every_operation_has_an_entry_and_every_entry_an_operation() {
    let tabled: Vec<&str> = TABLE.iter().map(|entry| entry.operation).collect();
    let tabled_set: BTreeSet<&str> = tabled.iter().copied().collect();
    let known: BTreeSet<&str> = OPERATIONS.iter().copied().collect();
    assert_eq!(
        tabled.len(),
        tabled_set.len(),
        "an operation is tabled twice"
    );
    assert_eq!(
        known.difference(&tabled_set).collect::<Vec<_>>(),
        Vec::<&&str>::new(),
        "operations without an entry"
    );
    assert_eq!(
        tabled_set.difference(&known).collect::<Vec<_>>(),
        Vec::<&&str>::new(),
        "entries of unknown operations"
    );
}

#[test]
fn every_operation_has_a_command() {
    for entry in TABLE {
        assert!(
            !entry.commands.is_empty(),
            "{} has no command",
            entry.operation
        );
        for command in entry.commands {
            assert!(
                help_succeeds(command),
                "{}: no command {command}",
                entry.operation
            );
        }
    }
}

#[test]
fn every_operation_has_a_tool_unless_it_is_for_people_only() {
    let listed = listed_tools();
    let mapped: BTreeSet<String> = TABLE
        .iter()
        .flat_map(|entry| entry.tools.iter().map(|tool| (*tool).to_owned()))
        .collect();
    for entry in TABLE {
        for tool in entry.tools {
            assert!(
                listed.contains(*tool),
                "{}: no tool {tool}",
                entry.operation
            );
        }
    }
    assert_eq!(
        listed.difference(&mapped).collect::<Vec<_>>(),
        Vec::<&String>::new(),
        "tools without an operation"
    );
}
