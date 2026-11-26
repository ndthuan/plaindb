use plaindb_core::{CheckStatus, DoctorOptions, Store};

#[test]
fn a_healthy_store_reports_stable_check_ids_and_no_error() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::init(temp.path()).unwrap();
    let report = store.doctor(DoctorOptions { offline: true });
    let ids: Vec<&str> = report.checks.iter().map(|check| check.id).collect();
    for id in [
        "git",
        "store_format",
        "journal",
        "write_lock",
        "documents_invalid",
        "documents_schema",
        "names_case",
        "index_cache",
        "filesystem_network",
        "filesystem_case",
        "durability",
        "disk_space",
    ] {
        assert!(ids.contains(&id), "{id} in {ids:?}");
    }
    assert_eq!(report.errors(), 0);
    assert!(
        report
            .checks
            .iter()
            .all(|check| check.status != CheckStatus::Error)
    );
    assert_eq!(
        report.to_json()["checks"].as_array().unwrap().len(),
        ids.len()
    );
}

#[test]
fn a_held_write_lock_is_reported() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::init(temp.path()).unwrap();
    let held = plaindb_core::WriteLock::try_acquire(temp.path())
        .unwrap()
        .unwrap();
    let report = store.doctor(DoctorOptions::default());
    let check = report
        .checks
        .iter()
        .find(|check| check.id == "write_lock")
        .unwrap();
    assert_eq!(check.status, CheckStatus::Warn);
    drop(held);
    let report = store.doctor(DoctorOptions::default());
    let check = report
        .checks
        .iter()
        .find(|check| check.id == "write_lock")
        .unwrap();
    assert_eq!(check.status, CheckStatus::Ok);
}
