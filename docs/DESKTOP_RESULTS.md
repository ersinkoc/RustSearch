# Desktop alpha verification

Verified on 17 November 2026 on Windows 11 x64 with the MSVC Rust toolchain. All workspace packages remain **0.0.1**. This is local desktop acceptance; it is not an installer release or an Everything performance comparison.

## Corrections during visual review

- Gave the virtual list and rows their full available width, keeping Name, Location, Type, and Size aligned and selection backgrounds consistent.
- Simplified ordinary extended drive and UNC prefixes for display while preserving original UTF-16 paths for file actions. Device/volume namespaces retain their meaning.
- Used the source folder name in the sidebar and made the name-search scope explicit in the placeholder.
- Cleared stale operation notices when beginning another folder scan.
- Resolved strict Clippy findings without suppressing lint rules.

## Observed desktop behavior

The generated, disposable folder on D: contains 762 indexed entries: 758 files and four folders. It includes 620 Rust files, twelve SVG files, 120 planning notes, Unicode names, a zero-length file, and a 3 MiB file. Screenshots show only this generated fixture. Raw real-disk indexes and private result lists are not published.

| Check | Observed result |
| --- | --- |
| Open a CLI-generated snapshot | 762 matches; first 500 hydrated rows |
| Contains `module_06` | Ten matches, `search_module_060.rs` through `069.rs` |
| Starts with `module_06` | Zero matches and a readable empty state |
| Reset filters from the empty state | Ten substring matches restored; text retained |
| Ctrl+J, then Down | Selection and inspector moved to rows 061 and 062 |
| Extension `svg` | Twelve matching SVG rows |
| Largest files | 758 known-size files, descending size; 3 MiB file first |
| Minimum size 1 MiB | Only the 3 MiB file |
| Show 500 more | All 762 fixture rows available; expansion button removed |
| Scroll the list | New rows render within the same fixed viewport |
| Copy path | Visible success notice |
| Index overview | 762 total, 758 files, four folders, 100% indexed-file size coverage, generation zero |
| Dark/light switch | Both appearances preserve readable text, selected rows, menus, inputs, and details |
| Native folder picker and scan | Fixture scanned and saved under the local application data directory; 762 entries and twelve retained SVG matches |
| Compact window, 960 × 600 logical pixels | Ten `module_06` matches in both appearances; aligned columns and visible footer; inspector automatically hidden |

![Dark desktop acceptance fixture](assets/desktop-dark.jpg)

![Light desktop acceptance fixture](assets/desktop-light.jpg)

![Compact desktop acceptance fixture](assets/desktop-compact.jpg)

## Automated validation

```powershell
cargo fmt --all --check
cargo clippy --workspace --all-targets --release --locked -- -D warnings
cargo test --workspace --release --locked
cargo build --workspace --release --bins --examples --locked
```

The workspace suite passes **45 tests**, with zero failures, ignored tests, or filtered tests. The desktop library contributes three tests: source-error recovery/latest-query delivery, two persisted delta generations with an unchanged base cache, and Unicode/namespace path presentation. The Windows indexer adds a controlled-walk cancellation check that leaves source files unchanged. Existing query, cache, snapshot, delta, UTF-16, zero/unknown-size, junction-cycle, and CLI integration checks also pass.

The worker's delta test loads the base, commits a rename/resize plus deletion/insertion, reloads generation one, then commits a file-to-folder change and deletion and reloads generation two. Returned IDs, names, kinds, sequence, and cache-hit state are asserted; base cache bytes remain identical. This is a deterministic persisted fixture test, separate from the previous real-NTFS acceptance reports.

## Limits of this verification

The desktop stderr capture was empty during the fixture session. Screen-reader support, multiple monitors, mixed DPI, remote/removable paths, long-duration memory/CPU behavior, and GUI-inclusive latency distributions were not accepted by this run. The previous approximately 887 MB CLI measurement is not a total desktop/GPU memory measurement. Footer query timings exclude path hydration and rendering and must not be presented as input-to-visible latency.

Folder maps and incremental folder totals remain planned. See the [desktop guide](DESKTOP.md) for supported behavior and the [benchmark protocol](BENCHMARKS.md) for future performance acceptance.
