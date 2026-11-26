use std::collections::HashMap;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread::sleep;
use std::time::Duration;

use plaindb_core::{
    At, DEFAULT_HISTORY_LIMIT, Doc, ErrorCode, Format, Op, Store, Timestamp, Tx, version_of,
};
use plaindb_core::{Durability, StoreConfig};
use serde_json::{Value, json};

fn doc(value: Value) -> Doc {
    match value {
        Value::Object(map) => map,
        other => panic!("not an object: {other}"),
    }
}

fn git(root: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

fn put(id: &str, value: Value) -> Op {
    Op::Put {
        db: "main".to_owned(),
        coll: "notes".to_owned(),
        id: id.to_owned(),
        doc: doc(value),
        if_version: None,
    }
}

fn git_store(root: &Path) -> Store {
    let store = Store::init_with(
        root,
        StoreConfig {
            git: true,
            durability: Durability::Off,
            ..StoreConfig::default()
        },
    )
    .unwrap();
    store.create_db("main").unwrap();
    store
        .create_collection("main", "notes", Format::Md)
        .unwrap();
    store
}

struct Written {
    commit: String,
    seconds: i64,
    version: String,
}

fn write_version(store: &Store, root: &Path, n: u64) -> Written {
    let result = store
        .apply(Tx {
            actor: Some("sample-agent".to_owned()),
            ..Tx::new(vec![put(
                "a",
                json!({"n": n, "body": format!("Version {n}\n")}),
            )])
        })
        .unwrap();
    Written {
        commit: git(root, &["rev-parse", "HEAD"]).trim().to_owned(),
        seconds: git(root, &["log", "-1", "--format=%ct"])
            .trim()
            .parse()
            .unwrap(),
        version: result.results[0].version.clone().unwrap(),
    }
}

fn three_versions(store: &Store, root: &Path) -> Vec<Written> {
    let mut written = Vec::new();
    for n in 1..=3 {
        if n > 1 {
            sleep(Duration::from_millis(1_100));
        }
        written.push(write_version(store, root, n));
    }
    written
}

fn at_second(seconds: i64) -> At {
    At::Time(Timestamp::from_second(seconds).unwrap())
}

#[test]
fn a_document_is_read_at_each_commit_and_time() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let store = git_store(root);
    let written = three_versions(&store, root);
    for (index, entry) in written.iter().enumerate() {
        let n = json!(index + 1);
        let (found, version) = store
            .get_at("main", "notes", "a", At::Commit(entry.commit.clone()))
            .unwrap()
            .unwrap();
        assert_eq!(found["n"], n);
        assert_eq!(found["_id"], json!("a"));
        assert_eq!(found["body"], json!(format!("Version {}\n", index + 1)));
        assert_eq!(version, entry.version);
        let (short, _) = store
            .get_at(
                "main",
                "notes",
                "a",
                At::Commit(entry.commit[..7].to_owned()),
            )
            .unwrap()
            .unwrap();
        assert_eq!(short["n"], n);
        let (timed, version) = store
            .get_at("main", "notes", "a", at_second(entry.seconds))
            .unwrap()
            .unwrap();
        assert_eq!(timed["n"], n);
        assert_eq!(version, entry.version);
    }
    let (latest, _) = store
        .get_at("main", "notes", "a", at_second(written[2].seconds + 3_600))
        .unwrap()
        .unwrap();
    assert_eq!(latest["n"], json!(3));
    match store.get_at("main", "notes", "a", at_second(written[0].seconds - 1)) {
        Ok(None) => {}
        Err(error) if error.code == ErrorCode::NotFound => {}
        other => panic!("a time before the first version found {other:?}"),
    }
    let error = store
        .get_at("main", "notes", "a", at_second(1))
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::NotFound);
    assert!(
        store
            .get_at(
                "main",
                "notes",
                "missing",
                At::Commit(written[0].commit.clone())
            )
            .unwrap()
            .is_none()
    );
}

#[test]
fn history_lists_changes_newest_first_and_ends_with_the_delete() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let store = git_store(root);
    store
        .apply(Tx::new(vec![put("other", json!({"n": 0}))]))
        .unwrap();
    let written = three_versions(&store, root);
    store
        .apply(Tx::new(vec![put("other", json!({"n": 1}))]))
        .unwrap();
    let deleted = store
        .apply(Tx {
            actor: Some("sample-agent".to_owned()),
            ..Tx::new(vec![Op::Delete {
                db: "main".to_owned(),
                coll: "notes".to_owned(),
                id: "a".to_owned(),
                if_version: None,
            }])
        })
        .unwrap();
    let history = store
        .history("main", "notes", "a", DEFAULT_HISTORY_LIMIT)
        .unwrap();
    assert_eq!(history.len(), 4);
    assert_eq!(history[0].version, None);
    assert_eq!(history[0].tx_ids, [deleted.tx_id]);
    assert_eq!(history[0].commit, git(root, &["rev-parse", "HEAD"]).trim());
    for (entry, expected) in history[1..].iter().zip(written.iter().rev()) {
        assert_eq!(entry.commit, expected.commit);
        assert_eq!(entry.version.as_deref(), Some(expected.version.as_str()));
        assert_eq!(entry.time.as_second(), expected.seconds);
        assert_eq!(entry.actor.as_deref(), Some("sample-agent"));
        assert_eq!(entry.tx_ids.len(), 1);
    }
    let limited = store.history("main", "notes", "a", 2).unwrap();
    assert_eq!(limited, history[..2]);
}

fn hash_object(root: &Path, contents: &str) -> String {
    let mut child = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["hash-object", "-w", "--stdin"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(contents.as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

fn colliding_contents() -> (String, String, String) {
    let mut seen: HashMap<String, String> = HashMap::new();
    for n in 0u64.. {
        let contents = format!("probe {n}\n");
        let prefix = version_of(contents.as_bytes())[..7].to_owned();
        if let Some(previous) = seen.insert(prefix.clone(), contents.clone()) {
            return (prefix, previous, contents);
        }
    }
    unreachable!()
}

#[test]
fn an_ambiguous_or_wrong_commit_id_is_an_invalid_query() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let store = git_store(root);
    store
        .apply(Tx::new(vec![put("a", json!({"n": 1}))]))
        .unwrap();
    let (prefix, first, second) = colliding_contents();
    let blob = hash_object(root, &first);
    hash_object(root, &second);
    let ambiguous = store
        .get_at("main", "notes", "a", At::Commit(prefix))
        .unwrap_err();
    assert_eq!(ambiguous.code, ErrorCode::InvalidQuery);
    assert_eq!(ambiguous.details["reason"], json!("ambiguous"));
    let not_a_commit = store
        .get_at("main", "notes", "a", At::Commit(blob))
        .unwrap_err();
    assert_eq!(not_a_commit.code, ErrorCode::InvalidQuery);
    let too_short = store
        .get_at("main", "notes", "a", At::Commit("abc12".to_owned()))
        .unwrap_err();
    assert_eq!(too_short.code, ErrorCode::InvalidQuery);
    let unknown = store
        .get_at("main", "notes", "a", At::Commit("0".repeat(40)))
        .unwrap_err();
    assert_eq!(unknown.code, ErrorCode::NotFound);
}

#[test]
fn time_travel_needs_git() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::init_with(
        dir.path(),
        StoreConfig {
            durability: Durability::Off,
            ..StoreConfig::default()
        },
    )
    .unwrap();
    store.create_db("main").unwrap();
    store
        .create_collection("main", "notes", Format::Md)
        .unwrap();
    let error = store
        .get_at("main", "notes", "a", At::Commit("abcdef0".to_owned()))
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::GitDisabled);
    let error = store
        .history("main", "notes", "a", DEFAULT_HISTORY_LIMIT)
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::GitDisabled);
}
