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

The tools are `plaindb_status`, `plaindb_list_dbs`, `plaindb_list_collections`, `plaindb_get_schema`, `plaindb_list_indexes`, `plaindb_get`, `plaindb_find`, `plaindb_count`, `plaindb_distinct`, `plaindb_history`, `plaindb_doctor`, `plaindb_insert`, `plaindb_put`, `plaindb_patch`, `plaindb_delete`, `plaindb_tx`, `plaindb_create_db`, `plaindb_create_collection`, `plaindb_set_schema`, `plaindb_add_index`, `plaindb_drop_index`, `plaindb_commit` and `plaindb_push`. A result is the `result` object of the matching command with `--json`; a failure is its `error` object and sets `isError`. A call with an argument the tool does not know is refused as the JSON-RPC error `invalid_params`, whose message names the argument; every tool's input schema says so with `additionalProperties: false`. `plaindb_push` takes no argument: clearing the mark of a remote that is ahead is left to a person, with `plaindb push --reset-remote-ahead`.

`--read-only` offers only the tools that read. The tools that drop a database or a collection, `plaindb_drop_db` and `plaindb_drop_collection`, are offered only with `--allow-drop`. The server tells the agent to use the tools for every read and write and never to touch the store's files, because a change made outside the tools may be refused or lost.
