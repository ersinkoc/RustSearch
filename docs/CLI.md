# RustSearch CLI

Windows-only **0.0.1** feasibility prototype. The application reads user-file metadata and writes its own snapshot, delta, derived search cache, and reports. The acceptance script changes only the fixture files it creates.

## Build and checks

The development environment uses Rust/Cargo 1.93.0, Windows 11 x64, and MSVC.

```powershell
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
cargo build --release --bins --examples --locked
```

Executable: `target\release\rustsearch.exe`.

| Crate | Responsibility |
| --- | --- |
| core | Full file references, compact metadata, lossless name storage, search keys, path reconstruction |
| windows-indexer | Win32 integration, NTFS/USN enumeration, directory walking, size probes, process counters |
| query | Sorted name groups, character/trigram summaries, extension and known-size ordering |
| storage | CRC-checked snapshots and bounded delta log, checkpoints, writer locks, private Windows DACL |
| cli | Commands, JSON reports, warm benchmarks, idle measurements |

## Indexing

```powershell
New-Item -ItemType Directory -Path .temp_files -Force

# Accessible folders, no elevation required; reparse targets are not followed.
.\target\release\rustsearch.exe index --root D:\Projects --sizes --output .temp_files\projects.rsidx

# Elevated PowerShell required for native volume access.
.\target\release\rustsearch.exe probe --volume D:
.\target\release\rustsearch.exe index --volume D: --output .temp_files\native.rsidx
.\target\release\rustsearch.exe index --volume D: --sizes --size-limit 5000 --output .temp_files\sampled.rsidx
```

`--volume` uses `FSCTL_ENUM_USN_DATA`. It fails without the required access instead of silently switching to directory traversal. It uses the existing journal without creating, deleting, or reconfiguring it. This API is metadata enumeration, not a raw MFT byte parser.

`--root` uses normal Windows directory traversal and snapshot-local synthetic IDs. Skipped entries and reparse points are reported. This mode has different coverage and performance from native enumeration.

Native `--sizes` probes the default stream's logical length. `--size-limit` selects the first eligible objects in enumeration/ID order; it is not random statistical sampling. Unprobed sizes remain unknown, not zero. Sampled results cannot describe the whole disk's space distribution.

## Search

```powershell
.\target\release\rustsearch.exe search --index .temp_files\native.rsidx --query report
.\target\release\rustsearch.exe search --index .temp_files\native.rsidx --query README --mode prefix --kind file
.\target\release\rustsearch.exe search --index .temp_files\native.rsidx --query Cargo.toml --mode exact --json
.\target\release\rustsearch.exe search --index .temp_files\sampled.rsidx --ext iso --min-size 1GiB --json
.\target\release\rustsearch.exe search --index .temp_files\sampled.rsidx --largest --limit 100 --json
```

Filters combine with AND. Path/date/regex/fuzzy search and a free-form query parser are not implemented. Name matching uses Unicode lowercase, without locale collation, normalization, or Windows ordinal equivalence. For example, uppercase dotted I folds to `i` plus a combining dot. Original UTF-16 is preserved separately; lossy display conversion is marked in JSON.

Results use ascending full file ID. `--largest` uses descending size, then ascending ID. All matches are counted; at most `--limit` rows are returned. Query timing includes counting and selection, but excludes snapshot loading, search-table preparation, path reconstruction, and UI work. This is not a time-to-first-result measurement.

The default indexed strategy uses binary lookup for exact/prefix/extension searches and distinct-name groups with character/trigram summaries for substring candidates. Actual byte matching follows when required. Summaries do not discard valid matches. `--strategy linear` retains the reference implementation for count/order checks.

Unknown sizes are excluded from size-filtered and largest-file results. `logical_size: 0` differs from `logical_size: null`. Check `unknown_size_files` and `pending_refresh` before relying on coverage.

Single ASCII letter/digit substring queries without extension/size/largest filters scan exact character bits directly in the existing group arrays. File/folder selection, full counts, top-k ordering, shadowed base rows, and overlay merging remain supported. Other queries retain the general matching paths. See [common-substring results](COMMON_SUBSTRING_RESULTS.md).

## Persistent cache and compact memory

`index` and `compact` prepare `.rsidx.search` once by default. The next separate CLI query can use a cache hit. `--no-cache` skips preparation on those commands; on `search`, `bench`, and `idle`, it rebuilds in memory without reading or writing the cache.

JSON reports include cache state, read/build/write time, delta binding time, and total preparation time. Missing, corrupt, incompatible, or wrong-generation caches rebuild. If publication fails or another builder holds the lock, valid-source search continues with an unsaved in-memory index. Source snapshot/delta corruption still stops the command.

Cache v2 stores base-row-to-group associations. Loading visits source records in ID order and reconstructs sorted posting lists. Validation covers group ranges/order, representative ID, membership/type, masks, complete coverage, extension uniqueness, known-size coverage/order, CRC, length, base identity, and EOF. Nothing is exposed to queries before validation completes.

V1 caches are fully validated and upgraded once under a cache writer lock (`upgraded`, `build_ms=0`, conversion included in `write_ms`). If conversion cannot be published, the validated old cache remains usable (`legacy_hit`). Earlier experiment binaries cannot read v2; use separate cache paths for cross-format comparisons.

The base cache is immutable across deltas. Updated old rows are masked; group counts are adjusted; current records are queried in a small overlay. `bound_sequence` identifies the committed delta view. Rename, deletion, extension/size/type changes, and insertions must not return stale base results. Overlay cost grows with accumulated changes; use explicit compaction when needed.

In memory, each base record is 40 bytes. A zero-size metadata word holds the complete signed USN; nonzero raw sizes use a separate variable-length size/USN payload. The known-size flag distinguishes zero from unknown. Original UTF-16 units use 4096-unit blocks with low bytes and sparse high-byte exceptions; dense blocks retain ordinary UTF-16. Names decode only when needed for display, paths, or persistence. Search keys remain uncompressed. This changes neither snapshot/cache formats nor search semantics. Savings depend on name content and size coverage.

Snapshot/cache reads use bounded blocks and calculate CRC during decoding. There is no memory mapping or full encoded-payload staging buffer. The compact name arena can append small deltas without copying all existing blocks; the key arena retains 64 KiB of append headroom.

## USN refresh and compaction

```powershell
# Elevated PowerShell: append a small committed delta.
.\target\release\rustsearch.exe refresh --index .temp_files\native.rsidx --sizes --report .temp_files\refresh.json

# Publish a consolidated snapshot to a new path.
.\target\release\rustsearch.exe compact --index .temp_files\native.rsidx --output .temp_files\compacted.rsidx
```

Events after the captured checkpoint are replayed. Current name/parent data is queried by file ID; size is queried separately. Missing journal history, changed journal identity, or an invalid cursor requires reconciliation/reindexing. Pending metadata failures are retained for later refresh attempts; advancing a checkpoint does not imply all metadata is current. Active-volume observations are not an atomic volume snapshot.

Each refresh appends only changed/deleted records and the checkpoint to `.rsidx.delta`. Frames carry sequence, preceding checkpoint, CRC, and a commit marker. An incomplete final frame cannot advance the view; the next writer trims that tail before appending. Corruption in a complete commit fails visibly. A nonblocking `.rsidx.lock` covers load, journal collection, and append. Success follows `sync_all`.

The log budget is 64 MiB. Reaching it requests explicit compaction to a new snapshot rather than an automatic large rewrite. Compaction merges updates and interns names/keys; it preserves the source base and log. Overwriting a base that already has a delta log is rejected. There is no automatic compaction or continuously running service.

For backup/transfer, keep the base and its matching `.delta` together. `.lock` is coordination only. `.search` is derived and optional; `.search.lock` coordinates builders. Use compaction for a single-file source export. The oldest experiment CLI did not read deltas; use the current CLI for current indexes.

## Resource measurements

```powershell
.\target\release\rustsearch.exe info --index .temp_files\native.rsidx
.\target\release\rustsearch.exe bench --index .temp_files\native.rsidx --iterations 100 --warmup 5 --verify-linear --report .temp_files\bench.json
.\target\release\rustsearch.exe idle --index .temp_files\native.rsidx --seconds 10 --report .temp_files\idle.json
```

Bench runs nine fixed query groups. Warmup is excluded. p95 requires at least 100 samples; p99 requires at least 1000. `--verify-linear` checks each full count and returned ID order outside the timed query loop. The warm query loop does not read disk. Restarting a process does not establish a cold disk cache.

CPU uses process kernel+user time, expressed relative to one core. Memory reports private commit, working set, and process-lifetime peaks; these are different measures. I/O uses process transfer counters, including IOCTL/other categories, not physical-device traffic. Report/stdout writes follow the resource sample. `idle` only measures a sleeping loaded CLI, not a continuously updating daemon or the planned ten-minute idle target.

## Real NTFS acceptance

```powershell
# Build release first; use elevated PowerShell. Project and fixture must be on D:.
.\scripts\ntfs-acceptance.ps1 -Volume D:
```

The script creates its own fixture under `.temp_files`, enumerates the full native volume, and attempts 5000 size probes. It checks initial cache-hit behavior and then rename, move, resize, create/delete, Unicode names, and zero length against independent `Get-ChildItem` results. Two refreshes must preserve base/cache SHA256 and bind sequences 1 and 2 across separate processes. This fixture does not independently prove all twelve million disk records.

`scripts/benchmark-startup.ps1` compares retained binaries in alternating new processes with fixed child affinity; it checks source, optional delta, cache hashes, counts, and sequences. Optional `-CurrentIndex` supports byte-identical sources with separate derived caches. `scripts/benchmark-hot.ps1` compares warm-query distributions across alternating processes with a linear oracle.

## Persistence and remaining limits

Snapshots and caches use atomic temporary-file publication and Windows replacement. Before writing data, an inheritance-disabled DACL grants access to the current user, SYSTEM, and Administrators. CRC32 detects accidental corruption; it is not authentication. Snapshots and result rows contain sensitive filenames and directory structure. Raw local evidence is not published.

Native coverage is one canonical name per full file reference. Additional hard links, allocated/compressed/sparse space, alternate stream totals, date/path search, GUI, folder totals, and disk-space maps remain outside the implemented scope. USN parser support is V2 only; unsupported versions fail instead of being silently misparsed. No Everything speed/RAM superiority claim has been established.

See [memory results](MEMORY_RESULTS.md), [cache validation results](CACHE_VALIDATION_RESULTS.md), and [benchmark targets](BENCHMARKS.md). API references: [USN enumeration](https://learn.microsoft.com/en-us/windows/win32/api/winioctl/ni-winioctl-fsctl_enum_usn_data), [current USN read](https://learn.microsoft.com/en-us/windows/win32/api/winioctl/ni-winioctl-fsctl_read_file_usn_data), [process memory](https://learn.microsoft.com/en-us/windows/win32/api/psapi/ns-psapi-process_memory_counters_ex).
