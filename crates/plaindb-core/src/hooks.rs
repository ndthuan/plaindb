#[cfg(feature = "test-hooks")]
mod enabled {
    use std::time::Duration;

    const CRASH_STATUS: i32 = 86;

    fn crash() -> ! {
        std::process::exit(CRASH_STATUS)
    }

    fn var(name: &str) -> Option<String> {
        std::env::var(name).ok().filter(|value| !value.is_empty())
    }

    pub(crate) fn before_journal() {
        if var("PLAINDB_TEST_CRASH_BEFORE_JOURNAL").as_deref() == Some("1") {
            crash();
        }
    }

    pub(crate) fn after_journal() {
        if let Some(ms) = var("PLAINDB_TEST_HOLD_LOCK_MS").and_then(|text| text.parse().ok()) {
            std::thread::sleep(Duration::from_millis(ms));
        }
    }

    pub(crate) fn after_files(applied: usize) {
        if var("PLAINDB_TEST_CRASH_AFTER").and_then(|text| text.parse::<usize>().ok())
            == Some(applied)
        {
            crash();
        }
    }

    pub(crate) fn before_git_commit() {
        if var("PLAINDB_TEST_CRASH_BEFORE_COMMIT").as_deref() == Some("1") {
            crash();
        }
    }

    pub(crate) fn after_git_commit() {
        if var("PLAINDB_TEST_CRASH_AFTER_COMMIT").as_deref() == Some("1") {
            crash();
        }
    }
}

#[cfg(not(feature = "test-hooks"))]
mod enabled {
    pub(crate) fn before_journal() {}

    pub(crate) fn after_journal() {}

    pub(crate) fn after_files(_applied: usize) {}

    pub(crate) fn before_git_commit() {}

    pub(crate) fn after_git_commit() {}
}

pub(crate) use enabled::{
    after_files, after_git_commit, after_journal, before_git_commit, before_journal,
};
