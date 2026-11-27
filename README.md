# plaindb

plaindb is a document database whose data is plain text files. Every document is one JSON, YAML or Markdown file that you can read, edit and diff with ordinary tools. One binary reads and writes the store from the command line and serves it to AI agents over the Model Context Protocol. It checks every write against the files on disk, so a person with an editor and a program can share one directory. With git turned on, every transaction becomes a commit: the history of each document can be read back, and the whole store can be pushed to a remote as a backup.

## Highlights

- **Plain files.** Every document is one JSON, YAML or Markdown file that you can read, edit and diff with ordinary tools.
- **Hand edits are safe.** plaindb notices a file changed by hand and never overwrites it: a write that would fails with `version_conflict`.
- **Transactions.** Several documents change together or not at all, with `--if-version` checks, and several processes share one store.
- **Crash safe.** A crash never leaves half a transaction, and the durability level sets what a power loss can lose.
- **Queries and indexes.** Filters, sorting, paging and field selection, with field, unique and text indexes.
- **History and backup in git.** With git on, every transaction is a commit: read a document as of any time, list its history, push the store to a remote after each commit and restore it with `git clone`.
- **Made for AI agents.** `plaindb mcp` serves the store over the Model Context Protocol, with read-only and drop switches.
- **It checks itself.** `plaindb doctor` lists every problem with the command that fixes it.

## Install

Download the archive for your platform from the [releases page](https://github.com/ndthuan/plaindb/releases), check it against `SHA256SUMS` and put `plaindb` on your `PATH`, or build it from source with `cargo install --git https://github.com/ndthuan/plaindb plaindb`. [Install](docs/install.md) lists the platforms and every step. Windows is not supported yet.

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

## Documentation

- [Install](docs/install.md): the release archives and their platforms, checking a download, building from source, and when `git` is needed.
- [Command line](docs/cli.md): choosing a store, the commands, filters, settings, the `--json` output, exit statuses, error codes, import and export.
- [Use with an AI agent](docs/mcp.md): connecting Claude Code and other MCP clients, and the tools.
- [Store format](docs/store-format.md): the directory layout, the three document formats, ids and names, system fields and versions.
- [Guarantees](docs/guarantees.md): what plaindb promises, each promise backed by tests.
- [Durability](docs/durability.md): the durability levels, the journal and checkpoints, and recovery after a crash.
- [Backup and restore](docs/backup.md): git history, pushing to a remote, restoring with `git clone`, and keeping the repository small.
- [Troubleshooting](docs/troubleshooting.md): `plaindb doctor` and what each check means.
- [Performance](docs/performance.md): git history size, speed and memory, measured.

Bug reports are welcome; [CONTRIBUTING.md](CONTRIBUTING.md) says how, and [SECURITY.md](.github/SECURITY.md) how to report a vulnerability.

## Not in scope


- Sync between machines. A store has one place where it is written; a remote is a backup, nothing is pulled or merged automatically, and two copies that both change are merged by a person with git.
- A network server. plaindb is a library and a command that runs where the files are; it opens no port.
- Authentication and access control. Whoever can read or write the directory can read or write the data, and the MCP server does not ask who is calling.

## Licence

MIT, see [LICENSE](LICENSE).
