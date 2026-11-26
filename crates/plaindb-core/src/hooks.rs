#[cfg(feature = "test-hooks")]
mod enabled {
    use std::cell::Cell;
    use std::time::Duration;

    thread_local! {
        static FULL_FLUSHES: Cell<u64> = const { Cell::new(0) };
    }

    pub(crate) fn full_flush() {
        FULL_FLUSHES.with(|count| count.set(count.get() + 1));
    }

    pub fn full_flush_count() -> u64 {
        FULL_FLUSHES.with(Cell::get)
    }

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

    pub(crate) fn boot_id() -> Option<String> {
        var("PLAINDB_TEST_BOOT_ID")
    }

    pub(crate) fn during_checkpoint(step: usize) {
        if var("PLAINDB_TEST_CRASH_IN_CHECKPOINT").and_then(|text| text.parse::<usize>().ok())
            == Some(step)
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
    pub(crate) fn full_flush() {}

    pub(crate) fn before_journal() {}

    pub(crate) fn after_journal() {}

    pub(crate) fn after_files(_applied: usize) {}

    pub(crate) fn boot_id() -> Option<String> {
        None
    }

    pub(crate) fn during_checkpoint(_step: usize) {}

    pub(crate) fn before_git_commit() {}

    pub(crate) fn after_git_commit() {}
}

#[cfg(feature = "test-hooks")]
pub use enabled::full_flush_count;
pub(crate) use enabled::{
    after_files, after_git_commit, after_journal, before_git_commit, before_journal, boot_id,
    during_checkpoint, full_flush,
};
