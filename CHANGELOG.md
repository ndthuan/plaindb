# Changelog

All notable changes to plaindb are listed here. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project follows [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added

- Documents can carry attached files, such as images, PDFs or audio. Each file is stored as itself in `<db>/<collection>/_files/<id>/<name>` and described in the document's system field `_files` by its size, SHA-256 and MIME type. A file is attached or detached in the same transaction, journal record and git commit as its document, and is refused over `limits.max_file_bytes` (10 MiB by default). Deleting a document removes its files, a file changed by hand is not overwritten (`version_conflict` with the reason `changed_on_disk`), and document writes cannot set `_files`.
- `plaindb file put`, `file get`, `file list` and `file rm` attach, read, list and remove the files of a document. `file get` prints the bytes on standard output, or with `--out` writes them to a file; under `--json` it needs `--out`.
- The MCP tools `plaindb_put_file`, `plaindb_get_file` and `plaindb_delete_file` attach, read and remove files as base64; `--read-only` offers only `plaindb_get_file` of them.
- `plaindb doctor` warns about attached files that are missing (`attachment_missing`), changed by hand (`attachment_mismatch`) or listed by no document (`attachment_orphan`), each with the first 10 and how to fix them.

### Changed

- The documentation moved from the README into pages under `docs/`, and `SECURITY.md` into `.github/`.
- plaindb is now licensed under MIT instead of AGPL-3.0-only.
- Every read sees each transaction whole, also across processes: scans hold a shared lock on `.plaindb/lock`, and a store whose lock file cannot be opened is scanned until nothing moved.
- Read commands and MCP read tools open the store for reading: they change nothing but the index cache, never checkpoint the journal or touch `.git`, and work on a directory that cannot be written.
- With git on and no `.git` directory, plaindb no longer starts a new repository by itself: writes fail with `git_disabled` and the reason `repository_missing` until `plaindb git enable`.
- The MCP document write tools take an optional `tx_id`, so a lost answer can be retried without writing twice, and `plaindb mcp` exits with an error when its directory holds no store.
- `plaindb_doctor` over MCP does not contact the remote unless `offline` is `false`.
- A write that times out waiting for the lock no longer leaves a thread and an open file behind.
- A damaged journal record followed by more data is kept in `.plaindb/journal.rejected-<id>` instead of being cut off, and `status` warns about it.
- A write that reached the journal always succeeds. When a later step fails, the result carries `deferred` with the step and the error, the command prints a warning and exits 0, and `status` and `doctor` show git commits that are still to be made.
- The index cache carries a checksum of its contents and the revision of the decoder that wrote it; a damaged cache or one from another build is ignored and rebuilt.
- Numbers compare exactly: integers beyond 2^53 stay apart, an integer equals the float of the same value, and `-0.0` equals `0.0`. Arrays compare element by element and objects by their sorted keys and values, in filters, sort, `distinct` and indexes.
- YAML files and Markdown front matter are read as YAML 1.2: `NO`, `yes`, `off` and other YAML 1.1 words stay text, and `010` stays the text `010`. A leading byte order mark is dropped in every format and in a JSONL import, and a Markdown fence may end in spaces or tabs.
- A Markdown write is checked against the schema with its `body`, as the scan checks it, so a write the scan would call invalid is refused with `schema_violation`.
- With `--json` a usage error prints one failure line with the code `usage`, `doctor --json` with a check in error is a failure with the code `doctor_failed` and the report in its details, `plaindb mcp` never prints a command line envelope on standard output, and a reader that closes the pipe ends plaindb quietly with status 0.
- An import reports a repeated id as `duplicate_in_input` instead of writing it twice, keeps the details of each failure, writes each batch of 500 in one transaction and one commit even when documents in it fail, refuses timestamps that are not RFC 3339 and lines longer than the document limit, and returns its report with `aborted` when an error stops it; it then exits 1, as it does when every document failed.
- A file that cannot be read no longer makes every command fail: it is an invalid file with the reason `unreadable`. A file over `limits.max_document_bytes`, or 64 MiB with no limit, is not read and is invalid with the reason `too_large`. Symlinks, names that are not UTF-8 and files with another document extension are counted in the status warning `skipped_files` and the doctor check `store_skipped_files`.
- Documents that share a value under a unique index, for example after a file was copied by hand, are shown by the status warning `unique_duplicates` and the doctor error `store_unique_duplicates`.
- A file modified within 2 seconds of being read is read and hashed again on the next scan, so a same-size edit within one timestamp tick is no longer missed.
- A cursor is tied to the filter and sort it was made for; using it with another fails with `invalid_query` and the reason `cursor_mismatch`. Cursors from earlier versions are refused with `bad_cursor`.
- Text search normalises words to NFC, so composed and decomposed Unicode find each other, and searching all fields reaches strings inside nested objects.
- `limits.max_results` and `limits.max_scan_ms` refuse `0` in `config set` and in `_store.json`.
- Outside macOS, `durability: full` flushes files and directories with `fsync` instead of `fdatasync`, so it is never weaker than `normal`, which flushes directories with `fsync` too.
- A file plaindb rewrites keeps its permission bits on Unix; before, a mode set by hand was lost on the next write of that document.
- Recovery, when a store is opened or an interrupted write is finished, removes a leftover temp file only when it is older than 60 seconds, so the temp file of a write still in progress, such as an index cache a reader is saving, is left alone.
- The journal is also checkpointed once it reaches 32 MiB, so large writes do not pile up in it until the checkpoint interval.
- After the machine restarts, a document file that a committed insert created and that is missing is written again from the journal, so an insert whose rename a power loss took is no longer lost; later transactions in the journal still have the last word. A file deleted by hand before the next checkpoint therefore comes back after a reboot. A generation file that cannot be read as a number now logs a warning.
- A power-loss test runs on Linux CI: a store at `full` on LazyFS, with the unflushed data dropped at a random point, keeps every committed insert. `make powerloss` runs it on a Linux machine with FUSE.

## [0.1.0] - 2026-11-26

First release.

### Added

- A document store made of plain files: JSON, YAML and Markdown collections, one file per document, with system fields, versions that are git blob hashes, JSON Schema validation and import and export.
- Atomic multi-document transactions with `--if-version` conflict checks, a write lock shared by several processes, and recovery of a transaction cut short by a crash.
- Durability levels `full`, `normal` and `off`, with one full flush per write at `full` and a journal that is checkpointed in batches.
- Hand edits made outside plaindb are noticed, never overwritten, and kept when the store is reopened after a restart.
- Queries with filters, sorting, paging and field selection, field indexes, unique indexes and a text index, and an index cache that makes a warm open fast.
- Git history: one commit per transaction or one commit for many, time travel with `get --at`, `history` per document, and a warning when commits are made outside plaindb.
- Backup by push after each commit, without force and without failing a write, automatic housekeeping of the repository, and restore by cloning the remote.
- `plaindb doctor` and the settings commands `config get` and `config set`.
- `plaindb mcp`, a Model Context Protocol server with 25 tools and read-only and drop switches.
- Compressed postings in the indexes, with a memory budget checked by `make soak`.
- Release archives for macOS on Apple silicon and for Linux on x86_64 and aarch64, with a checksum file per archive and a `SHA256SUMS` file.
