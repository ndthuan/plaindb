use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::Path;

use serde_json::Value;

fn collect(dir: &Path, out: &mut BTreeMap<String, f64>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let estimates = path.join("new").join("estimates.json");
        if let Ok(bytes) = fs::read(&estimates) {
            let value: Value = serde_json::from_slice(&bytes).expect("estimates");
            let mean = value["mean"]["point_estimate"].as_f64().expect("mean");
            let name = path
                .strip_prefix(env::args().nth(1).expect("criterion dir"))
                .expect("under the criterion dir")
                .to_string_lossy()
                .into_owned();
            out.insert(name, mean);
        } else {
            collect(&path, out);
        }
    }
}

fn format_time(nanos: f64) -> String {
    if nanos >= 1e9 {
        format!("{:.2} s", nanos / 1e9)
    } else if nanos >= 1e6 {
        format!("{:.1} ms", nanos / 1e6)
    } else if nanos >= 1e3 {
        format!("{:.1} us", nanos / 1e3)
    } else {
        format!("{nanos:.0} ns")
    }
}

fn main() {
    let root = env::args().nth(1).expect("criterion dir");
    let mut results = BTreeMap::new();
    collect(Path::new(&root), &mut results);
    println!("| benchmark | mean |");
    println!("| --- | --- |");
    for (name, nanos) in results {
        println!("| {name} | {} |", format_time(nanos));
    }
}
