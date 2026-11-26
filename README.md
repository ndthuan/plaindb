# plaindb

A NoSQL document database stored as plain text files, versioned and backed up with git.

Status: early development.

## Command line

`plaindb` takes the store from `--store <dir>`, then `PLAINDB_STORE`, then the current directory.

```
plaindb init --git
plaindb db create app
plaindb collection create app notes --format md
plaindb insert app notes --data '{"title": "First", "body": "Hello"}'
plaindb find app notes --filter '{"title": "First"}' --sort title --limit 10
plaindb patch app notes <id> --data '{"title": "Second"}' --if-version <version>
plaindb import app notes --dir ./old-notes
plaindb export app notes --jsonl --out notes.jsonl
```

Run `plaindb --help` for every command. Settings are read and changed with `plaindb config get` and `plaindb config set`.

With `--json` every command prints exactly one line:

```
{"schema":1,"ok":true,"result":{...}}
{"schema":1,"ok":false,"error":{"code":"not_found","message":"...","details":{}}}
```

The exit status is 0 on success, 1 when plaindb reports an error and 2 for a usage error. `schema` changes only with a breaking change to a response, and JSON Schema files for every response are in `crates/plaindb/schemas`. `export` without `--out` prints the documents themselves, one per line, in place of that object. `import` takes JSONL (`--jsonl <file>` or `-` for standard input) or a directory (`--dir`) of `.json`, `.yaml`, `.yml` and `.md` files whose names are the ids; documents that fail a check are listed in `failed` and the others are written, 500 to a transaction.

## Use with an AI agent (MCP)

`plaindb mcp` serves a store to an AI agent over the Model Context Protocol on standard input and output. Each tool call takes the store's write lock like any other process, so several agent sessions and the command line can share one store. Logs go to standard error, at the level of `PLAINDB_LOG`.

With Claude Code:

```
claude mcp add plaindb -- plaindb mcp --store /path/to/store
```

Or in an MCP client configuration file:

```
{
  "mcpServers": {
    "plaindb": {
      "command": "plaindb",
      "args": ["mcp", "--store", "/path/to/store"]
    }
  }
}
```

The tools are `plaindb_status`, `plaindb_list_dbs`, `plaindb_list_collections`, `plaindb_get_schema`, `plaindb_list_indexes`, `plaindb_get`, `plaindb_find`, `plaindb_count`, `plaindb_distinct`, `plaindb_history`, `plaindb_insert`, `plaindb_put`, `plaindb_patch`, `plaindb_delete`, `plaindb_tx`, `plaindb_create_db`, `plaindb_create_collection`, `plaindb_set_schema`, `plaindb_add_index`, `plaindb_drop_index`, `plaindb_commit` and `plaindb_push`. A result is the `result` object of the matching command with `--json`; a failure is its `error` object and sets `isError`.

`--read-only` offers only the tools that read. The tools that drop a database or a collection, `plaindb_drop_db` and `plaindb_drop_collection`, are offered only with `--allow-drop`. The server tells the agent to use the tools for every read and write and never to touch the store's files, because a change made outside the tools may be refused or lost.

## Backup and restore

Git is optional. With it on, every transaction becomes one commit, or in manual mode one commit gathers many transactions, and each commit carries the ids of the transactions it holds.

To back a store up:

1. Turn git on and set a remote. The remote is a plain git remote, so it can be a hosted repository or a bare repository on another disk.
2. Set `"push": "on-commit"` in `_store.json`. After each commit the store runs `git push <remote> <branch>` with a 60 second limit and up to three attempts. It never forces a push.
3. A failed push never fails the write. The commit stays local, and the status reports the number of unpushed commits and the last error. The next commit, or an explicit push, tries again.
4. A push refused because the remote holds commits the store lacks marks the remote as ahead and stops pushing until a person has merged the remote into the store and cleared the mark.

Credentials are git's own: the push uses the user's ssh agent and credential helpers, and the store keeps none.

Every `git` the store runs, for pushing and for housekeeping, is started the same way: messages are never translated, no password or passphrase prompt is ever shown, and ssh runs with `BatchMode=yes` unless `core.sshCommand` or `GIT_SSH_COMMAND` is set, so a remote that needs a passphrase fails at once with an error instead of waiting. A rejected push is recognised from git's machine-readable output, not from its wording. The `git` binary is the `"git_binary"` key of `_store.json` if set, else the `PLAINDB_GIT` environment variable, else `git` on `PATH`; the status names the one in use. With push on and no binary found, the status carries a `git_not_found` warning and each write reports that its push did not happen.

To restore, run `git clone <remote> <directory>` and open that directory as a store. Documents, their versions and their history are the same as in the original, because a version is the git blob hash of the file bytes and timestamps are document fields. A test restores a store of 200 transactions over JSON, YAML and Markdown collections this way and compares every file, version and history.

Nothing is pulled or merged automatically.

### Keeping the repository small

Every `"gc_every"` commits (default 200, `0` turns it off in `_store.json`) the store runs `git gc --auto` in its directory. Git packs the loose objects only when it holds more than its own threshold, so most runs do nothing. The run happens after the write lock is released and after any push, so it never delays a write by another process or a backup. Only one run is active at a time; a run that finds another one busy, including a `git gc` started by a person, leaves it alone and is reported as busy.

A failed run never fails a write. The status reports the time, outcome and error of the last run, and an explicit run packs everything now and reports the size of `.git` before and after.

## Durability

`"durability"` in `_store.json` sets how far a write is flushed before it returns:

- `full`, the default: flushed to stable storage, with a full drive cache flush on macOS. A committed write survives a power loss.
- `normal`: a plain `fsync`. A committed write survives a crash of the process or the operating system. On macOS a power loss may lose the last writes, but never leaves the store half written.
- `off`: no flush. A committed write survives a crash of the process only.

A transaction is committed when its record, which holds the new bytes of every file it changes, is appended to the journal in `.plaindb/` and flushed once at the store's level. The document files are then replaced by rename. On macOS they are only ordered behind the journal; on Linux, which has no ordering-only flush, each file is flushed before its rename.

Records stay in the journal until a checkpoint: every 64 transactions (`"checkpoint_interval"` in `_store.json`), when a store is closed, or when asked for. A checkpoint flushes the directories the records touched, moves the records into the transaction log and empties the journal.

Opening a store finishes any transaction whose files were not all written. After the machine restarts, it also writes again the files of every transaction not yet checkpointed, except files that were edited or removed by hand since that transaction. With git on, a transaction that already has its commit is never committed again.

## Performance

Git history size after 10,000 single-document transactions: each transaction rewrites one of 1,000 documents of about 270 bytes in a JSON collection. Measured with `make bench` on an Apple M2 Pro, with the default `gc_every`.

| Commit mode | `.git` size | After `git gc --aggressive` | Time |
|---|---|---|---|
| One commit per transaction | 13.0 MiB | 10.0 MiB | 157 s |
| One commit per transaction, housekeeping off (`"gc_every": 0`) | 247 MiB | 9.9 MiB | 152 s |
| One commit per 100 transactions | 4.9 MiB | 1.5 MiB | 74 s |

Each commit rewrites the collection's tree, which lists every document, and leaves it as a loose object. The store therefore packs its repository by itself, as described under Backup and restore, and the second row shows what the repository would grow to without it.

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
