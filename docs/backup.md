# Backup and restore


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

## Keeping the repository small

Every `"gc_every"` commits (default 200, `0` turns it off, set with `plaindb config set gc_every`) the store runs `git gc --auto` in its directory. Git packs the loose objects only when it holds more than its own threshold, so most runs do nothing. The run happens after the write lock is released and after any push, so it never delays a write by another process or a backup. Only one run is active at a time; a run that finds another one busy, including a `git gc` started by a person, leaves it alone and is reported as busy.

A failed run never fails a write. The status reports the time, outcome and error of the last run. Through the library, an explicit run packs everything now and reports the size of `.git` before and after.
