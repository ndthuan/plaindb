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
      _files/
        <id>/
          <name>           a file attached to the document <id>
  .plaindb/                state of the running store; never edit it
  .git/                    present when git is on
```

A collection holds documents of one format, chosen when it is created:

- `json`: `<id>.json`, one object, keys sorted, two-space indentation.
- `yaml`: `<id>.yaml`. Comments in a hand-written file are read and dropped when plaindb rewrites it.
- `md`: `<id>.md`, the fields as YAML front matter and the field `body` as the Markdown text after it. The body is kept byte for byte.

An id is 1 to 128 characters from letters, digits, `_` and `-`, and does not begin with `_` or `-`; a database or collection name is 1 to 64 lower case letters, digits, `_` and `-`, and does not begin with `_`. plaindb makes a ULID when an id is left out. The file name is the id, and ids that differ only by letter case are refused, so a store copies safely to a case-insensitive disk.

Every document carries `_id`, `_created_at` and `_updated_at`. The version of a document is the git blob hash of its file, so `git hash-object <file>` prints it. Writes take `--if-version` to fail with `version_conflict` when the document has changed since it was read. A collection can have a JSON Schema, checked on every write and by `plaindb validate`, and indexes on fields or a text index.

A document can carry files that are not documents, such as images, PDFs or audio: `plaindb file put` attaches one, and it is stored as itself under `_files/<id>/<name>` in the collection, so any program opens it. The document's system field `_files` describes each one, for example `{"photo.png": {"size": 3072, "sha256": "<lower case hex>", "mime": "image/png"}}`. The MIME type is the one given, or else is guessed from the extension (`png`, `jpg`, `jpeg`, `gif`, `webp`, `svg`, `pdf`, `mp3`, `m4a`, `wav`, `ogg`, `mp4`, `webm`, `txt`, `csv`, `json`, `zip`), or else is `application/octet-stream`. A file name is 1 to 128 letters, digits, `_`, `-` and `.`, does not begin with `.`, `_` or `-`, holds no `..`, and may not differ from another file of the same document only by letter case. Attaching or removing a file rewrites its document in the same transaction, so the document's version changes and `--if-version` applies; attaching under a name already taken replaces that file. Document writes cannot set `_files` (`invalid_document`), and `put` and `patch` keep it as it is. Deleting a document removes its files, and dropping a collection or database removes them with it. A file is refused with `limit_exceeded` when it is larger than `limits.max_file_bytes`, 10 MiB unless set otherwise. With git on, an attached file is committed with its document and stays in the git history for good, even after it is removed, so a large or private file stays in every clone; plaindb does not use Git LFS. A file changed by hand is not overwritten: attaching over it or removing it fails with `version_conflict` and the reason `changed_on_disk`, and reading it does too; `plaindb doctor` lists missing, changed and orphan files. Files are read whole into memory, and `export` carries only the `_files` metadata.

Files are plain: editing one by hand is supported, and a file plaindb rewrites keeps its permission bits on Unix. plaindb notices the change by the file's size and modification time on the next command, and a file modified within 2 seconds of being read is read again on the next scan, so a same-size edit within one timestamp tick is seen too; and a file it cannot read is listed as invalid instead of stopping the store: with the reason `unreadable` when the file or its metadata cannot be read, and `too_large`, unread, when it is larger than `limits.max_document_bytes` or, with no limit set, 64 MiB. Such a file is looked at again on every scan. A symlink, a name that is not UTF-8 and a file with another document extension (`.json`, `.yaml`, `.yml`, `.md`, `.markdown`) than the collection's own are not read: `status` shows one `skipped_files` warning per collection with the count and the first 10 names, and `doctor` the warning `store_skipped_files`. A leading UTF-8 byte order mark is dropped, and a Markdown fence line may end in spaces or tabs. YAML is read as YAML 1.2: only `true` and `false` are booleans, so `NO`, `yes` and `off` stay text, and a number written with a leading zero, such as `010`, stays the text `010`. Other numbers are numbers, `+12345` included, so quote a value that must stay text when it looks like a number, such as a phone number: `phone: "+12345"`. On a Markdown collection the schema sees the document with its `body`, on a write as on a read, so a schema that refuses unknown fields has to declare `body`.
