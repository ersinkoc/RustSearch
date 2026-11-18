# Bounded live-folder reconciliation — 0.0.1

## Observed cost

A fresh 61-sample, 61.54-second window on the same C:/D:/E: worker measured **70.72% of one core / 2.21% of the 32-thread machine**, private commit **1809.1–1849.4 MiB**, and resident working set **1511.0–1545.9 MiB**. The GUI was closed, no foreground queries were issued, size collection remained disabled, and ambient filesystem activity continued. Runtime phases included short directory checks; this is app-idle monitoring on active volumes.

Small nonrecursive folder reconciliation enumerated every live indexed row to find missing children. That also consulted the overlay map for unrelated base IDs. Routine checks therefore scaled with volume size even when the folder had one unchanged file.

A frozen D: index and one owned one-file directory reproduced the production `Recovery::step` path. Across twelve unchanged checks, the baseline measured **247.73–383.03 ms**, median **343.61 ms**. The same assertions also verified that removing the owned file produces exactly one tombstone.

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

The restart's offline-change check is still running at this verification checkpoint. Post-catch-up CPU/RAM comparison will be recorded after it drains. The startup/offline traversal was not optimized by this change.

The existing [runtime installation report](RUNTIME_INSTALLATION_RESULTS.md) describes earlier notification and durable-log repairs. The public v0.0.1 tag and release ZIP remain unchanged; this installation uses the updated local build. Raw disk metadata, logs, endpoint credentials, and proof fixtures remain private.
