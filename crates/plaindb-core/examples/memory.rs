use std::env;
use std::fs;
use std::mem::MaybeUninit;
use std::path::Path;
use std::process::Command;

use plaindb_core::cache::CACHE_FILE;
use plaindb_core::testgen::{COLLECTION, DB, build_store, document, id_for};
use plaindb_core::{Durability, Format, Op, Query, Store, Tx};
use serde_json::{Value, json};

const COUNTS: [usize; 3] = [1_000, 10_000, 100_000];
const SEED: u64 = 7;
const QUERIES: usize = 100;
const WRITES: usize = 10_000;
const MIB: f64 = 1024.0 * 1024.0;

fn format_named(name: &str) -> Format {
    match name {
        "json" => Format::Json,
        "yaml" => Format::Yaml,
        "md" => Format::Md,
        other => panic!("unknown format {other}"),
    }
}

fn peak_resident_bytes() -> u64 {
    let mut usage = MaybeUninit::<libc::rusage>::uninit();
    let status = unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) };
    assert_eq!(status, 0, "getrusage failed");
    let peak = u64::try_from(unsafe { usage.assume_init() }.ru_maxrss).expect("positive");
    if cfg!(target_os = "macos") {
        peak
    } else {
        peak * 1024
    }
}

#[cfg(target_os = "macos")]
fn resident_bytes() -> u64 {
    let mut info = MaybeUninit::<libc::proc_taskinfo>::uninit();
    let size = i32::try_from(size_of::<libc::proc_taskinfo>()).expect("fits");
    let written = unsafe {
        libc::proc_pidinfo(
            libc::getpid(),
            libc::PROC_PIDTASKINFO,
            0,
            info.as_mut_ptr().cast(),
            size,
        )
    };
    assert_eq!(written, size, "proc_pidinfo failed");
    unsafe { info.assume_init() }.pti_resident_size
}

#[cfg(not(target_os = "macos"))]
fn resident_bytes() -> u64 {
    let statm = fs::read_to_string("/proc/self/statm").expect("statm");
    let pages: u64 = statm
        .split_whitespace()
        .nth(1)
        .expect("resident field")
        .parse()
        .expect("number");
    let page = u64::try_from(unsafe { libc::sysconf(libc::_SC_PAGESIZE) }).expect("page size");
    pages * page
}

fn query(filter: Value) -> Query {
    Query {
        db: DB.to_owned(),
        coll: COLLECTION.to_owned(),
        filter,
        limit: Some(100),
        ..Query::default()
    }
}

fn run_queries(store: &Store) {
    let filters = [
        json!({"status": "approved"}),
        json!({"email": "user42@example.test"}),
        json!({"age": {"$gte": 30, "$lt": 40}}),
        json!({"tags": {"$in": ["harbour", "ledger"]}}),
        json!({"address.city": {"$prefix": "Ha"}}),
        json!({"score": {"$gt": 50}}),
        json!({"$text": "harbour ledger"}),
        json!({"$or": [{"status": "draft"}, {"active": false}]}),
    ];
    for round in 0..QUERIES {
        let filter = filters[round % filters.len()].clone();
        store.find(query(filter.clone())).expect("find");
        if round % 4 == 0 {
            store.count(DB, COLLECTION, &filter).expect("count");
        }
    }
}

fn rewrite(store: &Store, format: Format, count: usize) {
    store.set_durability(Durability::Off).expect("durability");
    for round in 0..WRITES {
        let index = round % count;
        let mut doc = document(
            format,
            SEED + 1 + u64::try_from(round).expect("fits"),
            index,
        );
        doc.remove("_id");
        store
            .apply(Tx::new(vec![Op::Put {
                db: DB.to_owned(),
                coll: COLLECTION.to_owned(),
                id: id_for(index),
                doc,
                if_version: None,
            }]))
            .expect("write");
    }
}

fn open_cold(root: &Path) {
    let _ = fs::remove_file(root.join(".plaindb").join(CACHE_FILE));
    let store = Store::open(root).expect("open");
    println!("{}", peak_resident_bytes());
    drop(store);
}

fn open_warm(root: &Path, format: Format, count: usize) {
    let store = Store::open(root).expect("open");
    let peak = peak_resident_bytes();
    run_queries(&store);
    let steady = resident_bytes();
    rewrite(&store, format, count);
    run_queries(&store);
    let written = resident_bytes();
    println!("{peak} {steady} {written}");
}

fn document_bytes(root: &Path) -> u64 {
    fs::read_dir(root.join(DB).join(COLLECTION))
        .expect("collection dir")
        .flatten()
        .filter(|entry| !entry.file_name().to_string_lossy().starts_with('_'))
        .map(|entry| entry.metadata().expect("metadata").len())
        .sum()
}

fn child(mode: &str, root: &Path, format: &str, count: usize) -> Vec<u64> {
    let output = Command::new(env::current_exe().expect("exe"))
        .args([mode, &root.to_string_lossy(), format, &count.to_string()])
        .output()
        .expect("child");
    assert!(
        output.status.success(),
        "the measuring process failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout)
        .split_whitespace()
        .map(|value| value.parse().expect("number"))
        .collect()
}

fn mib(bytes: u64) -> String {
    format!("{:.0} MiB", bytes as f64 / MIB)
}

fn per_doc(bytes: u64, count: usize) -> String {
    format!("{:.1} KiB", bytes as f64 / 1024.0 / count as f64)
}

fn main() {
    let args: Vec<String> = env::args().collect();
    if let [_, mode, root, format, count] = args.as_slice()
        && matches!(mode.as_str(), "cold" | "warm")
    {
        let root = Path::new(root);
        let format = format_named(format);
        let count = count.parse().expect("count");
        if mode == "cold" {
            open_cold(root);
        } else {
            open_warm(root, format, count);
        }
        return;
    }
    let counts: Vec<usize> = if args.len() > 1 {
        args[1..]
            .iter()
            .map(|count| count.parse().expect("count"))
            .collect()
    } else {
        COUNTS.to_vec()
    };
    println!(
        "| Documents | Format | On disk | Open peak, cold | Open peak, warm | Steady | Steady per document | After {WRITES} writes |"
    );
    println!("| --- | --- | --- | --- | --- | --- | --- | --- |");
    for count in counts {
        for name in ["json", "yaml", "md"] {
            let dir = tempfile::TempDir::new().expect("temp dir");
            drop(build_store(dir.path(), format_named(name), count, SEED, true).expect("build"));
            let disk = document_bytes(dir.path());
            let cold = child("cold", dir.path(), name, count)[0];
            let warm = child("warm", dir.path(), name, count);
            println!(
                "| {count} | {name} | {} | {} | {} | {} | {} | {} |",
                mib(disk),
                mib(cold),
                mib(warm[0]),
                mib(warm[1]),
                per_doc(warm[1], count),
                mib(warm[2]),
            );
        }
    }
}
