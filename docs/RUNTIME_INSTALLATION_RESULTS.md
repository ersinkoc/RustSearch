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

## Checks and local build identity

Version remains **0.0.1**. The installed local desktop build contains source through `4d5b58c440c7afb9172d533d11ed44696532d6d4`, with desktop SHA-256 `47158183741d93b2b61dcaf351955602e01e25f0a376a3a5558db03934bf9786`.

- `cargo fmt --all -- --check`: passed.
- `cargo clippy --workspace --all-targets --release --locked -- -D warnings`: passed.
- `cargo test --workspace --release --locked`: **79 passed**, zero failed.
- `cargo build --workspace --bins --release --locked`: passed.

These checks cover backend/runtime behavior. GUI layout, accessibility, and visual acceptance were not repeated in this change. Existing [desktop limitations](DESKTOP.md) and the upstream dependency advisory still apply.

## Same-disk startup and live results

The repaired worker reused the same saved C:/D:/E: catalog throughout the checks; no source/catalog replacement was accepted. Cached-start observations varied as the corpus and accumulated deltas changed:

| Local build/run | Catalog load | Initial search preparation | Startup probe visible during synchronization |
| --- | ---: | ---: | ---: |
| First cooperative recovery build | 2,816 ms | 1,299 ms | 0.531 s |
| Junction/overlay follow-up | 34,650 ms | 1,862 ms | 2.395 s |
| Larger-buffer build | 35,767 ms | 1,530 ms | 2.781 s |
| Reused-overlay build | 32,680 ms | 1,429 ms | 1.585 s |
| Final subtree-range build | 31,237 ms | 1,486 ms | 1.938 s |

The probe interval starts after initial source publication; it is **not** process-to-first-result time. The initial publications used cache hits. These are single observations with different accumulated deltas/cache conditions, not controlled startup speedup benchmarks. The latest load remains expensive and is not represented by the earlier 2.8-second sample.

The final publication contained **22,150,547** records; ambient changes continued during catch-up. The live harness checked all three drives while synchronization was active:

| Assertion | Result |
| --- | --- |
| Combined/source-filtered search | 3 combined matches; 1 per drive |
| Live creation | Visible on all drives after 0.457 s |
| File rename | Original source+ID preserved on all drives |
| Unicode folder move | Descendant IDs retained; path predicate visible after 0.612 s |
| Deletion | Zero fixture matches after 1.033 s |
| Persistence | All 6 snapshot/cache SHA-256 values and catalog unchanged |

The resize fixture wrote 4,097 bytes on each drive; indexed sizes correctly remained unknown because this profile has size collection disabled. This run does not claim indexed size-change verification. The native watcher test and size-enabled regressions verify that path separately. Latencies are polling-harness observations during recovery, not percentile benchmarks. Owned fixture files/directories were removed.

## Background resource results

The first larger-buffer sample ran for **61.41 seconds**, with 61 process samples, GUI closed, and no search requests. The worker was still **Checking saved files** throughout: **100.07% of one core / 3.13% of the 32-thread machine**, private commit **1718.1–1756.1 MiB**, resident working set **1713.3–1757.3 MiB**. This is active catch-up, not idle. One catalog remained in use.

The final subtree-range build's active catch-up sample lasted **61.44 seconds**: **100.07% of one core / 3.13% of the machine**, private commit **1723.2–1775.3 MiB**, working set **1723.9–1776.4 MiB**. It also retained one catalog and never entered a full scan during the interval.

At the 18 November 2026 08:55 local checkpoint (UTC+02:00), E: had completed its saved-file traversal; C:/D: had cumulatively checked about **194,000 / 202,000 directories**, with further descendants queued. The final worker reported **zero backend losses and zero queue recoveries**. True monitoring-only CPU/RAM was not yet measured at this checkpoint. The previous worker had three application queue recoveries during local builds, coalesced into folder work without kernel loss or catalog replacement.

The earlier [overflow report](OVERFLOW_RECOVERY_RESULTS.md) measured 66.82–85.33% of one core during monitoring with size collection enabled. That profile, corpus, and phase differ from this size-disabled recovery run, so no CPU/memory improvement is claimed from those numbers. Raw disk paths, endpoint credentials, row metadata, logs, hashes, and samples remain private; only aggregate evidence is published.
