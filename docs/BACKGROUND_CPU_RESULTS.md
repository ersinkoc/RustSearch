# Bounded live-folder reconciliation — 0.0.1

## Observed cost

A fresh 61-sample, 61.54-second window on the same C:/D:/E: worker measured **70.72% of one core / 2.21% of the 32-thread machine**, private commit **1809.1–1849.4 MiB**, and resident working set **1511.0–1545.9 MiB**. The GUI was closed, no foreground queries were issued, size collection remained disabled, and ambient filesystem activity continued. Runtime phases included short directory checks; this is app-idle monitoring on active volumes.

Small nonrecursive folder reconciliation enumerated every live indexed row to find missing children. That also consulted the overlay map for unrelated base IDs. Routine checks therefore scaled with volume size even when the folder had one unchanged file.

A frozen D: index and one owned one-file directory reproduced the production `Recovery::step` path. The directory was introduced into the loaded index as a delta overlay before timing. Across twelve unchanged checks, the baseline measured **247.73–383.03 ms**, median **343.61 ms**. The same assertions also verified that removing the owned file produces exactly one tombstone.

## Change

A persistent recent-parent cache retains immutable base-row ranges for at most **64 parents / 4,096 total ranges per source**, below **64 KiB including bookkeeping**. Cache hits inspect only those base rows and the existing parent-scoped overlay. Newly allocated parents beyond the immutable maximum parent ID need no base scan after that bound is known. Cache misses still scan immutable parent fields; eviction and fragmented layouts retain complete fallback behavior.

The cache stores row boundaries, not paths or filenames, and clears when the base generation changes. Delta overlays, staged moves, inserted children, inaccessible-directory protection, and tombstones remain authoritative. Broad recursive recovery keeps its existing temporary lookup table. Deleting new or originally empty directories avoids building a volume-wide table; moved-in base subtrees retain the full traversal.

The frozen-source production-path probe measured **0.05–0.26 ms**, median **0.058 ms**, after warming the cache. Its deletion control passed unchanged. This is a repeated one-file-folder microbenchmark, not a whole-application speedup or an Everything comparison. Query-engine production code and snapshot/log/cache formats were not changed.

## Verification and local installation

- Version remains **0.0.1**; code and documentation are English.
- Source: `b651d3a3e6c40610b8ba042662deeea9d2d11a9d`.
- Installed desktop SHA-256: `1c14161e609158cf50fa9457cd35fbe2d3806a719938ab02648affb3b7096671`.
- Formatting, strict workspace release Clippy, **87 workspace tests**, and release binaries build passed.
- New controls cover eviction, overlay children, cross-parent renames, fragmented over-budget layouts, base-generation invalidation, and a base subtree moved into a newly created directory before deletion.
- Removing generation invalidation deliberately made its deletion assertion fail; restoring it passed. Temporary probe/mutation code was removed.

The actual portable GUI/CLI and their metadata/checksums were updated after stopping the previous worker. Original binaries and pre-update logs were backed up. Normal C:/D:/E: scope, disabled sizes, background indexing, and Windows sign-in startup were preserved. The new worker reused the same saved catalog.

The first new-runtime C:/D:/E: live round passed combined/per-drive search, creation, resize with sizes unknown by configuration, file rename with retained IDs, Unicode directory move with retained descendant IDs, and deletion. All six snapshot/cache SHA-256 values and the catalog were unchanged. Disposable fixture files were removed.

## Same-disk monitoring comparison

The new worker reported no queued recovery across all three sources **24 minutes 45.52 seconds after launch**. That first observed drained interval contained **22,288,292 records** (C: 7,667,102; D: 14,469,208; E: 151,982). Coverage remains partial: the sources retain 958 baseline skipped entries and 955 rows pending refresh at this observation; those categories can overlap.

The startup/offline traversal was not the optimization target. Catalog loading measured **20.25 seconds** with accumulated update history. Its elapsed catch-up time is not a controlled comparison with the earlier 64-minute observation: filesystem caches, ambient changes, and recovery scopes differed. A restart still requires the offline-change check.

Two monitoring windows followed the drained interval. Both had the GUI closed, indexing enabled, no foreground queries or fixture mutations during sampling, and continued ambient disk activity. Each window used 61 process samples and one unchanged catalog. CPU is cumulative process CPU-time change divided by elapsed wall time, then divided by **32 logical processors** for the whole-machine column.

| Window | Duration | CPU, one core = 100% | CPU, whole machine = 100% | Private commit | Resident working set |
| --- | ---: | ---: | ---: | ---: | ---: |
| Before, old worker | 61.54 s | 70.72% | 2.21% | 1809.1–1849.4 MiB | 1511.0–1545.9 MiB |
| After, before final live round | 61.34 s | 29.88% | 0.93% | 1768.3–1808.6 MiB | 1687.8–1721.9 MiB |
| After, following final live round | 61.28 s | 25.62% | 0.80% | 1769.5–1808.8 MiB | 1687.0–1720.2 MiB |

Observed process CPU was **58–64% lower** than the fresh baseline. This is a real-worker observation under changing filesystem activity, not a controlled replay of an identical workload. The frozen-source folder probe separately isolates the removed cost.

Private commit was lower in these observations, while resident working set was higher in the fresh process. **A general RAM reduction is not established.** Process age, cache residency, and workload differed. The three persistent recent-parent caches reported **3,088 bytes combined** at final inspection; the implementation and tests cap each source below 64 KiB. Large volume-wide lookup tables are not retained by this cache.

Phase samples were 41 Monitoring / 20 Checking before, 55 / 6 in the first new window, and 59 / 2 in the second. The offline check had drained before the new windows; short live directory batches can still display **Checking saved files**. These are app-idle monitoring measurements on active volumes, not quiescent-volume idle measurements. Background CPU is substantially lower but still not negligible; repeated overlay binding and cold-parent misses remain candidates for further profiling.

## Final live verification

A second post-catch-up C:/D:/E: round passed combined and per-drive search, creation (**0.465 s**), rename with original IDs, Unicode folder moves with retained descendant IDs (**0.509 s**), and deletion (**0.727 s**). The resize fixture wrote 4,097 bytes; indexed sizes remained unknown because collection is disabled. All six snapshot/cache hashes and the catalog stayed unchanged. Owned fixtures were removed.

The worker recorded **one application queue recovery, zero backend losses**, and no new reconciliation-failure marker after launch. The retained old failure marker predates this worker. The portable package's **987 manifest entries** were verified. One updated worker remains running with the original C:/D:/E: profile and sign-in startup settings. GUI visual checks and warm-query throughput benchmarks were not rerun; production query-engine code was unchanged.

The existing [runtime installation report](RUNTIME_INSTALLATION_RESULTS.md) describes earlier notification and durable-log repairs. The public v0.0.1 tag and release ZIP remain unchanged; this installation uses the updated local build. Raw disk metadata, logs, endpoint credentials, and proof fixtures remain private.
