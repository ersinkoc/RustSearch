# Name-search and delta optimization — 16 November 2026

These are historical development measurements, not current performance promises. Earlier 0.2/0.3/0.4 labels identify unpublished experiment binaries; the project version is 0.0.1. The current memory comparison is in [MEMORY_RESULTS.md](MEMORY_RESULTS.md). Raw local evidence under `.temp_files` is excluded from the public repository.

Environment: Windows 11 Pro build 26300, AMD Ryzen 9 9950X3D, Samsung SSD 990 EVO Plus 4 TB NVMe on D: NTFS, approximately 126 GiB visible RAM, Rust 1.93/MSVC release. OS cache was not flushed; these are not controlled cold-SSD results.

## Same-record comparison

The Phase A snapshot was frozen at 12,285,803 records. The retained baseline executable was rerun; only data layout changed for the new engine. Every ID, parent, USN, raw size, flag, original UTF-16 name, and normalized key matched between snapshots.

Each query group used five warmups, 100 measured queries, full counts, and at most 100 identically ordered results. The new engine also matched the linear count/returned-ID oracle. Runs were sequential; OS cache and external load were uncontrolled. No p99 was claimed.

## Warm queries

| Group | Retained baseline p95 | New implementation p95 |
| --- | --- | --- |
| Rare substring | 157.389 ms | 9.068 ms |
| Common one-character substring | 156.337 ms | 16.531 ms |
| No-match substring | 78.873 ms | 8.325 ms |
| Prefix | 54.972 ms | 0.0067 ms |
| Exact name | 55.575 ms | 0.0039 ms |
| Extension | 68.228 ms | 0.0220 ms |
| Large-file filter | 31.255 ms | 0.0007 ms |
| Largest top-100 | 33.244 ms | 0.0004 ms |
| All records | 49.402 ms | 0.0004 ms |

Rare substring improved about 17.4x and the common one-character query about 9.5x. Microsecond values are warm core measurements, not UI or separate-process startup. Size cases still cover only 5499 known-size files.

Records were grouped by normalized name and file/folder type: 2,903,752 groups. Exact/prefix lookup uses sorted groups, extensions have their own order, and known sizes have a size order. Character/trigram summaries narrow substring candidates without false negatives; actual byte matching follows when needed. Sorted postings and group minimum IDs avoid random access to millions of entries during top-100 selection. Counts remain complete. General substring cost still grows with distinct-name count.

## Memory and startup

| Measurement | Before | After |
| --- | --- | --- |
| Frozen corpus snapshot | 1.28 GiB | 0.85 GiB |
| Private commit including search helpers | 1.286 GiB | 1.012 GiB |
| Query experiment peak private commit | 1.287 GiB | 1.216 GiB |
| Working set | 1.289 GiB | 1.014 GiB |

Private commit fell about 21.4%, including 173,703,896 bytes of search helpers. Original names and normalized keys were interned separately, preserving spelling and all records. Savings depend on repetition.

The tradeoff was startup: source load took 1.674 s and search-table preparation took 8.368 s in this unpinned run. Helpers were not yet persistent. Warm-query gains did not imply a faster first CLI command.

A new live NTFS index took 35.820 s versus Phase A's 31.868 s; interning added work and live event sets differed. Indexing peak private commit fell from 3.073 to about 2.383 GiB. Neither run was a controlled cold-disk experiment.

## Identical synthetic update batch

Apply 128 synthetic metadata edits to the same frozen real-disk snapshot without changing user files. Each path ran in a separate process; source load and read-only hard-link setup were excluded. The old reference used `Index::apply + save`; the new path used `append_delta + apply_delta`. Result signatures matched.

| Measurement | Full repack and rewrite | Delta |
| --- | --- | --- |
| Update wall time | 5452.068 ms | 4.608 ms |
| Persisted bytes | 1,377,352,999 | 22,213 |
| Process peak private commit | 2.572 GiB | 1.287 GiB |

This single controlled batch reduced update time by about 1183x and persisted bytes by about 62,000x; neither is a universal guarantee. Short CPU counters have coarse resolution. The production-backed runner is `crates/cli/examples/update_compare.rs`.

The base snapshot became immutable; only committed records/checkpoints were appended to `.delta`. At that time, loading left 192 KiB of arena headroom to avoid copying large pools for tiny updates. Current compact name storage uses block appends instead; the table above describes the historical implementation.

## Live native acceptance

A separate active-disk scan produced 12,268,945 records. Fixture paths/sizes matched independent enumeration after create/delete/rename/move/resize, Unicode, and zero-size operations.

| Measurement | First | Second |
| --- | --- | --- |
| Journal events | 7713 | 6028 |
| Appended delta | 190,658 byte | 121,122 byte |
| Total CLI wall time including load | 1.949 s | 1.930 s |
| Process peak private commit | about 0.849 GiB | about 0.849 GiB |
| Delta sequence | 1 | 2 |

Both refreshes preserved the base SHA256 and had zero pending refresh. Different live batches cannot yield an exact speed ratio against Phase A's 8.831 s refresh. The delivered binary also passed a third append and separate-process fixture query at sequence 3.

Format, Clippy, 22 tests, release/example builds, query-oracle matrices, incomplete delta tails, committed corruption, stale writer state, generation mismatch, and writer contention passed. Delta budget is 64 MiB; explicit new-path compaction is required at the limit. Preserve base plus matching log. Actual power loss, process-kill acceptance, long-running service, hosted CI, and Everything comparison were not run.

Local evidence: [baseline](../.temp_files/optimization-20261116/baseline-original.json), [indexed queries](../.temp_files/optimization-20261116/indexed-final.json), [all-record verification](../.temp_files/optimization-20261116/logical-record-verification.txt), [rewrite](../.temp_files/optimization-20261116/update-final-rewrite.json), [delta](../.temp_files/optimization-20261116/update-final-delta.json), [native acceptance](../.temp_files/ntfs-optimized-1794857366388/acceptance.json), [final native verification](../.temp_files/ntfs-optimized-1794857366388/final-binary.json). Disposable large update outputs were removed; frozen source and JSON evidence were retained.

Retained binary SHA256 identities, in historical report order:

- `3774F647E8663C76CB069E7433F147BC2E7CFABE659A723C739F9CF2E518D33A`
- `E43FCA23CBD078E8C0B6CA302CAEF8A98607E9BAC85A40A265DC741B3B8EB4C2`
