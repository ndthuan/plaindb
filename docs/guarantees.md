# Guarantees


Each line below is backed by tests named in [`crates/plaindb-core/tests/guarantees.rs`](../crates/plaindb-core/tests/guarantees.rs), and a test fails when this list and that table disagree.

- A transaction applies fully or not at all: every operation is checked before any file changes, and a transaction with a failing operation leaves every file as it was. <!-- g:atomic-tx -->
- A crash never leaves half a transaction. A process killed at any point either left the store untouched, or its transaction is finished the next time the store is opened, and with git on its commit is made exactly once. <!-- g:crash -->
- Several processes may write one store. Writes take one lock and queue behind it, a transaction waits `lock_timeout_ms` (5 seconds by default) before failing with `store_locked_timeout`, and no write is lost. <!-- g:multi-process -->
- A hand edit made while no write is in progress is never overwritten. A write that would replace a file changed since plaindb read it fails with `version_conflict` and the reason `changed_on_disk`, and the file stays as it is. After a restart a hand edit or a hand delete made since a committed transaction is kept as well. The one exception is a transaction that was cut short: finishing it writes its own files again. <!-- g:hand-edit -->
- A file plaindb wrote reads back unchanged: a document in each format decodes to the value that was encoded, a Markdown body survives byte for byte, and a clone of the remote opens as the same store with the same versions. <!-- g:round-trip -->
- With git on, a document can be read as of any commit or time with `get --at`, and `history` lists the commits that changed it. <!-- g:time-travel -->
- plaindb never force pushes. A push the remote refuses because it holds commits the store lacks, or because its history is unrelated, changes nothing on the remote. <!-- g:no-force-push -->

What plaindb does not promise: durability beyond the level chosen in [Durability](durability.md), and anything about files changed while a write is running.
