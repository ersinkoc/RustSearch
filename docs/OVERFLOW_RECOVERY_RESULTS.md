# Notification overflow recovery — Windows 0.0.1

This follow-up covers the repeated full-scan failure recorded in [full-drive acceptance](FULL_DRIVE_ACCEPTANCE.md). The project version stays **0.0.1**. The existing public v0.0.1 release archive and tag are unchanged; the implementation here is a subsequent source/local-build fix.

## Cause and recovery

The original application treated its 4,096-event queue limit as lost filesystem history and discarded the prepared source. On a large C:/D:/E: scan, notifications accumulated before monitoring began; recovery immediately started another full scan. Ordinary metadata writes could also consume work without changing any indexed field. Windows buffer loss also needs explicit handling, separate from application queue pressure.

The new implementation:

- Keeps a bounded event queue, converts pressure to affected-folder invalidations, and promotes excessive folder coverage to recursive ancestors. Existing source/search allocations remain usable.
- Uses one native overlapped Windows reader per watched root, two 64 KiB buffers per reader, immediate rearming before batch delivery, explicit zero-byte/error recovery, and acknowledged I/O cancellation before releasing buffers.
- Reconciles structural notifications against current folder contents, including files created and removed before enumeration. Incomplete/inaccessible enumerations preserve known children.
- Preserves paired rename IDs, including descendants of renamed directories. Pairing is scoped per reader, and staged names override cached names within a delta.
- Gives live-update work a turn during frequent searches, with updates batched at intervals of at least 250 ms. Queries no longer have to leave a quiet polling interval before notifications can commit.
- Appends committed deltas without rewriting base snapshots or caches. Unchanged indexed metadata keeps the existing bound search tables and statistics.
- Records queue pressure separately from backend loss. A remaining durable-update failure records its reason before requesting a full scan.

A true Windows buffer loss can still require recursive enumeration of the affected root. Recovery cost therefore depends on the affected scope; this change does not promise constant-time recovery or access to protected files. Full scans after worker downtime, journal gaps, or failed durable storage remain necessary.

## Automated verification

The structural-burst regression failed on the previous queue behavior because it set the overflow flag. A Windows-shaped split rename regression also exposed lost directory identity before pairing was added. Both assertions pass with recovery enabled. Real-drive polling then exposed a separate scheduling defect: queries every 200 ms prevented the 250 ms poll from timing out, so later notifications never committed. An unchanged regression querying every 20 ms failed before the scheduling fix and passes afterward.

Release-profile checks on the implementation:

- `cargo fmt --all -- --check`: passed.
- `cargo clippy --workspace --all-targets --release --locked -- -D warnings`: passed.
- `cargo test --workspace --release --locked`: **65 passed**, zero failed.
- `cargo build --workspace --bins --release --locked`: passed.

Coverage includes 10,000 structural notifications, 12,000 affected folders, 4,200 actual files during paused background indexing, explicit backend-loss invalidation, durable replay, unchanged base/cache bytes, Unicode subtree moves, reader-scoped rename pairs, reused old names, vanished staged files, malformed notification packets, unpaired UTF-16 names, cancellation of idle native readers, and progress under continuous queries. Backend-loss flags in the deterministic regression are injected; the real-drive counters below distinguish observed loss from queue pressure.

## Real-drive acceptance

Verified on 17 November 2026 on the same Windows machine and C:/D:/E: drives as the previous report, in Accessible mode with size collection enabled and the GUI closed. Ambient Windows/application activity continued; unrelated processes were left running.

- Desktop binary SHA-256: `066840e95b5ee4e1459e3d5be5a1a1b8a497da5319e980a34c35991ea6003b3f`.
- Product/file version: **0.0.1**. Source baseline: `18fefbd`, plus the accompanying implementation and regression tests.
- First complete publication: **21,934,188 records**, 19,020,669 files, 2,913,519 folders. Coverage remains limited to reachable metadata; protected entries and reparse targets are not claimed.

| Drive | Published records | Skipped entries | Reparse targets not followed |
| --- | ---: | ---: | ---: |
| C: | 7,608,110 | 951 | 242,621 |
| D: | 14,174,092 | 5 | 146,707 |
| E: | 151,986 | 3 | 0 |

The startup harness created and deleted **5,000 disposable files** while the full scan was running. The worker recorded **one application queue recovery and zero backend-loss notifications**. It completed one full C:/D:/E: scan and retained that publication through all live checks and the resource window. There were **zero full-reconciliation progress events**. Genuine kernel-buffer loss is covered by deterministic invalidation tests; it was not observed in this real-drive run.

| Live assertion | Result |
| --- | --- |
| Combined search / source-filtered search | 3 combined matches; 1 on each drive |
| Known zero-length files | Correct on all three drives |
| Create / resize to 4,097 bytes | Correct on all three drives |
| Rename files | Correct on all three drives; IDs preserved |
| Rename directories to a Unicode name | Descendant paths updated on all three drives; IDs preserved |
| Delete files and subtrees | Zero remaining fixture matches |
| Snapshot/cache integrity | All 6 SHA-256 values unchanged; catalog unchanged |

The assertions used repeated queries roughly 200 ms apart, exercising the scheduling defect that failed before the fix. Creation became visible on all drives after 5.95 seconds, resizing after 1.45 seconds, the Unicode directory-path predicate after 1.01 seconds, and final deletion after 2.56 seconds. These are single batch observations from a polling correctness harness during background activity, not per-drive latency percentiles or search benchmarks. Ordinary updates are not yet guaranteed to appear instantly.

All disposable fixture files and their owned directories were removed. Saved C:/D:/E: scope, size collection, background indexing, and sign-in startup preferences were preserved. The updated worker remains running. Raw aggregate logs, hashes, and process samples stay in the ignored local acceptance directory; real disk metadata is not published.

## Background resource measurement

Two windows sampled the same worker 61 times each, approximately once per second. The GUI was closed, no foreground search requests were issued during sampling, and indexing remained enabled. Both windows stayed in **Monitoring changes**, with one unchanged catalog and no full rescans. Ambient filesystem activity continued, so these are **app-idle background-monitoring measurements**, not quiescent-volume idle measurements.

CPU is the change in cumulative process CPU time divided by elapsed wall time. Whole-machine CPU additionally divides by the verified **32 logical processors**. Private memory means private committed bytes; working set means resident process memory.

| Window | Duration | CPU, one core = 100% | CPU, whole machine = 100% | Private commit | Resident working set |
| --- | ---: | ---: | ---: | ---: | ---: |
| After live checks | 61.55 s | 66.82% | 2.09% | 1826.4–1826.9 MiB | 1737.1–1741.8 MiB |
| Later settled window | 61.65 s | 85.33% | 2.67% | 1826.9–1828.0 MiB | 1743.7–1746.3 MiB |

**Background CPU remains too high for a low-resource idle claim.** The second window did not fall toward zero after settling. These results establish continuous monitoring without overflow-driven source replacement; they do not establish a negligible CPU budget. A quiescent-volume interval was not available, and further reduction of metadata/reconciliation work remains necessary.

The sampled process-lifetime peak working set was **2,004.7 MiB**; Windows `PeakPagedMemorySize64` reported **2,342.2 MiB**. These include startup scanning/preparation and are not idle interval averages. Source/search allocations were **1,753.65 MiB** at first publication and **1,776.54 MiB** after live updates; that internal allocation metric is separate from process commit/residency.

The previous released worker never established a stable full-drive idle interval, so there is no comparable pre-fix idle baseline. Its 1.41–1.52% whole-machine CPU windows were active partial C: scans; comparing their 328–547 MiB working sets with a loaded three-drive catalog would be misleading. Its lifetime peak working set was 1,996.7 MiB and its loaded source allocation was 1,746.21 MiB. The physical drives are the same, but the ambient corpus changed from 21,863,572 to 21,934,188 published records. No memory reduction or CPU speedup is claimed by this fix.

## Publication and remaining limits

The source fix and English documentation are on `main`; the existing v0.0.1 tag and release ZIP retain their original bytes. The new local worker was exercised directly through its authenticated protocol; GUI layout/interaction, sign-in/reboot startup, and long-duration resource behavior were not revalidated in this backend change. Existing protected-file, reparse, native-volume-access, and upstream dependency limitations still apply.
