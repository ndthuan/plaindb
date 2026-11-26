use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use plaindb_core::{Doc, ErrorCode, Op, Result, Store, Tx, new_id};
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

const SOAK_COLLECTIONS: [&str; 3] = ["docs", "cards", "notes"];
const SOAK_IDS: u64 = 200;

struct Random(u64);

impl Random {
    fn seeded() -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_nanos());
        Self((nanos as u64) ^ (u64::from(std::process::id()) << 32) | 1)
    }

    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, bound: u64) -> u64 {
        self.next() % bound
    }
}

fn soak_target(random: &mut Random) -> (&'static str, String) {
    let index = random.below(SOAK_IDS);
    let collection = SOAK_COLLECTIONS[usize::try_from(index % 3).unwrap_or(0)];
    (collection, format!("d{index}"))
}

fn soak_doc(random: &mut Random, collection: &str, tx_id: &str) -> Doc {
    let tags: Vec<&str> = ["a", "b"]
        .into_iter()
        .take(usize::try_from(random.below(3)).unwrap_or(0))
        .collect();
    let mut fields = json!({
        "tx": tx_id,
        "size": random.below(1_000),
        "tags": tags,
        "nested": { "x": random.below(10), "y": "text" },
    });
    if collection == "notes" {
        fields["body"] = json!(format!(
            "# Note {}\n\nWritten by {tx_id}.\n",
            random.below(100)
        ));
    }
    doc(fields)
}

fn soak_op(store: &Store, random: &mut Random, tx_id: &str) -> (String, Op) {
    let (collection, id) = soak_target(random);
    let if_version = if random.below(2) == 0 {
        store
            .get(DB, collection, &id)
            .ok()
            .flatten()
            .map(|(_, version)| version)
    } else {
        None
    };
    let db = DB.to_owned();
    let coll = collection.to_owned();
    let op = match random.below(4) {
        0 => Op::Insert {
            db,
            coll,
            id: Some(id),
            doc: soak_doc(random, collection, tx_id),
        },
        1 => Op::Put {
            db,
            coll,
            id,
            doc: soak_doc(random, collection, tx_id),
            if_version,
        },
        2 => Op::Patch {
            db,
            coll,
            id,
            patch: json!({ "tx": tx_id, "patched": random.below(100), "nested": { "y": null } }),
            if_version,
        },
        _ => Op::Delete {
            db,
            coll,
            id,
            if_version,
        },
    };
    (collection.to_owned(), op)
}

fn log_line(log: &mut File, line: &Value) -> Result<()> {
    log.write_all(format!("{line}\n").as_bytes())?;
    Ok(())
}

fn run_soak(store: &Store, count: u64, log_path: &str) -> Result<()> {
    let torn = fs::read(log_path).is_ok_and(|bytes| !bytes.is_empty() && !bytes.ends_with(b"\n"));
    let mut log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)?;
    if torn {
        log.write_all(b"\n")?;
    }
    let mut random = Random::seeded();
    for _ in 0..count {
        let tx_id = new_id();
        let mut collections = Vec::new();
        let mut ops = Vec::new();
        for _ in 0..=random.below(3) {
            let (collection, op) = soak_op(store, &mut random, &tx_id);
            collections.push(collection);
            ops.push(op);
        }
        log_line(
            &mut log,
            &json!({ "begin": tx_id, "collections": collections }),
        )?;
        let outcome = store.apply(Tx {
            id: Some(tx_id.clone()),
            ..Tx::new(ops)
        });
        match outcome {
            Ok(result) => {
                log_line(&mut log, &json!({ "commit": tx_id, "result": result }))?;
            }
            Err(error)
                if matches!(
                    error.code,
                    ErrorCode::AlreadyExists
                        | ErrorCode::NotFound
                        | ErrorCode::VersionConflict
                        | ErrorCode::StoreLockedTimeout
                ) =>
            {
                log_line(&mut log, &json!({ "failed": tx_id }))?;
            }
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!(
            "usage: plaindb-test-writer <store> <insert|patch|tx|crash|open|soak|push> [count] [log]"
        );
        return ExitCode::from(2);
    }
    let count: u64 = args.get(3).and_then(|text| text.parse().ok()).unwrap_or(1);
    let outcome = Store::open(&args[1]).and_then(|store| match args[2].as_str() {
        "insert" => run_inserts(&store, count),
        "patch" => run_patches(&store, count),
        "tx" => run_transactions(&store, count),
        "crash" => run_crash(&store),
        "open" => Ok(()),
        "push" => store.push().map(|outcome| println!("{outcome:?}")),
        "soak" => match args.get(4) {
            Some(log_path) => run_soak(&store, count, log_path),
            None => {
                eprintln!("soak needs a log path");
                std::process::exit(2);
            }
        },
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
