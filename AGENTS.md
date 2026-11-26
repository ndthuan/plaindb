# Working on plaindb

The guide for whoever changes plaindb, people and coding agents alike.

## Writing

- Write everything in English: code, names, docs, commit messages, issues
  and pull requests.
- Code carries no comments; names carry the meaning. A reason goes in the
  commit message. Only what a tool reads stays (a lint or build directive,
  a generated-code marker).
- Nothing from a local environment goes in the repo: no machine paths,
  host names, IP addresses, user or account names, private repositories
  or services, tracker keys from outside this repository, or personal
  email addresses. Examples and test data use neutral placeholders
  (`example.com`, `/path/to/data`, `DOC-12`).
- No secret in code, tests, fixtures or logs. Tests make their own fake
  credentials.

## Working

- Run `make quality` before every push; CI must stay green.
- Every change comes with a test that fails without it.
- Work on `main` in small, focused commits.
- Commit subjects are imperative and at most 72 characters; bodies wrap
  at 72.
- Dependencies are pinned and the lockfile is committed.
