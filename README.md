# plaindb

A NoSQL document database stored as plain text files, versioned and backed up with git.

Status: early development.

## Backup and restore

Git is optional. With it on, every transaction becomes one commit, or in manual mode one commit gathers many transactions, and each commit carries the ids of the transactions it holds.

To back a store up:

1. Turn git on and set a remote. The remote is a plain git remote, so it can be a hosted repository or a bare repository on another disk.
2. Set `"push": "on-commit"` in `_store.json`. After each commit the store runs `git push <remote> <branch>` with a 60 second limit and up to three attempts. It never forces a push.
3. A failed push never fails the write. The commit stays local, and the status reports the number of unpushed commits and the last error. The next commit, or an explicit push, tries again.
4. A push refused because the remote holds commits the store lacks marks the remote as ahead and stops pushing until a person has merged the remote into the store and cleared the mark.

Credentials are git's own: the push uses the user's ssh agent and credential helpers, and the store keeps none.

To restore, run `git clone <remote> <directory>` and open that directory as a store. Documents, their versions and their history are the same as in the original, because a version is the git blob hash of the file bytes and timestamps are document fields. A test restores a store of 200 transactions over JSON, YAML and Markdown collections this way and compares every file, version and history.

Nothing is pulled or merged automatically.

## Durability

`"durability"` in `_store.json` sets how far a write is flushed before it returns:

- `full`, the default: flushed to stable storage, with a full drive cache flush on macOS. A committed write survives a power loss.
- `normal`: a plain `fsync`. A committed write survives a crash of the process or the operating system. On macOS a power loss may lose the last writes, but never leaves the store half written.
- `off`: no flush. A committed write survives a crash of the process only.

A transaction is committed when its record, which holds the new bytes of every file it changes, is appended to the journal in `.plaindb/` and flushed once at the store's level. The document files are then replaced by rename. On macOS they are only ordered behind the journal; on Linux, which has no ordering-only flush, each file is flushed before its rename.

Records stay in the journal until a checkpoint: every 64 transactions (`"checkpoint_interval"` in `_store.json`), when a store is closed, or when asked for. A checkpoint flushes the directories the records touched, moves the records into the transaction log and empties the journal.

Opening a store finishes any transaction whose files were not all written. After the machine restarts, it also writes again the files of every transaction not yet checkpointed, except files that were edited by hand since that transaction.

## Performance

Git history size after 10,000 single-document transactions: each transaction rewrites one of 1,000 documents of about 270 bytes in a JSON collection. Measured with `make bench` on an Apple M2 Pro.

| Commit mode | `.git` size | After `git gc --aggressive` | Time |
|---|---|---|---|
| One commit per transaction | 247 MiB | 9.9 MiB | 233 s |
| One commit per 100 transactions | 4.9 MiB | 1.4 MiB | 178 s |

The large size of the first row is loose objects: each commit rewrites the collection's tree, which lists every document. Manual mode, or a periodic `git gc`, keeps the repository small.

Licensed under AGPL-3.0-only. See `LICENSE`.

## Performance

Measured with `make bench` on an Apple M2 Pro, 2026-11-26. Each store holds 10,000 generated documents of 1 to 4 KB with 10 fields, declared indexes on `email` (unique), `age`, `status`, `address.city`, `tags` and a text index. Times are means.

| Measurement | JSON | YAML | Markdown |
| --- | --- | --- | --- |
| Open, cold (no index cache; scans every file and writes the cache) | 1.07 s | 1.23 s | 1.33 s |
| Open, warm (index cache present) | 437 ms | 437 ms | 477 ms |
| `find` on an indexed field, equality | 14 us | 14 us | 14 us |
| `find` on the same field with no index | 1.4 ms | 1.4 ms | 0.65 ms |
| `count` over an indexed range | 3.3 ms | 3.3 ms | 3.3 ms |
| `$text` search | n/a | n/a | 1.6 ms |
| Peak resident memory after open and one query | 230 MB | 230 MB | 251 MB |

The warm open loads the cache and then stats every file to pick up hand edits. The cache is written without syncing and no measurement writes a document, so the durability setting does not change these numbers. Peak memory is the resident set of a fresh process that opens the store and runs one query.
