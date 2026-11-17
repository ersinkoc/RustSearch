# Feature baseline and effortless-use requirements

RustSearch targets a broader daily workflow than name search: find an item, understand its disk-space impact, and act on it from one current metadata index. Ease of use, correct coverage, low idle cost, and measured responsiveness are release requirements. Version 0.0.1 is an alpha and does not yet provide full Everything parity or demonstrated speed superiority.

The baseline is informed by the [Everything support index](https://www.voidtools.com/support/everything/), including its searching, syntax, indexes, service, customization, history, file-list, and server guides. Future comparisons must record the exact Everything version/channel/settings; a support-page feature is not a measured benchmark.

| Capability | RustSearch status |
| --- | --- |
| Fast name, extension, type, and logical-size queries | Implemented; exact/prefix/substring filters and largest files |
| Automatic local-drive indexing | Implemented for ready assigned local fixed drives, with editable scope |
| Live changes and persistence | Managed worker notifications/deltas; native journal updates where access permits |
| Background lifetime and startup | Per-user worker, pause/resume/stop, optional sign-in startup |
| Keyboard navigation and file actions | Implemented; open, reveal, copy path, bounded virtual results |
| Global activation and tray controls | Pending |
| Query operators, quoting, exclusions, wildcards, and regex | Pending |
| Path/date/property search and configurable sorting/columns | Pending; metadata model needs date/property expansion |
| Saved searches, bookmarks, history, and configurable hotkeys | Pending |
| Export/file lists | CLI JSON exists; desktop export/import pending |
| Network/remote search and server interfaces | Future scope requiring separate access and resource rules |
| Native folder totals and disk-space map | Planned product requirement; not implemented |
| Logical/allocated/reclaimable distinctions and hard-link-aware totals | Planned correctness work; no reclaimed-space claims |
| Content search | Later opt-in scope, as selected during project planning |
| Installer, updates, accessibility and mixed-DPI acceptance | Pending |

## Interaction requirements

1. A normal launch starts or reconnects to indexing without requiring a manually created snapshot.
2. Scope, progress, partial coverage, pause, and errors must remain visible. Changing scope must stay reachable during an initial scan.
3. Closing the search window must preserve background indexing when enabled. The next launch reconnects to the same index owner.
4. Live updates must preserve IDs, original UTF-16 paths, zero/unknown size distinctions, delta durability, and complete match counts.
5. The GUI receives bounded result rows; it must not duplicate the worker's full metadata/search tables.
6. Index storage must not trigger its own monitoring loop. Restart gaps and overflow must cause visible reconciliation.
7. Native window operations and keyboard paths require direct Windows verification, not only a passing build.
8. A comparative claim requires equivalent indexed scope, freshness, matching, sorting, CPU, RAM, and I/O evidence. Passing unit tests does not establish competitor superiority.

Next delivery priorities after this foundation are the query/parser and navigation gaps, followed by incremental folder totals and an integrated disk-space map. Content search remains separate so it does not impose a mandatory index cost on metadata-only use.
