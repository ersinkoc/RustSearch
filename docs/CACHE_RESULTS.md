# Persistent search cache — 16 November 2026

These are historical development measurements, not current performance promises. Earlier 0.2/0.3/0.4 labels identify unpublished experiment binaries; the project version is 0.0.1. The current memory comparison is in [MEMORY_RESULTS.md](MEMORY_RESULTS.md). Raw local evidence under `.temp_files` is excluded from the public repository.

Environment: Windows 11 Pro build 26300, AMD Ryzen 9 9950X3D, Samsung SSD 990 EVO Plus 4 TB NVMe on D: NTFS, approximately 126 GiB visible RAM, Rust 1.93/MSVC release. OS cache was not flushed; these are not controlled cold-SSD results.

## Change

Persist name groups, masks, postings, extension order, and known-size order in `.rsidx.search`, bound to immutable base header identity. `index` and `compact` prepare it once. Cache validation still checks checksum, shape, memberships, masks, ordering, and coverage. Missing, corrupt, incompatible, or wrong-generation cache rebuilds; failed publication falls back to valid-source in-memory search. Source corruption still fails.

Committed deltas mask changed base rows and bind a small current overlay without rewriting the base cache. Cache publication uses atomic temp/sync/replace and a private Windows DACL. The frame/compaction protocol did not change.

## New-process startup

Same frozen 12,285,803 records, five new processes per variant, alternating order, child affinity mask 1. Reported values are median (min–max); no startup p95/p99. This fixed-core rerun, not the historical unpinned 8.368 s preparation sample, is the comparison control.

| Metric | Experiment 0.2 rebuild | Experiment 0.3 cache hit |
| --- | --- | --- |
| Source load | 1.659 s (1.605–1.689) | 1.629 s (1.612–1.732) |
| Search-table preparation | 4.292 s (4.080–4.356) | 1.244 s (1.220–1.314) |
| End-to-end CLI | 6.060 s (5.795–6.248) | 3.051 s (2.935–3.100) |
| Process CPU time | 5.875 s | 2.906 s |
| Private commit | about 1.013 GiB | about 1.012 GiB |
| Process peak private commit | about 1.216 GiB | about 1.012 GiB |
| Working set | about 1.015 GiB | about 1.015 GiB |

Preparation fell 71%, total startup 49.7%, and peak private commit 16.8%. Steady memory stayed approximately unchanged. Disk cache size was 173,439,480 bytes (about 165.4 MiB); process counters do not represent all OS file-cache memory.

## Native delta acceptance

A fresh active D: run produced 12,274,519 records, distinct from the frozen corpus. One-time cache build took 6.391 s plus 142.948 ms publication; the complete index command took 62.154 s. This is not an initial-indexing speed claim.

The first separate query hit cache at sequence 0. Rename/move/resize/delete/create, Unicode, and zero size matched independent enumeration. After delta 1: 16 live overlay records, 0.092 ms binding, about 1.399 s preparation. After delta 2: 22 overlay records, 0.110 ms binding, about 1.431 s preparation. Base/cache hashes remained unchanged; log size was 6704 bytes and pending refresh was zero. Missing/incompatible cache still incurs one-time construction.

## Verification

Format, Clippy, 28 tests, release/example builds, native acceptance, and nine warm-query linear-oracle groups passed. Cases included two delta generations, parent moves, type/size/extension changes, Unicode, incomplete tails, corrupt/truncated/forged caches, wrong masks/postings, generation mismatch, failed publication, and cache-builder contention.

A valid-CRC duplicate extension posting initially escaped validation; the regression failed before uniqueness/coverage checks were added, then passed through cache rebuild with correct results. Hosted CI, power loss, long-running service, other hardware/OS, and GUI were not tested.

Local evidence: [startup samples](../.temp_files/cache-20261116/startup-summary.json), [warm oracle](../.temp_files/cache-20261116/hot-cached.json), [native acceptance](../.temp_files/ntfs-cache-1794862816351/acceptance.json), [first query](../.temp_files/ntfs-cache-1794862816351/fixture-before.stdout.json), [two-delta query](../.temp_files/ntfs-cache-1794862816351/fixture-after-again.stdout.json).

Retained binary SHA256 identities, in historical report order:

- `E43FCA23CBD078E8C0B6CA302CAEF8A98607E9BAC85A40A265DC741B3B8EB4C2`
- `D31F61409CE23E13A10CBA973D0BFE2F518A89D9A8076E9DEB8FE954B05D4841`
