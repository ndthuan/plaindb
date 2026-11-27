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

A filter is a JSON object. A field name alone means equality, and `$eq`, `$ne`, `$gt`, `$gte`, `$lt`, `$lte`, `$in`, `$exists`, `$prefix`, `$and`, `$or` and `$text` (search in the text index) build the rest. `plaindb index add` declares an index on a field, and a query on an indexed field does not read the other documents.

Every command that takes a document, a merge patch, a schema or a list of operations (`insert`, `put`, `patch`, `collection schema set` and `tx`) takes `--data <json>` or `--file <path>`, and `--file -` reads standard input; giving both is a usage error, and so is giving neither to `patch`, `collection schema set` and `tx` (`insert` and `put` then read standard input). `find` returns a page of up to 100 documents with a `next_cursor`; `find --all` reads the pages one after another in one process and prints every match with `next_cursor: null`. It cannot be combined with `--cursor` or `--limit`, a change made between two pages shows as it does when paging by hand, and with `limits.max_results` set it fails with `limit_exceeded` once the matches pass that number.

Run `plaindb --help` for every command. Settings are read and changed with `plaindb config get` and `plaindb config set`: `commit`, `push`, `remote`, `branch`, `lock_timeout_ms`, `durability`, `gc_every`, `checkpoint_interval` (a whole number above 0), `limits.max_document_bytes`, `limits.max_results` and `limits.max_scan_ms`. `config get` without a key prints every setting with its default, and a value a setting cannot take is refused with `invalid_config`. A key in `_store.json` that is not a setting is kept when the file is rewritten, shown by `status` as the warning `unknown_setting` and by `doctor` as a warning. A new database or collection name may not start with `-`; ones that already do stay usable. `init` refuses `--remote`, `--push on-commit` and `--commit manual` without `--git`. The `git` program is not a setting: it comes from the `PLAINDB_GIT` environment variable, an absolute path or a program name, or else `git` on `PATH`.

With `--json` every command prints exactly one line:

```
{"schema":1,"ok":true,"result":{...}}
{"schema":1,"ok":false,"error":{"code":"not_found","message":"...","details":{}}}
```

The exit status is 0 on success, 1 when plaindb reports an error and 2 for a usage error. `schema` changes only with a breaking change to a response, and JSON Schema files for every response are in [`crates/plaindb/schemas`](../crates/plaindb/schemas). `export` without `--out`, or with `--out -`, prints the documents themselves, one per line, in place of that object. `import` takes JSONL (`--jsonl <file>` or `-` for standard input) or a directory (`--dir`) of `.json`, `.yaml`, `.yml` and `.md` files whose names are the ids; documents that fail a check are listed in `failed` and the others are written, 500 to a transaction.

The commands that only read are `get`, `find`, `count`, `distinct`, `history`, `status`, `validate`, `export`, `doctor`, `config get` and the `list` and `schema get` commands. A read never changes the store apart from the index cache under `.plaindb`, so they also answer on a store the user cannot write. When another write was cut short and its files are not finished yet, a read finishes them if it can write the store and the lock is free; otherwise it reads the files as they are and `status` shows `journal_unfinished`. With git on and the `.git` directory gone, reads still work, `status` shows `git_repository_missing`, and every write fails with `git_disabled` until `plaindb git enable` starts a new history.

The error codes are `not_found`, `already_exists`, `version_conflict`, `invalid_name`, `invalid_document`, `invalid_config`, `schema_violation`, `unique_violation`, `invalid_query`, `store_locked_timeout`, `not_a_store`, `unsupported_format`, `limit_exceeded`, `git_disabled`, `git_not_found`, `remote_ahead`, `head_off_branch` and `io`.
