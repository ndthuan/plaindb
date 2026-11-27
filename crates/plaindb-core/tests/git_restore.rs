use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use plaindb_core::{
    DEFAULT_HISTORY_LIMIT, Doc, Format, Op, PushMode, PushOutcome, Store, Tx, codec_for,
};
use plaindb_core::{Durability, StoreConfig};
use serde_json::{Value, json};

const COLLECTIONS: [(&str, Format); 3] = [
    ("docs", Format::Json),
    ("cards", Format::Yaml),
    ("notes", Format::Md),
];
const TRANSACTIONS: u64 = 200;
const IDS: u64 = 40;

fn doc(value: Value) -> Doc {
    match value {
        Value::Object(map) => map,
        other => panic!("not an object: {other}"),
    }
}

fn git(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "user.name=Sample Writer",
            "-c",
            "user.email=writer@example.com",
        ])
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

fn body(format: Format, n: u64) -> Value {
    match format {
        Format::Md => {
            json!({ "n": n, "tags": ["a", "b"], "body": format!("# Note {n}\n\nText {n}.\n") })
        }
        _ => json!({ "n": n, "nested": { "x": n % 7, "y": "text" } }),
    }
}

fn run_transactions(store: &Store) {
    let mut present: BTreeSet<(usize, u64)> = BTreeSet::new();
    for n in 0..TRANSACTIONS {
        let index = usize::try_from(n % 3).unwrap();
        let (collection, format) = COLLECTIONS[index];
        let slot = n % IDS;
        let id = format!("d{slot}");
        let exists = present.contains(&(index, slot));
        let db = "main".to_owned();
        let coll = collection.to_owned();
        let op = if exists && n % 11 == 0 {
            present.remove(&(index, slot));
            Op::Delete {
                db,
                coll,
                id,
                if_version: None,
            }
        } else if exists && n % 5 == 0 {
            Op::Patch {
                db,
                coll,
                id,
                patch: json!({ "n": n, "patched": true }),
                if_version: None,
            }
        } else {
            present.insert((index, slot));
            Op::Put {
                db,
                coll,
                id,
                doc: doc(body(format, n)),
                if_version: None,
            }
        };
        let result = store.apply(Tx::new(vec![op])).unwrap();
        assert_eq!(result.push, Some(PushOutcome::Ok));
    }
}

fn document_files(root: &Path, collection: &str, format: Format) -> Vec<PathBuf> {
    let dir = root.join("main").join(collection);
    let mut files: Vec<PathBuf> = fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == format.extension())
                && !path.file_name().unwrap().to_string_lossy().starts_with('_')
        })
        .collect();
    files.sort();
    files
}

#[test]
fn a_clone_of_the_remote_opens_as_the_same_store() {
    let dir = tempfile::tempdir().unwrap();
    let remote = dir.path().join("remote.git");
    fs::create_dir_all(&remote).unwrap();
    git(&remote, &["init", "-q", "--bare", "-b", "main"]);
    let root = dir.path().join("store");
    let original = Store::init_with(
        &root,
        StoreConfig {
            git: true,
            durability: Durability::Off,
            ..StoreConfig::default()
        },
    )
    .unwrap();
    original.create_db("main").unwrap();
    for (collection, format) in COLLECTIONS {
        original
            .create_collection("main", collection, format)
            .unwrap();
    }
    original.set_remote(remote.to_str().unwrap()).unwrap();
    original.set_push(PushMode::OnCommit).unwrap();
    run_transactions(&original);
    assert_eq!(original.status().unwrap().git.unpushed, 0);

    let copy = dir.path().join("copy");
    git(
        dir.path(),
        &[
            "clone",
            "-q",
            remote.to_str().unwrap(),
            copy.to_str().unwrap(),
        ],
    );
    let restored = Store::open(&copy).unwrap();
    let status = restored.status().unwrap();
    assert!(status.warnings.is_empty());
    assert_eq!(status.git.head, original.status().unwrap().git.head);

    let mut compared = 0;
    for (collection, format) in COLLECTIONS {
        let expected = original.list_ids("main", collection).unwrap();
        assert_eq!(restored.list_ids("main", collection).unwrap(), expected);
        let originals = document_files(&root, collection, format);
        assert_eq!(
            document_files(&copy, collection, format).len(),
            originals.len()
        );
        for path in originals {
            let name = path.file_name().unwrap();
            assert_eq!(
                fs::read(copy.join("main").join(collection).join(name)).unwrap(),
                fs::read(&path).unwrap()
            );
        }
        let codec = codec_for(format);
        for id in &expected {
            let before = original.get("main", collection, id).unwrap().unwrap();
            let after = restored.get("main", collection, id).unwrap().unwrap();
            assert_eq!(before, after);
            let bytes = fs::read(
                copy.join("main")
                    .join(collection)
                    .join(format!("{id}.{}", format.extension())),
            )
            .unwrap();
            assert_eq!(codec.decode(&bytes).unwrap(), after.0);
            compared += 1;
        }
    }
    assert!(compared > 20);

    for (collection, id) in [("docs", "d0"), ("cards", "d1"), ("notes", "d2")] {
        let before = original
            .history("main", collection, id, DEFAULT_HISTORY_LIMIT)
            .unwrap();
        assert!(!before.is_empty());
        let after = restored
            .history("main", collection, id, DEFAULT_HISTORY_LIMIT)
            .unwrap();
        assert_eq!(before, after);
    }
}

#[test]
fn a_clone_of_the_remote_holds_the_same_attached_files() {
    let dir = tempfile::tempdir().unwrap();
    let remote = dir.path().join("remote.git");
    fs::create_dir_all(&remote).unwrap();
    git(&remote, &["init", "-q", "--bare", "-b", "main"]);
    let root = dir.path().join("store");
    let original = Store::init_with(
        &root,
        StoreConfig {
            git: true,
            durability: Durability::Off,
            ..StoreConfig::default()
        },
    )
    .unwrap();
    original.create_db("main").unwrap();
    for (collection, format) in COLLECTIONS {
        original
            .create_collection("main", collection, format)
            .unwrap();
    }
    original.set_remote(remote.to_str().unwrap()).unwrap();
    original.set_push(PushMode::OnCommit).unwrap();
    let photo: Vec<u8> = (0..4096_u32).map(|n| (n * 7 % 251) as u8).collect();
    for (collection, _) in COLLECTIONS {
        let result = original
            .apply(Tx::new(vec![
                Op::Insert {
                    db: "main".to_owned(),
                    coll: collection.to_owned(),
                    id: Some("d1".to_owned()),
                    doc: doc(json!({ "title": collection })),
                },
                Op::AttachFile {
                    db: "main".to_owned(),
                    coll: collection.to_owned(),
                    id: "d1".to_owned(),
                    name: "photo.png".to_owned(),
                    bytes: photo.clone(),
                    mime: None,
                    if_version: None,
                },
            ]))
            .unwrap();
        assert_eq!(result.push, Some(PushOutcome::Ok));
    }

    let copy = dir.path().join("copy");
    git(
        dir.path(),
        &[
            "clone",
            "-q",
            remote.to_str().unwrap(),
            copy.to_str().unwrap(),
        ],
    );
    let restored = Store::open(&copy).unwrap();
    assert!(restored.status().unwrap().warnings.is_empty());
    for (collection, _) in COLLECTIONS {
        let (before, _) = original.get("main", collection, "d1").unwrap().unwrap();
        let (after, _) = restored.get("main", collection, "d1").unwrap().unwrap();
        assert_eq!(after["_files"], before["_files"]);
        let (meta, bytes) = restored
            .read_file("main", collection, "d1", "photo.png")
            .unwrap();
        assert_eq!(bytes, photo);
        assert_eq!(meta.size, 4096);
    }
}
