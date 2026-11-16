# Phase A: real-disk feasibility — 16 November 2026

These are historical development measurements, not current performance promises. Earlier 0.2/0.3/0.4 labels identify unpublished experiment binaries; the project version is 0.0.1. The current memory comparison is in [MEMORY_RESULTS.md](MEMORY_RESULTS.md). Raw local evidence under `.temp_files` is excluded from the public repository.

Environment: Windows 11 Pro build 26300, AMD Ryzen 9 9950X3D, Samsung SSD 990 EVO Plus 4 TB NVMe on D: NTFS, approximately 126 GiB visible RAM, Rust 1.93/MSVC release. OS cache was not flushed; these are not controlled cold-SSD results.

## Scope

Native indexing produced 12,285,944 canonical file/folder records, followed by 12,285,803 after journal refresh. The disk was active. One canonical name per file reference was stored; full hard-link coverage was not established. Of 5000 eligible size probes, 4999 succeeded; refresh raised known-size coverage to 5499 files. This was not a complete disk-size index.

## Indexing and resources

| Measurement | Observed |
| --- | --- |
| Native name enumeration, sort, validation | 25.686 s |
| 5000 size probes | 116.445 ms; 1 failed |
| Initial journal replay and repacking | 3.912 s |
| Total index command wall time | 31.868 s |
| Index command CPU time | 30.563 s; about one core |
| Initial replay journal events | 14,561 |
| Private commit after indexing | 1.287 GiB |
| Indexing peak private commit | 3.073 GiB |
| Indexing peak working set | 2.573 GiB |
| Reloaded private commit | 1.286 GiB |
| Reloaded working set | 1.289 GiB |
| Final arena payload | 1,377,352,612 byte; about 112 bytes/record |
| Separate refresh command wall time | 8.831 s |
| Refresh peak private commit | 2.572 GiB |
| Snapshot load, checksum, validation | 2.735 s |

## Warm linear queries

Five warmups and 100 measured queries per group, full count and at most 100 results. Loading, path display, and UI were excluded. p95 is preliminary; no p99 was claimed.

| Query group | p50 | p95 |
| --- | --- | --- |
| Rare substring | 148.675 ms | 161.547 ms |
| Common substring | 174.958 ms | 184.508 ms |
| No-match substring | 85.255 ms | 98.486 ms |
| Prefix | 48.406 ms | 57.436 ms |
| Exact name | 51.903 ms | 64.534 ms |
| Extension | 61.591 ms | 68.271 ms |
| Large-file filter | 20.060 ms | 27.228 ms |
| Largest top-100 | 22.704 ms | 29.888 ms |
| All records | 41.559 ms | 50.303 ms |

Large-file results only cover the 5499 known-size files, although the linear implementation scanned all records. These numbers do not demonstrate the one-million-entry target or competitive superiority.

## Verification and issues found

Format, warnings-as-errors Clippy, 18 tests, release build, and real NTFS fixture acceptance passed. Rename/move/resize/create/delete, Unicode, and zero-size paths matched independent directory enumeration. Pending refresh was zero. A real junction-cycle test confirmed no reparse traversal. Snapshot DACL inheritance was disabled and limited to the current user, SYSTEM, and Administrators.

Two measured defects were fixed: a current-USN request needed Windows-compatible four-byte alignment (the failing live test returned OS error 1784), and one extra inserted record could double arena capacity during repacking. The latter regression initially allocated 150,000 bytes for 75,075 bytes of payload; reservation accounting fixed it. A live-disk observation reduced arena allocation from 2.56 GiB to 1.28 GiB, but its differing live records do not establish indexing speed improvement.

The fixture does not independently validate all disk records. Real journal destruction/wrap, device removal, sleep/resume, process-kill recovery, long-running service, other filesystems, GUI, allocated/ADS sizes, and all hard-link names were not tested/implemented.

Local evidence: [native index](../.temp_files/ntfs-verified-1794855229047/index.json), [refresh](../.temp_files/ntfs-verified-1794855229047/refresh.json), [acceptance](../.temp_files/ntfs-verified-1794855229047/acceptance.json), [warm queries](../.temp_files/ntfs-verified-1794855229047/bench.json), [idle](../.temp_files/ntfs-verified-1794855229047/idle.json). Earlier failed/intermediate logs were retained locally; redundant large snapshots were removed.

Retained binary SHA256 identities, in historical report order:

- `3774F647E8663C76CB069E7433F147BC2E7CFABE659A723C739F9CF2E518D33A`
