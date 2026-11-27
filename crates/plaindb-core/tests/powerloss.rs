#![cfg(feature = "test-hooks")]

use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use plaindb_core::{Doc, Durability, Format, Op, Store, StoreConfig, Tx};
use serde_json::{Value, json};

const WRITER: &str = env!("CARGO_BIN_EXE_plaindb-test-writer");
const ROUNDS: u64 = 20;
const INSERTS: u64 = 200;
const CLEAR_CACHE: &[u8] = b"lazyfs::clear-cache\n";
const CLEARED: &str = "finished::clear-cache";

struct Layer {
    dir: PathBuf,
    faults: PathBuf,
    cleared: BufReader<File>,
}

impl Layer {
    fn from_env() -> Option<Self> {
        let dir = PathBuf::from(std::env::var_os("PLAINDB_POWERLOSS_DIR")?);
        let faults = PathBuf::from(
            std::env::var_os("PLAINDB_POWERLOSS_FIFO").expect("PLAINDB_POWERLOSS_FIFO is set"),
        );
        let cleared = PathBuf::from(
            std::env::var_os("PLAINDB_POWERLOSS_DONE_FIFO")
                .expect("PLAINDB_POWERLOSS_DONE_FIFO is set"),
        );
        let cleared = BufReader::new(File::open(cleared).unwrap());
        Some(Self {
            dir,
            faults,
            cleared,
        })
    }

    fn lose_unsynced_data(&mut self) {
        OpenOptions::new()
            .write(true)
            .open(&self.faults)
            .unwrap()
            .write_all(CLEAR_CACHE)
            .unwrap();
        let mut line = String::new();
        loop {
            line.clear();
            assert!(self.cleared.read_line(&mut line).unwrap() > 0);
            if line.trim() == CLEARED {
                return;
            }
        }
    }
}

fn seed() -> u64 {
    std::env::var("PLAINDB_POWERLOSS_SEED")
        .ok()
        .and_then(|text| text.parse().ok())
        .unwrap_or_else(|| {
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(1, |elapsed| elapsed.as_nanos() as u64)
        })
}

fn fresh_store(root: &Path) {
    if root.exists() {
        fs::remove_dir_all(root).unwrap();
    }
    let store = Store::init_with(
        root,
        StoreConfig {
            durability: Durability::Full,
            ..StoreConfig::default()
        },
    )
    .unwrap();
    store.create_db("main").unwrap();
    store
        .create_collection("main", "items", Format::Json)
        .unwrap();
}

fn committed_before_a_kill(root: &Path, random: &mut fastrand::Rng) -> Vec<u64> {
    let mut command = Command::new(WRITER);
    command
        .arg(root)
        .arg("insert-loop")
        .arg(INSERTS.to_string())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    if std::env::var("PLAINDB_POWERLOSS_SKIP_FLUSH").as_deref() == Ok("1") {
        command.env("PLAINDB_TEST_SKIP_FLUSH", "1");
    }
    let mut child = command.spawn().unwrap();
    let kill_after = random.u64(1..INSERTS);
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    let mut committed = Vec::new();
    for line in lines.by_ref() {
        committed.push(parse_id(&line.unwrap()));
        if committed.len() as u64 >= kill_after {
            break;
        }
    }
    std::thread::sleep(Duration::from_micros(random.u64(0..5_000)));
    child.kill().unwrap();
    child.wait().unwrap();
    committed.extend(lines.map(|line| parse_id(&line.unwrap())));
    committed
}

fn parse_id(line: &str) -> u64 {
    line.strip_prefix('p')
        .and_then(|number| number.parse().ok())
        .unwrap_or_else(|| panic!("unexpected writer line {line:?}"))
}

fn without_system_fields(doc: &Doc) -> Value {
    let mut fields = doc.clone();
    fields.retain(|key, _| !key.starts_with('_'));
    Value::Object(fields)
}

#[test]
#[ignore = "needs a LazyFS mount; run with make powerloss"]
fn committed_inserts_survive_a_power_loss() {
    let Some(mut layer) = Layer::from_env() else {
        println!("skipped: PLAINDB_POWERLOSS_DIR is not set");
        return;
    };
    let seed = seed();
    println!("seed {seed}");
    let mut random = fastrand::Rng::with_seed(seed);
    for round in 0..ROUNDS {
        let root = layer.dir.join(format!("round-{round}"));
        fresh_store(&root);
        let committed = committed_before_a_kill(&root, &mut random);
        layer.lose_unsynced_data();
        let store = Store::open(&root)
            .unwrap_or_else(|error| panic!("round {round}, seed {seed}: {error}"));
        for n in &committed {
            let id = format!("p{n}");
            let found = store
                .get("main", "items", &id)
                .unwrap()
                .unwrap_or_else(|| panic!("round {round}, seed {seed}: {id} is lost"));
            assert_eq!(
                without_system_fields(&found.0),
                json!({ "n": n }),
                "round {round}, seed {seed}: {id}"
            );
        }
        let status = store.status().unwrap();
        assert!(
            status.invalid.is_empty(),
            "round {round}, seed {seed}: invalid files {:?}",
            status.invalid
        );
        assert!(
            status.warnings.is_empty(),
            "round {round}, seed {seed}: warnings {:?}",
            status.warnings
        );
        store
            .apply(Tx::new(vec![Op::Insert {
                db: "main".to_owned(),
                coll: "items".to_owned(),
                id: Some("after".to_owned()),
                doc: Doc::new(),
            }]))
            .unwrap_or_else(|error| panic!("round {round}, seed {seed}: {error}"));
        println!("round {round}: {} committed inserts kept", committed.len());
    }
}
