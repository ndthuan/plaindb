use std::env;
use std::mem::MaybeUninit;
use std::path::Path;
use std::process::Command;

use plaindb_core::testgen::{COLLECTION, DB, build_store};
use plaindb_core::{Format, Query, Store};
use serde_json::json;

const COUNT: usize = 10_000;
const SEED: u64 = 7;

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
    let peak = unsafe { usage.assume_init() }.ru_maxrss as u64;
    if cfg!(target_os = "macos") {
        peak
    } else {
        peak * 1024
    }
}

fn measure(root: &Path) {
    let store = Store::open(root).expect("open");
    let found = store
        .find(Query {
            db: DB.to_owned(),
            coll: COLLECTION.to_owned(),
            filter: json!({"status": "approved"}),
            limit: Some(100),
            ..Query::default()
        })
        .expect("find");
    assert!(!found.docs.is_empty());
    println!("{}", peak_resident_bytes());
}

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.get(1).map(String::as_str) == Some("measure") {
        measure(Path::new(&args[2]));
        return;
    }
    for name in ["json", "yaml", "md"] {
        let dir = tempfile::TempDir::new().expect("temp dir");
        drop(build_store(dir.path(), format_named(name), COUNT, SEED, true).expect("build"));
        let output = Command::new(env::current_exe().expect("exe"))
            .arg("measure")
            .arg(dir.path())
            .output()
            .expect("child");
        assert!(output.status.success(), "the measuring process failed");
        let bytes: u64 = String::from_utf8_lossy(&output.stdout)
            .trim()
            .parse()
            .expect("number");
        println!("rss {name} {COUNT} {}", bytes / (1024 * 1024));
    }
}
