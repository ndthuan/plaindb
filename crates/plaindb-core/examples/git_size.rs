use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::Instant;

use plaindb_core::{CommitMode, Doc, Format, Op, Store, Tx};
use serde_json::{Value, json};

const DEFAULT_TRANSACTIONS: u64 = 10_000;
const DOCUMENTS: u64 = 1_000;
const MANUAL_BATCH: u64 = 100;

fn doc(value: Value) -> Doc {
    match value {
        Value::Object(map) => map,
        _ => Doc::new(),
    }
}

fn directory_bytes(dir: &Path) -> u64 {
    fs::read_dir(dir)
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .map(|entry| match entry.file_type() {
                    Ok(kind) if kind.is_dir() => directory_bytes(&entry.path()),
                    Ok(_) => entry.metadata().map_or(0, |meta| meta.len()),
                    Err(_) => 0,
                })
                .sum()
        })
        .unwrap_or(0)
}

fn cpu_name() -> String {
    let probes: [(&str, &[&str]); 2] = [
        ("sysctl", &["-n", "machdep.cpu.brand_string"]),
        (
            "sh",
            &["-c", "grep -m1 'model name' /proc/cpuinfo | cut -d: -f2"],
        ),
    ];
    probes
        .into_iter()
        .filter_map(|(program, args)| Command::new(program).args(args).output().ok())
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        .find(|name| !name.is_empty())
        .unwrap_or_else(|| "unknown cpu".to_owned())
}

fn write_one(store: &Store, n: u64) {
    let slot = n % DOCUMENTS;
    let op = Op::Put {
        db: "main".to_owned(),
        coll: "items".to_owned(),
        id: format!("d{slot}"),
        doc: doc(json!({
            "n": n,
            "title": format!("Item {slot}"),
            "tags": ["alpha", "beta", "gamma"],
            "text": format!("Revision {n} of a short note kept for the size benchmark."),
        })),
        if_version: None,
    };
    store.apply(Tx::new(vec![op])).expect("write succeeds");
}

fn run(mode: CommitMode, transactions: u64) {
    let dir = tempfile::tempdir().expect("temp dir");
    let root = dir.path().join("store");
    let store = Store::init_with_git(&root).expect("store with git");
    store.create_db("main").expect("db");
    store
        .create_collection("main", "items", Format::Json)
        .expect("collection");
    store.set_commit_mode(mode).expect("mode");
    let started = Instant::now();
    for n in 0..transactions {
        write_one(&store, n);
        if mode == CommitMode::Manual && (n + 1) % MANUAL_BATCH == 0 {
            store.commit(None, None).expect("commit");
        }
    }
    if mode == CommitMode::Manual {
        store.commit(None, None).expect("final commit");
    }
    let elapsed = started.elapsed();
    let git_dir = root.join(".git");
    let before = directory_bytes(&git_dir);
    let gc = Command::new("git")
        .arg("-C")
        .arg(&root)
        .args(["gc", "--aggressive", "--quiet"])
        .status()
        .expect("git gc runs");
    assert!(gc.success());
    let after = directory_bytes(&git_dir);
    let label = match mode {
        CommitMode::Each => "each",
        CommitMode::Manual => "manual",
    };
    println!(
        "mode={label} transactions={transactions} git_bytes_before_gc={before} git_bytes_after_gc={after} store_bytes={} seconds={:.0}",
        directory_bytes(&root)
            - directory_bytes(&root.join(".git"))
            - directory_bytes(&root.join(".plaindb")),
        elapsed.as_secs_f64()
    );
}

fn main() {
    let transactions = std::env::args()
        .nth(1)
        .and_then(|text| text.parse().ok())
        .unwrap_or(DEFAULT_TRANSACTIONS);
    println!("cpu={}", cpu_name());
    run(CommitMode::Each, transactions);
    run(CommitMode::Manual, transactions);
}
