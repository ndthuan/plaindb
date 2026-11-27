# Use with an AI agent (MCP)


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

The tools are `plaindb_status`, `plaindb_list_dbs`, `plaindb_list_collections`, `plaindb_get_schema`, `plaindb_list_indexes`, `plaindb_get`, `plaindb_find`, `plaindb_count`, `plaindb_distinct`, `plaindb_history`, `plaindb_get_file`, `plaindb_doctor`, `plaindb_insert`, `plaindb_put`, `plaindb_patch`, `plaindb_delete`, `plaindb_tx`, `plaindb_put_file`, `plaindb_delete_file`, `plaindb_create_db`, `plaindb_create_collection`, `plaindb_set_schema`, `plaindb_add_index`, `plaindb_drop_index`, `plaindb_commit` and `plaindb_push`. A result is the `result` object of the matching command with `--json`; a failure is its `error` object and sets `isError`. A call with an argument the tool does not know is refused as the JSON-RPC error `invalid_params`, whose message names the argument; every tool's input schema says so with `additionalProperties: false`. `plaindb_push` takes no argument: clearing the mark of a remote that is ahead is left to a person, with `plaindb push --reset-remote-ahead`.

Attached files travel as standard base64: `plaindb_put_file` takes `db`, `collection`, `id`, `name` and `data_base64`, with optional `mime` and `if_version`, and answers as `file put --json` does; `data_base64` that is not base64 fails with `invalid_document`, and a file over `limits.max_file_bytes` with `limit_exceeded`. `plaindb_get_file` returns `name`, `size`, `sha256`, `mime` and `data_base64`, and `plaindb_delete_file` removes one file. The files of a document are listed in its `_files` field, as `plaindb_get` returns it.

`plaindb_insert`, `plaindb_put`, `plaindb_patch`, `plaindb_delete`, `plaindb_tx`, `plaindb_put_file` and `plaindb_delete_file` take an optional `tx_id`, a ULID like `--tx-id`: a call repeated with the same value returns the first result and writes nothing again, so a write whose answer was lost can be retried safely. The server opens the store once when it starts and exits with status 1 and the error on standard error, before answering anything, when the directory holds no store; each tool call then opens the store itself, so it sees what other processes wrote. The tools that read open the store for reading, as the read commands do, and `plaindb_doctor` does not contact the remote unless it is called with `offline: false`. `--read-only` offers only the tools that read, so the server also answers on a store it cannot write. The tools that drop a database or a collection, `plaindb_drop_db` and `plaindb_drop_collection`, are offered only with `--allow-drop`. The server tells the agent to use the tools for every read and write and never to touch the store's files, because a change made outside the tools may be refused or lost.
