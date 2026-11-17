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

By default, the app restores the last successfully opened index and appearance. `--no-restore` starts without an index; `--index PATH` selects one explicitly. These arguments are processed in order. `--light` selects light appearance. `--compact` starts at 960 × 600 instead of 1260 × 790 logical pixels. Settings and GUI-created folder indexes are stored under `%LOCALAPPDATA%\RustSearch`.

Choose **Index a folder** to scan accessible metadata and logical file sizes without elevation. Reparse targets are not followed. The app saves a private snapshot and prepares its search tables. Cancellation is cooperative: it stops traversal at an entry boundary; an operation already inside snapshot writing or cache preparation must finish that phase before the worker can stop. Source files are never changed by indexing.

Choose **Open index** for a `.rsidx` snapshot, including an existing native NTFS CLI index. Keep its matching `.delta` alongside it. The reload icon opens the latest committed view and rebinds the immutable base search cache. It does not watch the filesystem, run `refresh`, or elevate the process. Continue using the CLI for native-volume indexing and USN refresh.

## Search and file actions

The search field matches names. Contains, Starts with, and Exact name modes combine with extension, file/folder, and minimum logical-size filters. **Largest files** sorts known sizes descending; unknown sizes are excluded. Zero length remains distinct from unknown. **Reset** clears filters and returns to All items while retaining the search text; the search field's clear button clears its text.

The list counts every match and initially returns up to 500 rows. **Show 500 more results** raises the limit in steps, up to 10,000. Only visible rows are rendered. The details pane shows location, logical size, and full file reference. It can be hidden and is automatically omitted below 1080 logical pixels wide.

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

One background thread owns the source and borrowing search index. The UI receives immutable statistics and bounded result rows, without copying millions of records. One replaceable pending request coalesces rapid input, stale generations are ignored, and one pending reply bounds the return queue. Paths are reconstructed on the worker. The worker sleeps on a condition variable when idle; UI updates are event-driven.

The footer's query time measures query execution only. It excludes path hydration, delivery, painting, and source/cache loading. **Index memory** reports allocated source and search-table storage, not total process private memory or GPU memory. The overview shows size coverage among indexed files, snapshot generation, pending refresh count, and source/cache preparation times. Folder traversal can skip inaccessible entries; inspect the CLI metadata report when assessing full coverage.

This is a desktop alpha. Folder totals, treemaps, allocated/reclaimable space, path/date/content search, a query parser, tray/global activation, continuous refresh, installers, and updates are not implemented. Screen-reader support, mixed-DPI/multi-monitor behavior, and long-duration UI resource budgets still need acceptance testing. See [verification results](DESKTOP_RESULTS.md).
