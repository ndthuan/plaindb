use std::process::ExitCode;

use plaindb_core::{Doc, ErrorCode, Op, Result, Store, Tx};
use serde_json::{Value, json};

const DB: &str = "main";
const COLLECTION: &str = "items";

fn doc(value: Value) -> Doc {
    match value {
        Value::Object(map) => map,
        _ => Doc::new(),
    }
}

fn insert(id: Option<&str>, value: Value) -> Op {
    Op::Insert {
        db: DB.to_owned(),
        coll: COLLECTION.to_owned(),
        id: id.map(str::to_owned),
        doc: doc(value),
    }
}

fn run_inserts(store: &Store, count: u64) -> Result<()> {
    let writer = std::process::id();
    for n in 0..count {
        store.apply(Tx::new(vec![insert(
            None,
            json!({ "writer": writer, "n": n }),
        )]))?;
    }
    Ok(())
}

fn run_patches(store: &Store, count: u64) -> Result<()> {
    let writer = std::process::id();
    for n in 0..count {
        let (_, version) = store
            .get(DB, COLLECTION, "shared")?
            .expect("the shared document exists");
        let outcome = store.apply(Tx::new(vec![Op::Patch {
            db: DB.to_owned(),
            coll: COLLECTION.to_owned(),
            id: "shared".to_owned(),
            patch: json!({ "writer": writer, "n": n }),
            if_version: Some(version),
        }]));
        match outcome {
            Ok(result) => println!(
                "ok {}",
                result.results[0].version.as_deref().unwrap_or_default()
            ),
            Err(error) if error.code == ErrorCode::VersionConflict => println!("conflict"),
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn run_transactions(store: &Store, count: u64) -> Result<()> {
    let writer = std::process::id();
    for n in 0..count {
        let ops = (0..5)
            .map(|part| insert(None, json!({ "writer": writer, "n": n, "part": part })))
            .collect();
        store.apply(Tx::new(ops))?;
    }
    Ok(())
}

fn run_crash(store: &Store) -> Result<()> {
    let ops = vec![
        insert(Some("c1"), json!({ "v": 1 })),
        insert(Some("c2"), json!({ "v": 1 })),
        insert(Some("c3"), json!({ "v": 1 })),
        Op::Put {
            db: DB.to_owned(),
            coll: COLLECTION.to_owned(),
            id: "a".to_owned(),
            doc: doc(json!({ "v": 2 })),
            if_version: None,
        },
        Op::Delete {
            db: DB.to_owned(),
            coll: COLLECTION.to_owned(),
            id: "b".to_owned(),
            if_version: None,
        },
    ];
    let result = store.apply(Tx::new(ops))?;
    println!("{}", result.tx_id);
    Ok(())
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("usage: plaindb-test-writer <store> <insert|patch|tx|crash> [count]");
        return ExitCode::from(2);
    }
    let count: u64 = args.get(3).and_then(|text| text.parse().ok()).unwrap_or(1);
    let outcome = Store::open(&args[1]).and_then(|store| match args[2].as_str() {
        "insert" => run_inserts(&store, count),
        "patch" => run_patches(&store, count),
        "tx" => run_transactions(&store, count),
        "crash" => run_crash(&store),
        other => {
            eprintln!("unknown mode {other}");
            std::process::exit(2);
        }
    });
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error} {}", error.details);
            ExitCode::FAILURE
        }
    }
}
