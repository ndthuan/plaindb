#![cfg(feature = "test-hooks")]

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use plaindb_core::{
    Format, Store, TxResult, WriteLock, codec_for, scan::read_generation, version_of,
};
use serde_json::{Value, json};

const WRITER: &str = env!("CARGO_BIN_EXE_plaindb-test-writer");
const CHILDREN: usize = 4;
const TRANSACTIONS: u64 = 1_000;
const KILLS: usize = 50;
const COLLECTIONS: [(&str, Format); 3] = [
    ("docs", Format::Json),
    ("cards", Format::Yaml),
    ("notes", Format::Md),
];

#[derive(Debug, Clone, PartialEq)]
enum Event {
    Committed {
        previous: Option<String>,
        version: Option<String>,
    },
    Edited(String),
}

struct Slot {
    child: Child,
    log: PathBuf,
}

fn spawn(root: &Path, log: &Path, count: u64) -> Child {
    Command::new(WRITER)
        .arg(root)
        .arg("soak")
        .arg(count.to_string())
        .arg(log)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap()
}

fn log_lines(log: &Path) -> Vec<Value> {
    fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

fn begun(log: &Path) -> u64 {
    let text = fs::read_to_string(log).unwrap_or_default();
    u64::try_from(text.matches("\"begin\"").count()).unwrap()
}

fn unfinished(log: &Path) -> Option<(String, Vec<String>)> {
    let lines = log_lines(log);
    let last = lines.last()?;
    let tx_id = last.get("begin")?.as_str()?.to_owned();
    let collections = serde_json::from_value(last["collections"].clone()).unwrap();
    Some((tx_id, collections))
}

fn settle(root: &Path, tx_id: &str) -> Option<TxResult> {
    let state = root.join(".plaindb");
    if let Ok(bytes) = fs::read(state.join("journal.json")) {
        let journal: Value = serde_json::from_slice(&bytes).unwrap();
        if journal["tx_id"] == json!(tx_id) {
            return Some(serde_json::from_value(journal["result"].clone()).unwrap());
        }
    }
    fs::read_to_string(state.join("tx-log.jsonl"))
        .unwrap_or_default()
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .find(|record| record["tx_id"] == json!(tx_id))
        .map(|record| serde_json::from_value(record["result"].clone()).unwrap())
}

struct Random(u64);

impl Random {
    fn below(&mut self, bound: u64) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0 % bound
    }
}

fn hand_edit(
    root: &Path,
    random: &mut Random,
    marker: u64,
) -> Option<(u64, String, String, String)> {
    let _lock = WriteLock::acquire(root, Duration::from_secs(30)).unwrap();
    if root.join(".plaindb").join("journal.json").exists() {
        return None;
    }
    let (collection, format) = COLLECTIONS[usize::try_from(random.below(3)).unwrap()];
    let dir = root.join("main").join(collection);
    let mut names: Vec<String> = fs::read_dir(&dir)
        .unwrap()
        .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
        .filter(|name| !name.starts_with('.') && !name.starts_with('_'))
        .collect();
    if names.is_empty() {
        return None;
    }
    names.sort();
    let name = names[usize::try_from(random.below(names.len() as u64)).unwrap()].clone();
    let path = dir.join(&name);
    let codec = codec_for(format);
    let mut doc = codec.decode(&fs::read(&path).unwrap()).unwrap();
    doc.insert("marker".to_owned(), json!(marker));
    let bytes = codec.encode(&doc).unwrap();
    fs::write(&path, &bytes).unwrap();
    let id = name.rsplit_once('.').unwrap().0.to_owned();
    Some((
        read_generation(root).unwrap(),
        collection.to_owned(),
        id,
        version_of(&bytes),
    ))
}

fn record_commit(
    events: &mut Vec<(u64, u64, String, String, Event)>,
    collections: &[String],
    result: &TxResult,
) {
    for (index, (collection, op)) in collections.iter().zip(&result.results).enumerate() {
        let event = Event::Committed {
            previous: op.previous_version.clone(),
            version: op.version.clone(),
        };
        events.push((
            result.generation,
            u64::try_from(index).unwrap(),
            collection.clone(),
            op.id.clone(),
            event,
        ));
    }
}

#[test]
#[ignore = "long soak"]
fn writers_killed_at_random_lose_no_commit_and_no_hand_edit() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("store");
    let logs = temp.path().join("logs");
    fs::create_dir_all(&logs).unwrap();
    let store = Store::init(&root).unwrap();
    store.create_db("main").unwrap();
    for (collection, format) in COLLECTIONS {
        store.create_collection("main", collection, format).unwrap();
    }
    let mut random = Random(0x9e37_79b9_7f4a_7c15);
    let mut slots: Vec<Slot> = (0..CHILDREN)
        .map(|index| {
            let log = logs.join(format!("writer-{index}.jsonl"));
            Slot {
                child: spawn(&root, &log, TRANSACTIONS),
                log,
            }
        })
        .collect();
    let mut settled: Vec<(Vec<String>, TxResult)> = Vec::new();
    let mut edits: Vec<(u64, String, String, String)> = Vec::new();
    let mut kills = 0;
    let started = Instant::now();
    let total = TRANSACTIONS * CHILDREN as u64;
    let kill_spacing = total * 9 / 10 / KILLS as u64;
    let mut marker = 0;
    let mut last_edit = Instant::now();
    loop {
        std::thread::sleep(Duration::from_millis(5));
        if last_edit.elapsed() >= Duration::from_millis(50) {
            last_edit = Instant::now();
            marker += 1;
            if let Some(edit) = hand_edit(&root, &mut random, marker) {
                edits.push(edit);
            }
        }
        while kills < KILLS
            && slots.iter().map(|slot| begun(&slot.log)).sum::<u64>()
                >= (kills as u64 + 1) * kill_spacing
        {
            let first = usize::try_from(random.below(CHILDREN as u64)).unwrap();
            let running = (0..CHILDREN)
                .map(|offset| (first + offset) % CHILDREN)
                .find(|index| slots[*index].child.try_wait().unwrap().is_none());
            let Some(index) = running else {
                break;
            };
            std::thread::sleep(Duration::from_millis(random.below(20)));
            let slot = &mut slots[index];
            slot.child.kill().unwrap();
            slot.child.wait().unwrap();
            kills += 1;
            if let Some((tx_id, collections)) = unfinished(&slot.log)
                && let Some(result) = settle(&root, &tx_id)
            {
                settled.push((collections, result));
            }
            let remaining = TRANSACTIONS - begun(&slot.log);
            slot.child = spawn(&root, &slot.log, remaining);
        }
        let mut running = false;
        for slot in &mut slots {
            running |= slot.child.try_wait().unwrap().is_none();
        }
        if !running {
            break;
        }
        assert!(started.elapsed() < Duration::from_secs(1_800));
    }
    for slot in slots {
        let output = slot.child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(begun(&slot.log), TRANSACTIONS);
    }
    assert_eq!(kills, KILLS);
    assert!(edits.len() > 100, "only {} hand edits", edits.len());

    let mut events: Vec<(u64, u64, String, String, Event)> = Vec::new();
    let mut begins: HashMap<String, Vec<String>> = HashMap::new();
    for entry in fs::read_dir(&logs).unwrap() {
        for line in log_lines(&entry.unwrap().path()) {
            if let Some(tx_id) = line.get("begin").and_then(Value::as_str) {
                begins.insert(
                    tx_id.to_owned(),
                    serde_json::from_value(line["collections"].clone()).unwrap(),
                );
            }
            if line.get("commit").is_some() {
                let result: TxResult = serde_json::from_value(line["result"].clone()).unwrap();
                record_commit(&mut events, &begins[&result.tx_id], &result);
            }
        }
    }
    for (collections, result) in &settled {
        record_commit(&mut events, collections, result);
    }
    for (sequence, (generation, collection, id, version)) in edits.iter().enumerate() {
        events.push((
            *generation,
            1_000 + u64::try_from(sequence).unwrap(),
            collection.clone(),
            id.clone(),
            Event::Edited(version.clone()),
        ));
    }
    let committed = events
        .iter()
        .filter(|event| !matches!(event.4, Event::Edited(_)))
        .count();
    assert!(committed > 1_000, "only {committed} committed writes");
    events.sort_by_key(|event| (event.0, event.1));
    let mut last: BTreeMap<(String, String), (Option<String>, bool)> = BTreeMap::new();
    for (generation, _, collection, id, event) in events {
        let known = last
            .entry((collection.clone(), id.clone()))
            .or_insert((None, false));
        match event {
            Event::Committed { previous, version } => {
                assert_eq!(
                    previous, known.0,
                    "{collection}/{id} at generation {generation} replaced a version it never saw"
                );
                *known = (version, false);
            }
            Event::Edited(version) => *known = (Some(version), true),
        }
    }

    let reopened = Store::open(&root).unwrap();
    assert!(!root.join(".plaindb").join("journal.json").exists());
    let generation = read_generation(&root).unwrap();
    assert!(generation > 0);
    for (collection, format) in COLLECTIONS {
        let dir = root.join("main").join(collection);
        let codec = codec_for(format);
        for index in 0..200 {
            let id = format!("d{index}");
            let path = dir.join(format!("{id}.{}", format.extension()));
            let on_disk = fs::read(&path).ok();
            let (expected, edited) = last
                .get(&(collection.to_owned(), id.clone()))
                .cloned()
                .unwrap_or((None, false));
            assert_eq!(
                on_disk.as_deref().map(version_of),
                expected,
                "{collection}/{id}"
            );
            if let Some(bytes) = on_disk.filter(|_| !edited) {
                let again = codec.encode(&codec.decode(&bytes).unwrap()).unwrap();
                assert_eq!(again, bytes, "{collection}/{id} does not round-trip");
            }
            if path.is_file() {
                assert!(reopened.get("main", collection, &id).unwrap().is_some());
            }
        }
    }
}
