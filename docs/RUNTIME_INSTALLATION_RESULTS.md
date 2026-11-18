# Portable runtime repair and saved-catalog recovery — 0.0.1

## Installed executable mismatch

The follow-up report of repeated scanning was valid. Two background workers were running for different data directories: an updated development build and the extracted original portable release. The portable executable still had SHA-256 `a31c8c1e0e1626cc9e5d75f6bddd78313e9786c74168ad6d468c62057399ba92`; its status moved from scanning D: back to scanning C:. Updating the repository had not updated that executable.

Both workers were stopped through their authenticated protocol. Original portable files were backed up before replacing the local binaries and updating their build identity/checksums. The unused development profile's background/startup flags were disabled. The existing enabled Windows sign-in entry was transferred to the portable executable and normal `%LOCALAPPDATA%\RustSearch` data directory. A subsequent reboot launched that portable worker automatically. C:/D:/E: scope, disabled size collection, saved indexes, and other settings were retained.

The original downloaded ZIP, public v0.0.1 tag, and GitHub release archive remain unchanged. Re-extracting that ZIP restores old code. See [portable installation guidance](PORTABLE.md).

## Reproduced defects and changes

Before the metadata-ordering fix, the real-drive harness observed a new ID after each file rename on C:, D:, and E:. A delayed write notification of the old path saw `NotFound` after the filesystem rename and erased the identity before the paired rename was handled. Directory metadata could also enumerate children ahead of structural events. Missing metadata paths now retain pending identities; known-directory metadata updates do not enumerate children. Structural events and explicit loss recovery resolve missing entries. Three ordering regressions were red before the fix and green afterward; a positive control retains recursive retry of previously inaccessible directory contents.

Recovery also repeatedly looked up common names across an entire volume. Temporary parent-to-row boundaries now restrict immutable name postings, capped at 64 MiB per volume/recovery transaction and released afterward. A 20,000-path local probe with the same expected IDs measured **337.270 ms before / 29.870 ms after**, including table construction. This is a lookup microbenchmark, not a disk benchmark. Overlay candidates are restricted by parent and name key; permanent tests cover tombstones, case folding, moved entries, and inserted parents/children.

A later real-D: profiling probe exposed repeated sorting of roughly 219,000 changed rows and full-overlay pruning on every batch. Recovery now borrows the search index's existing parent/key-ordered overlay and inspects only children of checked parents, removing a duplicate temporary vector. Across 100 batches, recovery-step time fell from **4,427 ms to 849 ms**; completed-directory counts were 3,179 / 3,147, with changed-row counts 218,884 / 223,700. Including mandatory search rebinding in this deliberately pessimistic probe, the measured phases totaled **8,735 ms / 6,241 ms**. The disk changed between observations, so these are diagnostic profiles, not a controlled speedup claim. A larger batch-cap experiment showed no useful improvement and was discarded.

That lookup improvement alone still left startup queries blocked for more than **60 seconds** during recursive recovery. Recovery now yields after at most **32 directories**, checking a **25 ms** budget between directories. One directory remains atomic, so this is not a hard response-time bound. New live notifications take priority over the existing traversal. Queued directory IDs resolve their current paths, preserving work across directory moves. Counts update from committed delta records, and directory enumeration reuses entry metadata rather than querying every file again.

Subtree deletion also walked every live row, repeatedly for nested directories. It now follows the same parent-row boundaries, indexed overlay children, and staged children, preserving rows moved out of the deleted subtree. Highly fragmented layouts beyond the lookup-memory cap keep the original fallback. A controlled in-memory removal against one loaded D: index produced the same **36 tombstones** in **700.17 ms before / 77.06 ms after**, including temporary boundary construction. No filesystem entry was changed by that probe. A permanent regression covers nested deletion, moved-in/moved-out rows, and newly staged descendants.

A valid saved computer catalog now opens directly, including accessible-file indexes. Offline changes are checked in background batches while cached searches remain available. The interface shows **Syncing** and runtime status shows **Checking saved files** until that check completes. The saved-catalog restart assertion failed before this change and passes afterward. An unreadable saved source still triggers one full-scan fallback, covered by a separate control. Pending/inaccessible coverage is retained rather than silently deleting saved entries.

An actual NTFS junction exposed another classification defect: metadata refresh changed directory+reparse flags `5` to file+reparse flags `4`. Refresh now uses `FILE_ATTRIBUTE_DIRECTORY`, matching initial enumeration, and continues to avoid following reparse targets. The junction regression was observed red and then green.

During the first cooperative recovery runs, the native reader still reported repeated buffer losses (the last pre-buffer run reached **11 backend losses / 11 queue recoveries**). Local fixed/removable/RAM volumes now use two 1 MiB buffers per reader, while remote/unclassified volumes retain two 64 KiB buffers. The reader is armed before its constructor returns. Timestamp-only filters were removed because timestamps/content are not indexed; name, size, attribute, and security notifications remain. Microsoft's [ReadDirectoryChangesW documentation](https://learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-readdirectorychangesw) describes the network buffer limit and loss semantics. A real Windows watcher test checks immediate flushed size changes and paired renames after construction.

## Durable-log follow-up

The extended real-drive check exposed another independent source of repeated rebuilding: every append reparsed the complete durable log under a newly acquired writer lock. The D: log reached **66,982,890 bytes**, only **125,974 bytes below its 64 MiB limit**, before the old worker was stopped. A retained pre-fix diagnostic records **delta budget reached; compact to a new path**. The failed frame exceeded the remaining headroom, and that error path requests a full scan even without notification loss. The old worker was stopped before a replacement catalog was published. That diagnostic predates the current worker and remains as historical evidence; its timestamp has not advanced during the updated run.

Managed sources now retain one writer guard across updates. The guard caches only validated sequence/checkpoint/position and file metadata, not the log contents. A new guard or changed log metadata forces full validation; stale writers, checksums, checkpoint chains, and incomplete-tail recovery remain checked. Other cooperating writers are excluded while the worker owns that source; readers remain supported.

Accumulated history is atomically folded into a complete overlay checkpoint after 32 MiB of history or before an append exhausts the log budget. IDs, tombstones, UTF-16 names, unknown sizes, metadata, sequence numbers, and original base/cache bytes are retained. Checkpoint writes use a private temporary file, sync, and atomic replacement. The compacted log records its starting sequence in an `RSDEL002` header; readers also accept existing `RSDEL001` logs. The first checkpoint must be committed before it can replay, and interrupted later appends retain that checkpoint. An overlay whose checkpoint itself exceeds the 64 MiB budget still requires a new base; this is not an unlimited log format.

**Checkpointed logs require the updated GUI and CLI. The original release executables cannot read this header.** Pre-checkpoint logs and binaries were backed up before updating the local installation. The public ZIP was not replaced.

A private copy of the same D: snapshot/log measured **170–179 ms per empty append before**. The updated path paid **446 ms once** to validate/checkpoint, then **5.69–14.96 ms per append** (median **6.72 ms**), including durable flush. Log size fell from **56.1 MB to 18.6 MB**. Reloading that source fell from **33,334 ms to 1,003 ms**. A full canonical digest of all **240,096 overlay rows** plus metadata, counts, identity, and sequence matched after replay. These are local storage diagnostics, not an Everything comparison or whole-app startup benchmark.

Recovery also retains its temporary parent-row lookup through subtree deletion before returning it to the background work queue, avoiding a second lookup-table build during the same batch.

## Checks and local build identity

Version remains **0.0.1**. The installed local desktop build contains source through `ca785e847a089e9d6d7eb7a7bd27bac5feb1bedb`, with desktop SHA-256 `a401339b8c0f971a4b743bbd8117ac788ae0f5db7dbc78d5ba42c2026b39803a`.

- `cargo fmt --all -- --check`: passed.
- `cargo clippy --workspace --all-targets --release --locked -- -D warnings`: passed.
- `cargo test --workspace --release --locked`: **83 passed**, zero failed.
- `cargo build --workspace --bins --release --locked`: passed.

These checks cover backend/runtime behavior. Warm-query throughput was not rebenchmarked in this runtime follow-up; query correctness was checked against the linear implementation and with real live searches. GUI layout, accessibility, and visual acceptance were not repeated in this change. Existing [desktop limitations](DESKTOP.md) and the upstream dependency advisory still apply.

## Same-disk startup and live results

The repaired worker reused the same saved C:/D:/E: catalog throughout the checks; no source/catalog replacement was accepted. Cached-start observations varied as the corpus and accumulated deltas changed:

| Local build/run | Catalog load | Initial search preparation | Startup probe visible during synchronization |
| --- | ---: | ---: | ---: |
| First cooperative recovery build | 2,816 ms | 1,299 ms | 0.531 s |
| Junction/overlay follow-up | 34,650 ms | 1,862 ms | 2.395 s |
| Larger-buffer build | 35,767 ms | 1,530 ms | 2.781 s |
| Reused-overlay build | 32,680 ms | 1,429 ms | 1.585 s |
| Subtree-range build | 31,237 ms | 1,486 ms | 1.938 s |
| Checkpoint build loading accumulated legacy logs | 70,230 ms | 1,728 ms | 2.436 s |
| Same binary after log checkpointing | 3,092 ms | Not captured at initial publication | 1.263 s |

The probe interval starts after initial source publication; it is **not** process-to-first-result time. The captured initial preparations used cache hits. On the checkpointed restart the observer joined after an update had already rebound the tables (`live`); its 216 ms preparation sample is not an initial cache-load measurement. These are single observations with different accumulated deltas/cache conditions, not controlled startup speedup benchmarks. Catalog loading excludes search preparation and client subscription. The checkpointed 3.09-second load is one warm local observation; process-to-first-result and GUI startup are not derived from it.

The post-checkpoint restart publication contained **22,213,049** records; ambient changes continued during catch-up. The live harness checked all three drives while synchronization was active:

| Assertion | Result |
| --- | --- |
| Combined/source-filtered search | 3 combined matches; 1 per drive |
| Live creation | Visible on all drives after 0.630 s |
| File rename | Original source+ID preserved on all drives |
| Unicode folder move | Descendant IDs retained; path predicate visible after 0.654 s |
| Deletion | Zero fixture matches after 0.674 s |
| Persistence | All 6 snapshot/cache SHA-256 values and catalog unchanged |

The resize fixture wrote 4,097 bytes on each drive; indexed sizes correctly remained unknown because this profile has size collection disabled. This run does not claim indexed size-change verification. The native watcher test and size-enabled regressions verify that path separately. Latencies are polling-harness observations during recovery, not percentile benchmarks. Owned fixture files/directories were removed.

## Background resource results

The first observed interval with no queued recovery across all sources was **64 minutes 30 seconds after launch**, when the worker reported **Monitoring changes**. This is an observation time rather than a continuously captured exact completion timestamp. This was one background offline-change check on the retained catalog, including ordinary live changes and one application queue burst coalesced into folder deltas. At that point the source contained **22,270,123 records**: C: **7,660,497**, D: **14,457,622**, E: **152,004**. Coverage remains partial: the saved sources report **958 skipped entries**, and **970 indexed rows** were pending refresh at that observation. Those categories can overlap.

**Offline-change traversal remains far too expensive on this corpus.** Cached searches stay available during it; this fix does not eliminate the one check after worker downtime. After it drained, subsequent live activity caused short directory-check batches. The current **Syncing / Checking saved files** label also describes these ordinary batches and can therefore appear again without a new full scan.

Two app-idle monitoring windows followed the first empty recovery interval. The GUI was closed, indexing remained enabled, and no search requests or fixture mutations were issued during either window. Ambient filesystem changes continued. Each window sampled the same process 61 times and retained one catalog. These are **app-idle background-monitoring measurements with active volumes**, not quiescent-disk idle measurements.

CPU is process CPU-time change divided by elapsed time; whole-machine CPU divides again by the verified **32 logical processors**. Private commit and resident working set are separate Windows counters.

| Window | Duration | CPU, one core = 100% | CPU, whole machine = 100% | Private commit | Resident working set |
| --- | ---: | ---: | ---: | ---: | ---: |
| Before the final live round | 62.14 s | 66.08% | 2.07% | 1748.9–1788.2 MiB | 1537.3–1583.2 MiB |
| After the final live round | 61.92 s | 69.35% | 2.17% | 1757.2–1788.2 MiB | 1545.4–1571.6 MiB |

The first window recorded 41 **Monitoring changes** and 20 **Checking saved files** samples; the second recorded 38 / 23. The initial offline check had already drained before both windows. Brief live folder reconciliation explains the mixed phases; neither window is represented as 61 uninterrupted monitoring samples.

**The low-resource app-idle target remains unmet.** This change removes expensive repeated history validation and avoids rebuilding a source merely because redundant log history fills the budget. It does not establish negligible background CPU, a memory reduction, or superiority over Everything. Small-folder reconciliation and repeated search-overlay binding remain candidates for further profiling.

For comparison, the larger-buffer build's earlier active catch-up window used **100.07% of one core / 3.13% of the machine**, private commit **1718.1–1756.1 MiB**, and working set **1713.3–1757.3 MiB** over **61.41 seconds**. A later pre-checkpoint subtree-range window used **100.07% / 3.13%**, private commit **1723.2–1775.3 MiB**, and working set **1723.9–1776.4 MiB** over **61.44 seconds**. The checkpoint build's active catch-up window used **97.28% / 3.04%**, private commit **1754.6–1873.9 MiB**, and working set **1163.1–1217.6 MiB** over **61.94 seconds**. These differ in phase and cache/residency conditions; no controlled CPU or memory speedup is inferred from them.

The earlier [overflow report](OVERFLOW_RECOVERY_RESULTS.md) measured 66.82–85.33% of one core during monitoring with size collection enabled. That profile and corpus differ from this size-disabled run, so it is not an equivalent idle baseline.

## Final runtime verification

The post-catch-up C:/D:/E: mutation round passed combined and per-drive searches, creation (**1.043 s**), file rename with retained IDs, Unicode folder moves with retained descendant IDs (**0.879 s**), and deletion (**1.017 s**). Size fields remained unknown by configuration. All **six snapshot/cache SHA-256 values** and the catalog stayed unchanged. Owned fixture files and directories were removed.

The running worker recorded **one application queue recovery and zero backend losses** through these checks. C: also automatically folded another accumulated-history interval during the run: its log fell from about **48.4 MB to 22.0 MB**, advanced its checkpoint starting sequence from **9,753 to 13,399**, and retained all six base/cache hashes. No full source replacement was observed.

The local portable package's **987 checksum entries** were verified. One updated portable worker remains enabled for the normal data directory and Windows sign-in startup; the unused development profile remains disabled. Public **v0.0.1** tag/archive bytes are unchanged. GUI layout and visual acceptance were not rerun for this backend change.

Raw disk paths, endpoint credentials, row metadata, logs, hashes, and process samples stay private. Only aggregate evidence is published here.
