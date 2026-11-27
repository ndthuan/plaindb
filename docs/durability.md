# Durability


`"durability"` in `_store.json` (`plaindb config set durability <level>`) sets how far a write is flushed before it returns:

- `full`, the default: flushed to stable storage, with a full drive cache flush on macOS. A committed write survives a power loss.
- `normal`: a plain `fsync`. A committed write survives a crash of the process or the operating system. On macOS a power loss may lose the last writes, but never leaves the store half written.
- `off`: no flush. A committed write survives a crash of the process only.

A transaction is committed when its record, which holds the new bytes of every file it changes, is appended to the journal in `.plaindb/` and flushed once at the store's level. The document files are then replaced by rename. On macOS they are only ordered behind the journal; on Linux, which has no ordering-only flush, each file is flushed before its rename.

Records stay in the journal until a checkpoint: every 64 transactions (`"checkpoint_interval"` in `_store.json`), when a store is closed, or when asked for. A checkpoint flushes the directories the records touched, moves the records into the transaction log and empties the journal. A record cut short at the end of the journal, as a crash leaves it, is dropped. A damaged record followed by more data is not dropped silently: everything from it on is moved to `.plaindb/journal.rejected-<id>` before the journal is shortened, and `status` shows the warning `journal_truncated` with the file's name until the file is removed.

Opening a store finishes any transaction whose files were not all written. After the machine restarts, it also replays every transaction not yet checkpointed, in order: the transactions a process that did not close cleanly wrote since its last checkpoint. A file that is missing, or that holds exactly the bytes it had before one of those transactions, is written again from the journal, so a write whose rename a power loss took is not lost; a file edited by hand to other content is kept. A file deleted by hand in that window therefore comes back after the restart, and can be deleted again. With git on, a transaction that already has its commit is never committed again.
