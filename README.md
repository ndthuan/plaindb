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

## Performance

Git history size after 10,000 single-document transactions: each transaction rewrites one of 1,000 documents of about 270 bytes in a JSON collection. Measured with `make bench` on an Apple M2 Pro.

| Commit mode | `.git` size | After `git gc --aggressive` | Time |
|---|---|---|---|
| One commit per transaction | 247 MiB | 9.9 MiB | 233 s |
| One commit per 100 transactions | 4.9 MiB | 1.4 MiB | 178 s |

The large size of the first row is loose objects: each commit rewrites the collection's tree, which lists every document. Manual mode, or a periodic `git gc`, keeps the repository small.

Licensed under AGPL-3.0-only. See `LICENSE`.
