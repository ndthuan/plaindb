# Changelog

All notable changes to plaindb are listed here. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project follows [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Changed

- The documentation moved from the README into pages under `docs/`, and `SECURITY.md` into `.github/`.
- plaindb is now licensed under MIT instead of AGPL-3.0-only.
- A damaged journal record followed by more data is kept in `.plaindb/journal.rejected-<id>` instead of being cut off, and `status` warns about it.
- A write that reached the journal always succeeds. When a later step fails, the result carries `deferred` with the step and the error, the command prints a warning and exits 0, and `status` and `doctor` show git commits that are still to be made.

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
