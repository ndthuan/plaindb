# Troubleshooting


`plaindb doctor` checks the store and its environment and prints every problem with the command that fixes it. It never changes the store. It exits 1 when any check is an error, and 0 when there are only warnings. `--json` prints the same result as `plaindb_doctor` returns to an agent; `--offline` leaves the remote alone.

- git: the binary and where it came from, whether the remote answers, commits not pushed, a remote that is ahead, the last push error, lock files left in the repository and their age (an `index.lock` older than 10 minutes is an error), committed writes still waiting for their git commit, commits made outside plaindb, and whether housekeeping keeps up with `gc_every`.
- store: journal records not yet checkpointed, the write lock held by another process, unreadable documents and schema failures with the first 10 ids, file names that differ only by letter case, an index cache out of step with the files, and the store format.
- environment: a network filesystem, filesystem case sensitivity, the durability level and what a power loss can lose at it, and free disk space below 1 GiB or 10%.

A missing git binary or an unreachable remote is an error when push is on and a warning otherwise. The `journal` and `index_cache` warnings clear themselves, and can also show while another process is writing. `plaindb rescan` looks for edits made by hand, and `plaindb reindex` drops the index cache and reads every file again.
