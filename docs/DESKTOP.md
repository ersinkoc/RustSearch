# RustSearch desktop

The Windows **0.0.1** desktop alpha uses Rust and GPUI. It shares the CLI's snapshot, delta, cache, and query implementation. No browser or WebView is used for its interface.

![Dark appearance with generated fixture files](assets/desktop-dark.jpg)

## Build and start

Use Windows 11 x64, Rust 1.93+ MSVC, and the MSVC C++ build tools. The compatible GPUI 0.2.2 and gpui-component 0.5.1 releases are pinned; build with the committed lockfile.

```powershell
cargo build -p rustsearch-desktop --release --locked
.\target\release\rustsearch-desktop.exe
.\target\release\rustsearch-desktop.exe --index .temp_files\local.rsidx
.\target\release\rustsearch-desktop.exe --no-restore --light --compact
```

A normal launch starts or connects to the per-user background indexer and automatically indexes ready local fixed drives on first use. The GUI receives bounded results from that worker and does not load a second complete index. `--no-restore` opens a standalone empty window; `--index PATH` opens a standalone read-only snapshot. These arguments are processed in order. `--light` selects light appearance. `--compact` starts at 960 × 600 logical pixels. `--data-dir PATH` isolates all settings, locks, and worker data, which is useful for acceptance fixtures. The default data directory is `%LOCALAPPDATA%\RustSearch`.

Choose **Index a folder** to scan accessible metadata and logical file sizes without elevation. Reparse targets are not followed. The app saves a private snapshot and prepares its search tables. Cancellation is cooperative: it stops traversal at an entry boundary; an operation already inside snapshot writing or cache preparation must finish that phase before the worker can stop. Source files are never changed by indexing.

Choose **Open index** for a `.rsidx` snapshot or a `.rscatalog` computer catalog. Keep each snapshot's matching `.delta` alongside it. Explicit snapshot/folder views remain separate from automatic computer indexing. The reload icon rereads the selected source. Opening an external snapshot does not enable live writes to that snapshot, and it does not stop a connected background worker from maintaining its managed computer index. CLI-native snapshots remain compatible.

## Index this computer

Open **This computer**, or choose **Index this computer** on the welcome screen. Ready local fixed drives are selected initially; removable drives can be included explicitly. Each drive shows its filesystem, type, free space, and total capacity. Refresh drives after connecting media. These capacity bars describe Windows volume space, not indexed folder totals.

- **Accessible files** scans selected drives without administrator access. Protected entries may be skipped and the scan can take considerably longer than NTFS enumeration.
- **Fast NTFS** uses native metadata enumeration on NTFS drives. Run the executable as administrator yourself when using this mode. It requires an existing USN journal, never creates one, and reports failed drives instead of silently changing the scan method. File actions inherit that process's permissions.
- **Collect logical file sizes** enables size queries and Largest files. Disable it for a faster name index. Native size collection can be expensive on large volumes; unknown sizes are never treated as zero.

The scan processes drives sequentially and publishes a separate private snapshot for each successful drive. A private `computer.rscatalog` under `%LOCALAPPDATA%\RustSearch\managed-*` records the snapshot paths and failed-drive messages. It restores as one searchable source on the next launch. The original full file ID is scoped by its source index; identical IDs on two drives remain separate results.

**All locations** searches every loaded source; its menu narrows results to one drive. Normal results are ordered by catalog source, then full ID. Largest files combines known sizes across drives, descending, with source and ID as tie breakers. Counts include every matching source, while only the requested rows are hydrated. Failed sources and skipped entries appear in a persistent partial-coverage banner; **Review coverage** opens per-location details. Cancellation leaves completed private snapshots on disk but does not publish an unfinished catalog.

This flow covers assigned local fixed/removable drive letters. Network/optical drives, volumes mounted only as folders, unmounted or locked volumes, and reparse targets are excluded. It does not guarantee access to every protected file. Add a reachable folder separately where appropriate. Manually opened snapshots are point-in-time observations. Managed sources receive live changes; accessible sources are reconciled after worker downtime.

## Background indexing and startup

**Settings** contains Start, Pause/Resume, Stop, automatic startup indexing, keep-running-after-close, sign-in startup, and logical-size collection. The selected model is a per-user worker, not a machine-wide Windows service. No service account, administrator installation, or automatic elevation is used.

- Closing the GUI leaves the worker running by default. Reopening the app reconnects to the same index owner. With keep-running disabled, the worker exits when the last GUI disconnects.
- **Start with Windows** registers the executable with `--worker` in the current user's Windows Run key. It starts without a window at sign-in. This setting is off by default; enabling it also enables keep-running. Turning keep-running off removes sign-in startup.
- **Pause** suspends live changes and cooperatively pauses an initial traversal; already-loaded search remains available. Atomic storage/cache phases finish before pausing. Resume processes queued changes. Canceling a scan leaves indexing paused until resumed or restarted.
- Managed accessible sources use one cancellable Windows notification reader per root. Each reader is armed before startup returns and rearms before delivering a batch. Local fixed/removable/RAM volumes use two 1 MiB buffers per reader; remote and unclassified volumes retain two 64 KiB buffers. Timestamp-only notifications are omitted because timestamps and contents are not indexed. Creates, deletes, resizing, and paired directory renames become committed deltas. Rename pairing stays within its originating reader. Stable directory IDs let descendants move without rewriting every descendant record. Search tables move across delta application without being reloaded or copied.
- NTFS sources use the USN checkpoint when volume access is available. The worker checks native changes on a two-second cadence and does not elevate itself. Index-storage events are excluded to prevent self-triggered update loops.
- The application retains at most 4,096 raw notifications. Queue pressure coalesces work into at most 4,096 affected folders, broadening to recursive ancestors if necessary. It reconciles their current filesystem state into deltas while retaining the loaded snapshot and cache. Recovery runs in batches of at most 32 directories, checking a 25 ms budget between directories; an individual large directory is atomic. New notifications take priority over existing background traversal. Temporary parent-row boundaries, capped at 64 MiB per volume, are released after recovery. A Windows buffer-loss completion invalidates its watched root recursively. Inaccessible or incomplete directory enumerations retain previous children and pending refresh metadata instead of treating them as deleted.
- A journal gap, an overlay checkpoint beyond the 64 MiB log budget, or a failed durable update can still require a new full scan. Ordinary accumulated history is folded into an atomic log checkpoint, preserving the base snapshot, search cache, IDs, and sequence. The managed worker retains its writer guard so unchanged durable history is validated once; a new guard or changed log metadata requires revalidation. A valid saved computer catalog is reused after worker downtime, with offline changes checked in cooperative background batches. An unreadable saved source triggers one full-scan fallback. Initial scans and recursive recovery may take substantial time on large drives. Search becomes available after source loading/preparation, including during catch-up; **Syncing** identifies this state. Counts are adjusted from committed deltas. Recovery and subtree deletion borrow parent-scoped overlay rows from the search index instead of sorting/scanning the complete overlay for each batch. The private data directory contains aggregate queue/backend counters in `watch-recovery.json` and the latest full-reconciliation failure reason in `last-reconciliation.json`; these diagnostics contain no changed-file list. `recovery-progress.json` reports cumulative checked-directory and outstanding-directory counts per catalog source at roughly five-second intervals during recovery; counts can grow as descendants are discovered.
- Startup settings and worker status live in `background.json` and `status.json`. No telemetry is sent. Status persistence follows actual index changes rather than each query. Managed cache cleanup retains current and previous published generations; unrelated files/directories are skipped.

Opening the settings page never enables sign-in startup automatically. Stop the indexer before rebuilding/replacing its running executable on Windows. For a worker-only launch:

```powershell
.\target\release\rustsearch-desktop.exe --worker
```

The normal frame is owned by Windows: minimize, maximize/restore, close, caption dragging, resizing, the system menu, and Snap use native controls. The title bar no longer relies on the GPUI component's custom caption hit-testing.

Alt+Space opens the native window menu and Alt+F4 closes the search window. The native-frame handler prevents GPUI client input from consuming caption/resize clicks or these system shortcuts.

## Application identity

The executable embeds a seven-size Windows icon (16–256 pixels), English product metadata, and version **0.0.1**. The SVG source is used in the sidebar; generic file-type symbols distinguish source code, documents, images, media, and archives without loading Shell extensions for every result. The icon is bundled into the executable, so it does not depend on a neighboring image file.

To regenerate the ICO from the deterministic vector geometry:

```powershell
New-Item -ItemType Directory -Path .temp_files -Force
rustc scripts/generate-icon.rs -O -o .temp_files\generate-icon.exe
.\.temp_files\generate-icon.exe
```

## Search and file actions

The search field matches names. Contains, Starts with, and Exact name modes combine with extension, file/folder, and minimum logical-size filters. **Largest files** sorts known sizes descending; unknown sizes are excluded. Zero length remains distinct from unknown. **Reset** clears filters and returns to All items while retaining the search text; the search field's clear button clears its text.

The list counts every match and initially returns up to 500 rows. **Show 500 more results** raises the limit in steps, up to 10,000, subject to a 4 MiB hydrated path-data budget. Only visible rows are rendered. The details pane shows location, logical size, and full file reference. It can be hidden and is automatically omitted below 1080 logical pixels wide.

Double-click or use **Open file/folder** to open the selected item. **Show in Explorer** reveals it. File actions retain the original UTF-16 path; the displayed path hides ordinary internal extended-path prefixes. Copy path uses ordinary Unicode text and reports an error for unpaired UTF-16 instead of silently changing a filename. An indexed item may have moved since the snapshot; reload a refreshed index when necessary.

| Shortcut | Action |
| --- | --- |
| Ctrl+F / Ctrl+L | Focus search |
| Ctrl+O | Open an index |
| Ctrl+Shift+O | Index a folder |
| Ctrl+J / Ctrl+K | Select next/previous result and focus the list |
| F6 | Focus the result list |
| Up / Down, with the list focused | Select previous/next result |
| Enter | Open selected item |
| Ctrl+Enter, with the list focused | Reveal selected item |
| Ctrl+Shift+C | Copy selected path |
| Escape | Clear search |
| Ctrl+Shift+T | Toggle dark/light appearance |

![Light appearance with generated fixture files](assets/desktop-light.jpg)

## Resource behavior and limits

One background thread owns each loaded source and borrowing search index once. During a computer scan, it drops each drive's scan data before scanning the next; it then loads the published sources and search tables together. RAM scales with the sum of loaded indexes. Query merging retains at most twice the requested ID limit before truncation, and at most 10,000 hydrated rows. The GUI process receives immutable statistics and bounded rows through a loopback connection authenticated by a cryptographically random credential stored in a user-private file. At most eight connections and 32 MiB per frame are accepted; authentication frames have a 2 KiB limit. The GUI does not receive a complete index. One replaceable pending request coalesces rapid input, stale generations are ignored, and one pending reply bounds the return queue. Paths are reconstructed on the worker. The worker sleeps on a condition variable when idle; UI updates are event-driven.

The footer's query time measures query execution and merging only. It excludes path hydration, delivery, painting, and source/cache loading. **Index memory** reports allocated source and search-table storage, not total process private memory or GPU memory. The overview shows size coverage among indexed files, per-location generations and skips, pending refresh count, failed drives, and source/cache preparation times.

This is a desktop alpha. Folder totals, treemaps, allocated/reclaimable space, path/date/content search, a query parser, tray/global activation, installers, and application updates are not implemented. Screen-reader support, mixed-DPI/multi-monitor behavior, and long-duration UI resource budgets still need acceptance testing. See [verification results](DESKTOP_RESULTS.md).
