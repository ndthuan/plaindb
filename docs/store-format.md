# Store format


A store is a directory:

```
mystore/
  _store.json              settings of the store
  <db>/
    _db.json
    <collection>/
      _meta.json           format, schema and indexes of the collection
      <id>.json            one file per document
  .plaindb/                state of the running store; never edit it
  .git/                    present when git is on
```

A collection holds documents of one format, chosen when it is created:

- `json`: `<id>.json`, one object, keys sorted, two-space indentation.
- `yaml`: `<id>.yaml`. Comments in a hand-written file are read and dropped when plaindb rewrites it.
- `md`: `<id>.md`, the fields as YAML front matter and the field `body` as the Markdown text after it. The body is kept byte for byte.

An id is 1 to 128 characters from letters, digits, `_` and `-`, and does not begin with `_` or `-`; a database or collection name is 1 to 64 lower case letters, digits, `_` and `-`, and does not begin with `_`. plaindb makes a ULID when an id is left out. The file name is the id, and ids that differ only by letter case are refused, so a store copies safely to a case-insensitive disk.

Every document carries `_id`, `_created_at` and `_updated_at`. The version of a document is the git blob hash of its file, so `git hash-object <file>` prints it. Writes take `--if-version` to fail with `version_conflict` when the document has changed since it was read. A collection can have a JSON Schema, checked on every write and by `plaindb validate`, and indexes on fields or a text index.

Files are plain: editing one by hand is supported. plaindb notices the change by the file's size and modification time on the next command, and a file it cannot read is listed as invalid instead of stopping the store. A leading UTF-8 byte order mark is dropped, and a Markdown fence line may end in spaces or tabs. YAML is read as YAML 1.2: only `true` and `false` are booleans, so `NO`, `yes` and `off` stay text, and a number written with a leading zero, such as `010`, stays the text `010`. Other numbers are numbers, `+12345` included, so quote a value that must stay text when it looks like a number, such as a phone number: `phone: "+12345"`. On a Markdown collection the schema sees the document with its `body`, on a write as on a read, so a schema that refuses unknown fields has to declare `body`.
