# Performance


Git history size after 10,000 single-document transactions: each transaction rewrites one of 1,000 documents of about 270 bytes in a JSON collection. Measured with `make bench` on an Apple M2 Pro, with the default `gc_every`.

| Commit mode | `.git` size | After `git gc --aggressive` | Time |
|---|---|---|---|
| One commit per transaction | 13.0 MiB | 10.0 MiB | 157 s |
| One commit per transaction, housekeeping off (`"gc_every": 0`) | 247 MiB | 9.9 MiB | 152 s |
| One commit per 100 transactions | 4.9 MiB | 1.5 MiB | 74 s |

Each commit rewrites the collection's tree, which lists every document, and leaves it as a loose object. The store therefore packs its repository by itself, as described in [Keeping the repository small](backup.md#keeping-the-repository-small), and the second row shows what the repository would grow to without it.

Speed was measured with `make bench` on an Apple M2 Pro, 2026-11-26. Each store holds 10,000 generated documents of 1 to 4 KB with 10 fields, declared indexes on `email` (unique), `age`, `status`, `address.city`, `tags` and a text index. Times are means.

| Measurement | JSON | YAML | Markdown |
| --- | --- | --- | --- |
| Open, cold (no index cache; scans every file and writes the cache) | 739 ms | 950 ms | 1.01 s |
| Open, warm (index cache present) | 98 ms | 99 ms | 113 ms |
| `find` on an indexed field, equality | 12 us | 12 us | 12 us |
| `find` on the same field with no index | 1.4 ms | 1.4 ms | 0.75 ms |
| `count` over an indexed range | 2.8 ms | 2.8 ms | 2.8 ms |
| `$text` search | n/a | n/a | 1.1 ms |

The warm open loads the cache and then stats every file to pick up hand edits. The cache is written without syncing and no measurement writes a document, so the durability setting does not change these numbers.

## Memory

Resident memory of a fresh process, from the same generator and indexes. Steady is after the open and 100 queries; the last column is after 10,000 more writes at durability `off`.

| Documents | Format | On disk | Open peak, cold | Open peak, warm | Steady | Steady per document | After 10,000 writes |
| --- | --- | --- | --- | --- | --- | --- | --- |
| 1,000 | JSON | 3 MiB | 14 MiB | 12 MiB | 14 MiB | 14.3 KiB | 21 MiB |
| 1,000 | YAML | 2 MiB | 15 MiB | 13 MiB | 14 MiB | 14.7 KiB | 22 MiB |
| 1,000 | Markdown | 2 MiB | 14 MiB | 13 MiB | 14 MiB | 14.5 KiB | 23 MiB |
| 10,000 | JSON | 25 MiB | 92 MiB | 86 MiB | 89 MiB | 9.1 KiB | 125 MiB |
| 10,000 | YAML | 24 MiB | 95 MiB | 86 MiB | 89 MiB | 9.1 KiB | 128 MiB |
| 10,000 | Markdown | 24 MiB | 93 MiB | 88 MiB | 90 MiB | 9.2 KiB | 115 MiB |
| 100,000 | JSON | 250 MiB | 825 MiB | 809 MiB | 795 MiB | 8.1 KiB | 888 MiB |
| 100,000 | YAML | 240 MiB | 822 MiB | 809 MiB | 797 MiB | 8.2 KiB | 884 MiB |
| 100,000 | Markdown | 239 MiB | 855 MiB | 818 MiB | 801 MiB | 8.2 KiB | 876 MiB |

At 10,000 documents the heap holds about 5.1 KB per document for the parsed documents themselves, 0.4 to 0.6 KB for the text index, 0.4 KB for the field indexes, 0.3 KB for the file entries and 0.2 KB to number the documents for the indexes (`make bench` prints this split). The documents stay parsed so that a query on a field with no index reads them without decoding any file. The resident set grows after many writes while the heap does not: the allocator keeps the freed memory for reuse.

The budget at 10,000 documents is 6,600 bytes of heap per document at steady state and 70 MiB of heap at the peak of a warm open. `make soak` fails when either is exceeded by more than 25%, or when the heap grows by more than 5% over 2,000 writes.
