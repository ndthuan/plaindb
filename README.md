# plaindb

plaindb is a document database whose data is plain text files. Every document is one JSON, YAML or Markdown file that you can read, edit and diff with ordinary tools. One binary reads and writes the store from the command line and serves it to AI agents over the Model Context Protocol. It checks every write against the files on disk, so a person with an editor and a program can share one directory. With git turned on, every transaction becomes a commit: the history of each document can be read back, and the whole store can be pushed to a remote as a backup.

## Install

Download the archive for your platform from the [releases page](https://github.com/ndthuan/plaindb/releases): `aarch64-apple-darwin` for Macs with Apple silicon, `x86_64-unknown-linux-musl` or `aarch64-unknown-linux-musl` for Linux. The Linux binaries are static and run on any distribution. Check the download against the checksum file of the same release, then unpack it:

```
shasum -a 256 -c SHA256SUMS --ignore-missing
tar xzf plaindb-<version>-<target>.tar.gz
```

Put the `plaindb` binary on your `PATH`. To build it from source instead, with a Rust toolchain:

```
cargo install --git https://github.com/ndthuan/plaindb plaindb
```

`git` is needed only for the features that use it: history, time travel and backup.

## Quickstart

Every step needs only the `plaindb` binary and `git`. `init` creates the store in the directory that `PLAINDB_STORE` names, which does not have to exist yet.

```
export PLAINDB_STORE="$PWD/mystore"
plaindb init
plaindb db create demo
plaindb collection create demo notes --format md
plaindb insert demo notes n1 --data '{"title": "Hello", "body": "First note"}'
plaindb find demo notes --filter '{"title": "Hello"}'
plaindb git enable
git init --bare --initial-branch=main /tmp/plaindb-backup.git
plaindb git remote set /tmp/plaindb-backup.git
plaindb config set push on-commit
plaindb insert demo notes n2 --data '{"title": "Second", "body": "Another note"}'
plaindb push
plaindb history demo notes n1
```

The first note is the file `mystore/demo/notes/n1.md`. `git enable` commits the store as it is and every write after it makes a commit. With push set to `on-commit` each commit is pushed to the backup repository, and `plaindb push` sends anything still waiting. `plaindb init --git` makes a store that is under git from the start.

## Command line

`plaindb` takes the store from `--store <dir>`, then `PLAINDB_STORE`, then the current directory.

```
plaindb init --git
plaindb db create app
plaindb collection create app notes --format md
plaindb insert app notes --data '{"title": "First", "body": "Hello"}'
plaindb find app notes --filter '{"title": "First"}' --sort title --limit 10
plaindb get app notes <id> --at 2026-01-31T12:00:00Z
plaindb patch app notes <id> --data '{"title": "Second"}' --if-version <version>
plaindb import app notes --dir ./old-notes
plaindb export app notes --jsonl --out notes.jsonl
```

A filter is a JSON object. A field name alone means equality, and `$eq`, `$ne`, `$gt`, `$gte`, `$lt`, `$lte`, `$in`, `$exists`, `$prefix`, `$and`, `$or` and `$text` (search in the text index) build the rest. `plaindb index add` declares an index on a field, and a query on an indexed field does not read the other documents.

Run `plaindb --help` for every command. Settings are read and changed with `plaindb config get` and `plaindb config set`: `commit`, `push`, `remote`, `branch`, `lock_timeout_ms`, `durability`, `gc_every`, `git_binary`, `limits.max_document_bytes`, `limits.max_results` and `limits.max_scan_ms`. `config get` without a key prints every setting with its default, and a value a setting cannot take is refused with `invalid_config`.

With `--json` every command prints exactly one line:

```
{"schema":1,"ok":true,"result":{...}}
{"schema":1,"ok":false,"error":{"code":"not_found","message":"...","details":{}}}
```

The exit status is 0 on success, 1 when plaindb reports an error and 2 for a usage error. `schema` changes only with a breaking change to a response, and JSON Schema files for every response are in `crates/plaindb/schemas`. `export` without `--out` prints the documents themselves, one per line, in place of that object. `import` takes JSONL (`--jsonl <file>` or `-` for standard input) or a directory (`--dir`) of `.json`, `.yaml`, `.yml` and `.md` files whose names are the ids; documents that fail a check are listed in `failed` and the others are written, 500 to a transaction.

The error codes are `not_found`, `already_exists`, `version_conflict`, `invalid_name`, `invalid_document`, `invalid_config`, `schema_violation`, `unique_violation`, `invalid_query`, `store_locked_timeout`, `not_a_store`, `unsupported_format`, `limit_exceeded`, `git_disabled`, `git_not_found`, `remote_ahead` and `io`.

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

The tools are `plaindb_status`, `plaindb_list_dbs`, `plaindb_list_collections`, `plaindb_get_schema`, `plaindb_list_indexes`, `plaindb_get`, `plaindb_find`, `plaindb_count`, `plaindb_distinct`, `plaindb_history`, `plaindb_doctor`, `plaindb_insert`, `plaindb_put`, `plaindb_patch`, `plaindb_delete`, `plaindb_tx`, `plaindb_create_db`, `plaindb_create_collection`, `plaindb_set_schema`, `plaindb_add_index`, `plaindb_drop_index`, `plaindb_commit` and `plaindb_push`. A result is the `result` object of the matching command with `--json`; a failure is its `error` object and sets `isError`.

`--read-only` offers only the tools that read. The tools that drop a database or a collection, `plaindb_drop_db` and `plaindb_drop_collection`, are offered only with `--allow-drop`. The server tells the agent to use the tools for every read and write and never to touch the store's files, because a change made outside the tools may be refused or lost.

## Store format

A store is a directory:

```
mystore/
  _store.json              settings of the store
  <db>/
    _db.json
    <collection>/
      _meta.json           format, schema and indexes of the collection
      <id>.json            one file per document
  .plaindb/                state of the running store; never edit it
  .git/                    present when git is on
```

A collection holds documents of one format, chosen when it is created:

- `json`: `<id>.json`, one object, keys sorted, two-space indentation.
- `yaml`: `<id>.yaml`. Comments in a hand-written file are read and dropped when plaindb rewrites it.
- `md`: `<id>.md`, the fields as YAML front matter and the field `body` as the Markdown text after it. The body is kept byte for byte.

An id is 1 to 128 characters from letters, digits, `_` and `-`, and does not begin with `_` or `-`; a database or collection name is 1 to 64 lower case letters, digits, `_` and `-`, and does not begin with `_`. plaindb makes a ULID when an id is left out. The file name is the id, and ids that differ only by letter case are refused, so a store copies safely to a case-insensitive disk.

Every document carries `_id`, `_created_at` and `_updated_at`. The version of a document is the git blob hash of its file, so `git hash-object <file>` prints it. Writes take `--if-version` to fail with `version_conflict` when the document has changed since it was read. A collection can have a JSON Schema, checked on every write and by `plaindb validate`, and indexes on fields or a text index.

Files are plain: editing one by hand is supported. plaindb notices the change by the file's size and modification time on the next command, and a file it cannot read is listed as invalid instead of stopping the store.

## Guarantees

Each line below is backed by tests named in `crates/plaindb-core/tests/guarantees.rs`, and a test fails when this list and that table disagree.

- A transaction applies fully or not at all: every operation is checked before any file changes, and a transaction with a failing operation leaves every file as it was. <!-- g:atomic-tx -->
- A crash never leaves half a transaction. A process killed at any point either left the store untouched, or its transaction is finished the next time the store is opened, and with git on its commit is made exactly once. <!-- g:crash -->
- Several processes may write one store. Writes take one lock and queue behind it, a transaction waits `lock_timeout_ms` (5 seconds by default) before failing with `store_locked_timeout`, and no write is lost. <!-- g:multi-process -->
- A hand edit made while no write is in progress is never overwritten. A write that would replace a file changed since plaindb read it fails with `version_conflict` and the reason `changed_on_disk`, and the file stays as it is. After a restart a hand edit or a hand delete made since a committed transaction is kept as well. The one exception is a transaction that was cut short: finishing it writes its own files again. <!-- g:hand-edit -->
- A file plaindb wrote reads back unchanged: a document in each format decodes to the value that was encoded, a Markdown body survives byte for byte, and a clone of the remote opens as the same store with the same versions. <!-- g:round-trip -->
- With git on, a document can be read as of any commit or time with `get --at`, and `history` lists the commits that changed it. <!-- g:time-travel -->
- plaindb never force pushes. A push the remote refuses because it holds commits the store lacks, or because its history is unrelated, changes nothing on the remote. <!-- g:no-force-push -->

What plaindb does not promise: durability beyond the level chosen under Durability, and anything about files changed while a write is running.

## Durability

`"durability"` in `_store.json` (`plaindb config set durability <level>`) sets how far a write is flushed before it returns:

- `full`, the default: flushed to stable storage, with a full drive cache flush on macOS. A committed write survives a power loss.
- `normal`: a plain `fsync`. A committed write survives a crash of the process or the operating system. On macOS a power loss may lose the last writes, but never leaves the store half written.
- `off`: no flush. A committed write survives a crash of the process only.

A transaction is committed when its record, which holds the new bytes of every file it changes, is appended to the journal in `.plaindb/` and flushed once at the store's level. The document files are then replaced by rename. On macOS they are only ordered behind the journal; on Linux, which has no ordering-only flush, each file is flushed before its rename.

Records stay in the journal until a checkpoint: every 64 transactions (`"checkpoint_interval"` in `_store.json`), when a store is closed, or when asked for. A checkpoint flushes the directories the records touched, moves the records into the transaction log and empties the journal.

Opening a store finishes any transaction whose files were not all written. After the machine restarts, it also writes again the files of every transaction not yet checkpointed, except files that were edited or removed by hand since that transaction. With git on, a transaction that already has its commit is never committed again.

## Backup and restore

Git is optional. With it on, every transaction becomes one commit, or in manual mode one commit gathers many transactions, and each commit carries the ids of the transactions it holds.

To back a store up:

1. Turn git on and set a remote with `plaindb git remote set <url>`. The remote is a plain git remote, so it can be a hosted repository or a bare repository on another disk.
2. Run `plaindb config set push on-commit`. After each commit the store runs `git push <remote> <branch>` with a 60 second limit and up to three attempts. It never forces a push.
3. A failed push never fails the write. The commit stays local, and the status reports the number of unpushed commits and the last error. The next commit, or an explicit `plaindb push`, tries again.
4. A push refused because the remote holds commits the store lacks marks the remote as ahead and stops pushing until a person has merged the remote into the store's repository with git and run `plaindb push --reset-remote-ahead`.

Credentials are git's own: the push uses the user's ssh agent and credential helpers, and the store keeps none.

Every `git` the store runs, for pushing and for housekeeping, is started the same way: messages are never translated, no password or passphrase prompt is ever shown, and ssh runs with `BatchMode=yes` unless `core.sshCommand` or `GIT_SSH_COMMAND` is set, so a remote that needs a passphrase fails at once with an error instead of waiting. A rejected push is recognised from git's machine-readable output, not from its wording. The `git` binary is the `"git_binary"` key of `_store.json` if set, else the `PLAINDB_GIT` environment variable, else `git` on `PATH`; the status names the one in use. With push on and no binary found, the status carries a `git_not_found` warning and each write reports that its push did not happen.

To restore, run `git clone <remote> <directory>` and open that directory as a store. Documents, their versions and their history are the same as in the original, because a version is the git blob hash of the file bytes and timestamps are document fields. A test restores a store of 200 transactions over JSON, YAML and Markdown collections this way and compares every file, version and history.

Nothing is pulled or merged automatically.

### Keeping the repository small

Every `"gc_every"` commits (default 200, `0` turns it off, set with `plaindb config set gc_every`) the store runs `git gc --auto` in its directory. Git packs the loose objects only when it holds more than its own threshold, so most runs do nothing. The run happens after the write lock is released and after any push, so it never delays a write by another process or a backup. Only one run is active at a time; a run that finds another one busy, including a `git gc` started by a person, leaves it alone and is reported as busy.

A failed run never fails a write. The status reports the time, outcome and error of the last run. Through the library, an explicit run packs everything now and reports the size of `.git` before and after.

## Troubleshooting

`plaindb doctor` checks the store and its environment and prints every problem with the command that fixes it. It never changes the store. It exits 1 when any check is an error, and 0 when there are only warnings. `--json` prints the same result as `plaindb_doctor` returns to an agent; `--offline` leaves the remote alone.

- git: the binary and where it came from, whether the remote answers, commits not pushed, a remote that is ahead, the last push error, lock files left in the repository and their age (an `index.lock` older than 10 minutes is an error), commits made outside plaindb, and whether housekeeping keeps up with `gc_every`.
- store: journal records not yet checkpointed, the write lock held by another process, unreadable documents and schema failures with the first 10 ids, file names that differ only by letter case, an index cache out of step with the files, and the store format.
- environment: a network filesystem, filesystem case sensitivity, the durability level and what a power loss can lose at it, and free disk space below 1 GiB or 10%.

A missing git binary or an unreachable remote is an error when push is on and a warning otherwise. The `journal` and `index_cache` warnings clear themselves, and can also show while another process is writing. `plaindb rescan` looks for edits made by hand, and `plaindb reindex` drops the index cache and reads every file again.

## Performance

Git history size after 10,000 single-document transactions: each transaction rewrites one of 1,000 documents of about 270 bytes in a JSON collection. Measured with `make bench` on an Apple M2 Pro, with the default `gc_every`.

| Commit mode | `.git` size | After `git gc --aggressive` | Time |
|---|---|---|---|
| One commit per transaction | 13.0 MiB | 10.0 MiB | 157 s |
| One commit per transaction, housekeeping off (`"gc_every": 0`) | 247 MiB | 9.9 MiB | 152 s |
| One commit per 100 transactions | 4.9 MiB | 1.5 MiB | 74 s |

Each commit rewrites the collection's tree, which lists every document, and leaves it as a loose object. The store therefore packs its repository by itself, as described under Backup and restore, and the second row shows what the repository would grow to without it.

Speed was measured with `make bench` on an Apple M2 Pro, 2026-11-26. Each store holds 10,000 generated documents of 1 to 4 KB with 10 fields, declared indexes on `email` (unique), `age`, `status`, `address.city`, `tags` and a text index. Times are means.

| Measurement | JSON | YAML | Markdown |
| --- | --- | --- | --- |
| Open, cold (no index cache; scans every file and writes the cache) | 739 ms | 950 ms | 1.01 s |
| Open, warm (index cache present) | 98 ms | 99 ms | 113 ms |
| `find` on an indexed field, equality | 12 us | 12 us | 12 us |
| `find` on the same field with no index | 1.4 ms | 1.4 ms | 0.75 ms |
| `count` over an indexed range | 2.8 ms | 2.8 ms | 2.8 ms |
| `$text` search | n/a | n/a | 1.1 ms |

The warm open loads the cache and then stats every file to pick up hand edits. The cache is written without syncing and no measurement writes a document, so the durability setting does not change these numbers.

### Memory

Resident memory of a fresh process, from the same generator and indexes. Steady is after the open and 100 queries; the last column is after 10,000 more writes at durability `off`.

| Documents | Format | On disk | Open peak, cold | Open peak, warm | Steady | Steady per document | After 10,000 writes |
| --- | --- | --- | --- | --- | --- | --- | --- |
| 1,000 | JSON | 3 MiB | 14 MiB | 12 MiB | 14 MiB | 14.3 KiB | 21 MiB |
| 1,000 | YAML | 2 MiB | 15 MiB | 13 MiB | 14 MiB | 14.7 KiB | 22 MiB |
| 1,000 | Markdown | 2 MiB | 14 MiB | 13 MiB | 14 MiB | 14.5 KiB | 23 MiB |
| 10,000 | JSON | 25 MiB | 92 MiB | 86 MiB | 89 MiB | 9.1 KiB | 125 MiB |
| 10,000 | YAML | 24 MiB | 95 MiB | 86 MiB | 89 MiB | 9.1 KiB | 128 MiB |
| 10,000 | Markdown | 24 MiB | 93 MiB | 88 MiB | 90 MiB | 9.2 KiB | 115 MiB |
| 100,000 | JSON | 250 MiB | 825 MiB | 809 MiB | 795 MiB | 8.1 KiB | 888 MiB |
| 100,000 | YAML | 240 MiB | 822 MiB | 809 MiB | 797 MiB | 8.2 KiB | 884 MiB |
| 100,000 | Markdown | 239 MiB | 855 MiB | 818 MiB | 801 MiB | 8.2 KiB | 876 MiB |

At 10,000 documents the heap holds about 5.1 KB per document for the parsed documents themselves, 0.4 to 0.6 KB for the text index, 0.4 KB for the field indexes, 0.3 KB for the file entries and 0.2 KB to number the documents for the indexes (`make bench` prints this split). The documents stay parsed so that a query on a field with no index reads them without decoding any file. The resident set grows after many writes while the heap does not: the allocator keeps the freed memory for reuse.

The budget at 10,000 documents is 6,600 bytes of heap per document at steady state and 70 MiB of heap at the peak of a warm open. `make soak` fails when either is exceeded by more than 25%, or when the heap grows by more than 5% over 2,000 writes.

## Not in scope

- Sync between machines. A store has one place where it is written; a remote is a backup, nothing is pulled or merged automatically, and two copies that both change are merged by a person with git.
- A network server. plaindb is a library and a command that runs where the files are; it opens no port.
- Authentication and access control. Whoever can read or write the directory can read or write the data, and the MCP server does not ask who is calling.

## Licence and contributing

plaindb is licensed under AGPL-3.0-only; see `LICENSE`. Issues and bug reports are welcome. Code contributions are not accepted for now. See `CONTRIBUTING.md`, and `SECURITY.md` for security problems.
