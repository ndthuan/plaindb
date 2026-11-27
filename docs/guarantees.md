# Guarantees


Each line below is backed by tests named in [`crates/plaindb-core/tests/guarantees.rs`](../crates/plaindb-core/tests/guarantees.rs), and a test fails when this list and that table disagree.

- A transaction applies fully or not at all: every operation is checked before any file changes, and a transaction with a failing operation leaves every file as it was. <!-- g:atomic-tx -->
- A crash never leaves half a transaction. A process killed at any point either left the store untouched, or its transaction is finished the next time the store is opened, and with git on its commit is made exactly once. <!-- g:crash -->
- Several processes may write one store. Writes take one lock and queue behind it, a transaction waits `lock_timeout_ms` (5 seconds by default) before failing with `store_locked_timeout`, and no write is lost. <!-- g:multi-process -->
- Each read sees every transaction whole or not at all, also across processes. A read holds a shared lock on `.plaindb/lock` while it scans, waits at most `lock_timeout_ms` for a running write, and on a store whose lock file cannot be opened checks that nothing moved while it scanned. <!-- g:isolation -->
- A hand edit made while no write is in progress is never overwritten. A write that would replace a file changed since plaindb read it fails with `version_conflict` and the reason `changed_on_disk`, and the file stays as it is. After plaindb restarts on the same boot, a hand edit or a hand delete made since a committed transaction is kept as well. After the machine restarts, a committed write at `full` survives a power loss: within the checkpoint window (the transactions a process that did not close cleanly wrote since its last checkpoint), a file that is missing, or that holds exactly the bytes it had before one of those transactions, is written again from the journal, and a file edited by hand to other content is kept. The one exception is a transaction that was cut short: finishing it writes its own files again. <!-- g:hand-edit -->
- A file plaindb wrote reads back unchanged: a document in each format decodes to the value that was encoded, a Markdown body survives byte for byte, an attached file reads back byte for byte, and a clone of the remote opens as the same store with the same versions and the same attached files. <!-- g:round-trip -->
- A committed write at `full` survives a power loss on Linux. A test runs a store on LazyFS, a filesystem layer that drops on command the file data not yet flushed: in each of 20 rounds a writer making 200 inserts is killed at a random point and the unflushed data is dropped, and every insert reported as committed is then present, the store opens with no invalid file and no warning, and the next write succeeds. LazyFS keeps every directory change (a create, a rename, a remove) as soon as it is made, so a lost rename is tested only by simulating a reboot after removing files. On macOS power loss is tested at the process-crash level only. <!-- g:power-loss -->
- With git on, a document can be read as of any commit or time with `get --at`, and `history` lists the commits that changed it. <!-- g:time-travel -->
- plaindb never force pushes. A push the remote refuses because it holds commits the store lacks, or because its history is unrelated, changes nothing on the remote. <!-- g:no-force-push -->

A write is durable once it returns. When a step after the commit point fails (writing the files, making the git commit, or recording the new generation), the write still succeeds and its result carries `deferred` with that step and the error, for example `"deferred":{"step":"git","message":"..."}`. The command exits 0 and prints a warning on standard error. A missing git commit is made by the next write or open, in order and once, and `status` shows `git_commit_pending` until then; files that could not be written are finished the next time the store is opened, and opening fails with the reason `recovery_failed` until they can be.

A transaction id given with `--tx-id` is remembered for the last 1,000 transactions; repeating an older one writes again.

A read command changes nothing in the store but the index cache under `.plaindb`, and needs no write access to the directory.

These guarantees were tested on APFS (macOS) and ext4 (Linux). Other filesystems, network mounts and exFAT included, are untested.

What plaindb does not promise: durability beyond the level chosen in [Durability](durability.md), and anything about files changed while a write is running.
