# Full-drive acceptance — Windows 0.0.1

Follow-up: [overflow recovery and live-update results](OVERFLOW_RECOVERY_RESULTS.md) documents the subsequent source fix. This report preserves the original released binary's failed acceptance result.

Verified on 17 November 2026 on the running Windows x64 release worker, with the GUI closed. Version remains **0.0.1**. This is a runtime verification report; no application code, release tag, or published binary was changed.

**Outcome: snapshot search passes on C:, D:, and E:, but continuous full-drive monitoring fails to settle. Live-update acceptance and a stable idle resource budget are not established.**

## Scope and binary identity

The per-user worker was running in Accessible mode with C:/D:/E: selected, logical-size collection enabled, and sign-in startup enabled. The current executable's SHA-256 matches the downloaded public release executable:

- Source/release commit: `cefdc1cac7acfba0e9643a39d366b84de5ba9f30`
- Desktop SHA-256: `a31c8c1e0e1626cc9e5d75f6bddd78313e9786c74168ad6d468c62057399ba92`

A complete three-snapshot catalog was published. The observed publication contained the following reachable metadata; protected entries and reparse targets remain outside the coverage guarantee.

| Drive | Records | Files | Folders | Skipped entries | Reparse targets not followed | Files with unknown size |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| C: | 7,632,620 | 6,196,058 | 1,436,562 | 951 | 242,621 | 126,421 |
| D: | 14,078,966 | 12,631,001 | 1,447,965 | 5 | 144,930 | 23 |
| E: | 151,986 | 134,190 | 17,796 | 3 | 0 | 0 |

Total: **21,863,572 records**, 959 skipped entries, and 126,444 files with unknown logical size. No claim of access to every protected file is made.

## Search checks

For each published drive snapshot, the released CLI selected an indexed file that still existed on the real filesystem, then ran exact-name searches using both the prepared index and the linear reference. Full match counts and returned file IDs agreed on every drive. All three prepared queries reused their existing cache. Actual filenames and paths are omitted from this public report.

| Drive | Exact matches | Indexed/linear count and IDs | Cache |
| --- | ---: | --- | --- |
| C: | 1 | Equal | hit |
| D: | 3 | Equal | hit |
| E: | 1 | Equal | hit |

These are correctness probes of published snapshots, not a latency benchmark or an accepted federated interactive-worker session.

## Full-drive monitoring failure

A scan completed and briefly published a live-marked source with all three volumes, then the worker immediately returned to scanning C:. The same behavior occurred after an explicit reload of the published managed catalog. The worker reported:

```text
Reconciling after filesystem notification overflow
```

Reloading reused the caches and returned the full catalog, but did not restore sustained monitoring. This is a failed full-drive monitoring check, not evidence that the source stayed current. The current in-memory notification queue is bounded at 4,096 retained events. The observed error does not distinguish queue capacity, a backend rescan flag, or a callback error; those triggers need separate instrumentation before assigning a narrower root cause.

Disposable baseline and zero-length files were created in owned directories on all three drives, outside the excluded index-storage directory. The mutation harness required a stable monitoring state before checking combined/per-drive results, creation, resize, file/directory rename, and deletion. Reconciliation prevented it from reaching that stage. **Those full-scope live assertions did not pass or complete.** Earlier small-fixture live-update tests remain separate evidence and do not establish acceptance at this scale.

Only the known disposable files and their empty owned directories were removed. The worker and the user's selected scope/startup preferences were retained. No user file was renamed, modified, or deleted. Raw proof logs and counters remain local under the ignored worker acceptance directory.

## Background CPU and memory

Two active C: scan windows sampled the same worker 31 times at approximately one-second intervals. No GUI was running. CPU is the change in cumulative process CPU time divided by elapsed wall time. Machine-normalized CPU additionally divides by the verified **32 logical processors**. These windows measure active scanning with ambient filesystem activity, not idle monitoring.

| Active-scan window | Duration | CPU, one core = 100% | CPU, whole machine = 100% | Private committed memory | Resident working set |
| --- | ---: | ---: | ---: | ---: | ---: |
| Initial observation | 30.78 s | 45.07% | 1.41% | 434.3–652.6 MiB | 328.5–461.8 MiB |
| After reload/reconciliation | 30.85 s | 48.73% | 1.52% | 650.7–842.1 MiB | 429.0–547.3 MiB |

The Windows process-lifetime peak working set observed during this session was **1,996.7 MiB**. `PeakPagedMemorySize64` reported 2,339.7 MiB; this is a peak Windows commit counter, not resident RAM. These lifetime peaks include prior scan/load/reconciliation phases and are not interval averages.

The cache-reloaded catalog reported **1,746.21 MiB** allocated to source/search structures. This allocation metric is distinct from process private commit and working set, and it is not an idle measurement.

No stable full-drive idle interval was available, so no idle CPU/RAM number is reported. The existing approximately 887 MB CLI result covers a different 12.29-million-record corpus and is not a memory budget for this approximately 21.86-million-record multi-drive worker.

## Required follow-up

The blocker is the repeated recovery cycle after reported notification overflow. Before full-drive acceptance can pass, pending changes during long scans must be handled without losing correctness or immediately discarding the newly prepared source. Recovery diagnostics should retain the reason and distinguish backend loss from queue saturation. Then repeat the same three-drive live mutations and measure an uninterrupted monitoring interval. The published v0.0.1 artifact remains unchanged; this report does not claim a fix.
