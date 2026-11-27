# Command line


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

A filter is a JSON object. A field name alone means equality, and `$eq`, `$ne`, `$gt`, `$gte`, `$lt`, `$lte`, `$in`, `$exists`, `$prefix`, `$and`, `$or` and `$text` (search in the text index; a word matches in composed and decomposed Unicode alike) build the rest. `plaindb index add` declares an index on a field, and a query on an indexed field does not read the other documents. A unique index refuses a write that would give two documents the same value with `unique_violation`; an explicit `null` is a value under a unique index and a missing field is not. Documents that share a value because files were copied or edited by hand are shown by `status` as `unique_duplicates` and by `doctor` as the error `store_unique_duplicates`.

Every command that takes a document, a merge patch, a schema or a list of operations (`insert`, `put`, `patch`, `collection schema set` and `tx`) takes `--data <json>` or `--file <path>`, and `--file -` reads standard input; giving both is a usage error, and so is giving neither to `patch`, `collection schema set` and `tx` (`insert` and `put` then read standard input). `find` returns a page of up to 100 documents with a `next_cursor`, which only continues the same filter and sort: with another one it fails with `invalid_query` and the reason `cursor_mismatch`; `find --all` reads the pages one after another in one process and prints every match with `next_cursor: null`. It cannot be combined with `--cursor` or `--limit`, a change made between two pages shows as it does when paging by hand, and with `limits.max_results` set it fails with `limit_exceeded` once the matches pass that number.

Run `plaindb --help` for every command. Settings are read and changed with `plaindb config get` and `plaindb config set`: `commit`, `push`, `remote`, `branch`, `lock_timeout_ms`, `durability`, `gc_every`, `checkpoint_interval` (a whole number above 0), `limits.max_document_bytes`, `limits.max_results` and `limits.max_scan_ms` (the last two a whole number above 0, or empty for no limit). `config get` without a key prints every setting with its default, and a value a setting cannot take is refused with `invalid_config`. A key in `_store.json` that is not a setting is kept when the file is rewritten, shown by `status` as the warning `unknown_setting` and by `doctor` as a warning. A new database or collection name may not start with `-`; ones that already do stay usable. `init` refuses `--remote`, `--push on-commit` and `--commit manual` without `--git`. The `git` program is not a setting: it comes from the `PLAINDB_GIT` environment variable, an absolute path or a program name, or else `git` on `PATH`.

With `--json` every command prints exactly one line:

```
{"schema":1,"ok":true,"result":{...}}
{"schema":1,"ok":false,"error":{"code":"not_found","message":"...","details":{}}}
```

The exit status is 0 on success, 1 when plaindb reports an error and 2 for a usage error. A usage error under `--json` prints `{"schema":1,"ok":false,"error":{"code":"usage","message":"...","details":{}}}` with the first line of the parser's message, and the whole message on standard error; `--help` and `--version` print plain text. When `doctor --json` finds a check in error, the line is a failure with the code `doctor_failed` and the report under `error.details.report`. `plaindb mcp` prints only protocol messages on standard output, whatever `--json` says, and its errors go to standard error. When the reader of standard output goes away, plaindb stops quietly with status 0. A status of 101 means a bug in plaindb, and no JSON line is printed then. `schema` changes only with a breaking change to a response, and JSON Schema files for every response are in [`crates/plaindb/schemas`](../crates/plaindb/schemas). `export` without `--out`, or with `--out -`, prints the documents themselves, one per line, in place of that object. `import` takes JSONL (`--jsonl <file>` or `-` for standard input) or a directory (`--dir`) of `.json`, `.yaml`, `.yml` and `.md` files whose names are the ids; documents that fail a check are listed in `failed` with the line or file, the code and the details of the error, and the others are written, 500 to a transaction and one commit even when some of them fail. A line or file whose id came earlier in the same input fails with `duplicate_in_input`, with or without `--replace`. `_created_at` and `_updated_at`, when given, must be RFC 3339 timestamps, and a line longer than `limits.max_document_bytes` (64 MiB when unset) fails with `limit_exceeded`. When an error that is not about one document stops the import, such as a lock that stays taken, the report still comes with what was written before, under `aborted` the reason, and nothing read after the last written batch is written. The exit status is 1 when the import stopped or when every document failed, under `--json` with the code `import_failed` and the report under `error.details.report`; an empty input exits 0.

The commands that only read are `get`, `find`, `count`, `distinct`, `history`, `status`, `validate`, `export`, `doctor`, `config get` and the `list` and `schema get` commands. A read never changes the store apart from the index cache under `.plaindb`, so they also answer on a store the user cannot write. When another write was cut short and its files are not finished yet, a read finishes them if it can write the store and the lock is free; otherwise it reads the files as they are and `status` shows `journal_unfinished`. With git on and the `.git` directory gone, reads still work, `status` shows `git_repository_missing`, and every write fails with `git_disabled` until `plaindb git enable` starts a new history.

The error codes are `not_found`, `already_exists`, `version_conflict`, `invalid_name`, `invalid_document`, `invalid_config`, `schema_violation`, `unique_violation`, `invalid_query`, `store_locked_timeout`, `not_a_store`, `unsupported_format`, `limit_exceeded`, `git_disabled`, `git_not_found`, `remote_ahead`, `head_off_branch`, `io`, `usage`, `doctor_failed` and `import_failed`.
