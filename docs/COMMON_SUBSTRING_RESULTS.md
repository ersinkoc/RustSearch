# Common ASCII substring scan — 17 November 2026

Version remains **0.0.1**. The previous [memory-layout comparison](MEMORY_RESULTS.md) reported a roughly 3% slowdown in the common one-character substring benchmark. A direct scan of existing exact character bits removes that regression without adding persistent query data.

On the same 12,285,803-record D: corpus, the common `a` query changed from **12.04 to 6.01 ms**, a **50.1% reduction**. With two committed deltas on the separate 12,274,534-record corpus: **12.17 to 6.88 ms**, a **43.5% reduction**. These are fresh baseline reruns, not ratios against timing values from another session.

## Implementation

After Unicode lowercase, a single ASCII letter or digit has a dedicated exact bit in the validated character summary. For contains-mode queries without extension, minimum-size, or largest-file filters, the engine scans the existing group/summary arrays directly. File/folder filtering, full counts, ascending full-ID ordering, limits, and the top-k heap are preserved.

The clean-base loop omits delta operations entirely. The delta-bound loop subtracts removed group counts and skips shadowed postings; the existing overlay merge then handles current records. A shadowed group minimum remains a conservative lower bound for top-k pruning. Punctuation, multi-byte Unicode queries, multiple characters, exact/prefix modes, and the excluded filter combinations retain the general matching paths. The change does not make hashed punctuation bits exact or skip required name verification.

There are no added per-name/per-character indexes, caches, bitsets, or persistent arrays. Source, cache, and delta formats are unchanged; existing cache files are reused byte-for-byte. The specialization also avoids the generic boxed candidate iterator and the initial result-vector allocation for this query class. It does not optimize arbitrary common multi-character substrings.

## Protocol

Windows 11 Pro build 26300; Ryzen 9 9950X3D; Samsung SSD 990 EVO Plus 4 TB NVMe on D: NTFS; approximately 126 GiB visible RAM; Rust 1.93/MSVC release. Each variant runs in five new processes, alternating order, child affinity mask 1. Each process uses five warmups and 100 measured queries per group; the table is the median of the five process p50 values. Full count and returned ID order are checked against the linear reference outside the timed loop.

OS/file cache is not flushed. Warm core query timings exclude load, preparation, path materialization, and UI. These are not cold-SSD results. No process-level p95/p99 is inferred from five processes. All nine groups are reported, including slower microsecond results.

## Frozen corpus without deltas

12,285,803 records; base SHA256 and cache SHA256 stayed unchanged. The `a` query still counts all matches and returns at most the first 100 IDs.

| Case | Baseline p50, ms | Direct scan p50, ms |
| --- | ---: | ---: |
| rare_contains | 5.0742 | 5.0456 |
| common_contains | 12.0431 | 6.0063 |
| no_match | 4.8404 | 4.7171 |
| prefix | 0.0075 | 0.0068 |
| exact | 0.0022 | 0.0022 |
| extension | 0.0149 | 0.0200 |
| large_files | 0.0004 | 0.0005 |
| largest_100 | 0.0005 | 0.0006 |
| all | 0.0002 | 0.0003 |

Common-query process p50 values ranged **11.471–13.185 ms** before and **5.721–6.320 ms** after. The improvement exceeds the original slowdown. Rare/no-match queries stayed close; extension lookup moved by about five microseconds. Tiny near-resolution cases should not be interpreted through percentage ratios.

| Metric | Baseline | Direct scan |
| --- | ---: | ---: |
| Private commit | 887,259,136 bytes | 887,259,136 bytes |
| Process peak private commit | 887,259,136 bytes | 887,259,136 bytes |
| Working set | 887,431,168 bytes | 887,447,552 bytes |

The allocated source arena is exactly **705,178,162 bytes** and the search tables **173,439,384 bytes** in both variants. No indexed data was added. The measured process private and peak medians are equal; working-set variation is reported above rather than treated as a memory-layout change.

## Same retained source with two deltas

12,274,534 current records; sequence 2; 22 live overlay records; 7 shadowed base rows. Both variants read the same base/delta/cache bytes and preserve all hashes, counts, and sequences.

| Case | Baseline p50, ms | Direct scan p50, ms |
| --- | ---: | ---: |
| rare_contains | 5.0594 | 5.0120 |
| common_contains | 12.1721 | 6.8829 |
| no_match | 4.7182 | 4.6925 |
| prefix | 0.0075 | 0.0068 |
| exact | 0.0023 | 0.0023 |
| extension | 0.0166 | 0.0162 |
| large_files | 0.0005 | 0.0003 |
| largest_100 | 0.0092 | 0.0093 |
| all | 0.0004 | 0.0006 |

| Metric | Baseline | Direct scan |
| --- | ---: | ---: |
| Private commit | 899,063,808 bytes | 899,055,616 bytes |
| Process peak private commit | 899,088,384 bytes | 899,067,904 bytes |
| Working set | 898,609,152 bytes | 898,654,208 bytes |

Allocated source arena: **704,252,173 bytes** in both variants. Search tables plus delta structures: **186,204,288 bytes** in both. The delta query includes the existing overlay costs; no source row is silently dropped to improve timing.

## New-process startup and memory cross-check

Five alternating new processes per variant, identical zero-match query and limit 0. Values are median (min–max). That multi-character query uses the unchanged general path; this round does not target source/cache preparation.

Without deltas:

| Metric | Baseline | Direct scan |
| --- | --- | --- |
| Source load | 0.349 s (0.313–0.376) | 0.355 s (0.339–0.443) |
| Cache preparation | 0.747 s (0.725–2.952) | 0.866 s (0.774–1.081) |
| End-to-end CLI | 1.265 s (1.126–3.449) | 1.341 s (1.243–1.865) |

With two deltas:

| Metric | Baseline | Direct scan |
| --- | --- | --- |
| Source load | 0.349 s (0.330–0.364) | 0.379 s (0.359–0.430) |
| Cache preparation | 0.841 s (0.723–0.849) | 0.836 s (0.821–0.870) |
| End-to-end CLI | 1.267 s (1.170–1.296) | 1.318 s (1.291–1.348) |

Main-corpus startup private commit median is **887,255,040 bytes in both variants**. Delta startup private commit is **899,063,808 → 899,059,712 bytes**. Process peak/resident counters can vary by a few pages even when arena sizes match. Startup timings show run-to-run variation, including a 3.449-second baseline outlier in cache preparation; no startup speedup is claimed. All measured commands hit cache with zero build/write time.

## Verification

- **41 tests passed**, none failed or ignored; zero doc-tests are not counted as additional tests.
- The new public differential test checks ASCII lowercase/uppercase letters and digits, punctuation controls, duplicate names, file/folder filters, count-only and top-k limits, optional size/extension/largest filters, and two committed in-memory delta generations.
- Existing cache-backed differential tests exercise the fast path across persisted round trips, renamed/deleted/created records, type/size changes, and interrupted tails. Unicode and general-query control cases remain enabled.
- Twenty new-process warm benchmarks cover **180 query-group oracle checks** across both corpora. Twenty startup samples check stable hashes/counts/sequences. Full counts and returned ID ordering match the linear engine.
- `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --locked -- -D warnings`, `cargo test --workspace --locked`, and `cargo build --release --bins --examples --locked` passed on the measured code.

Fresh native D: mutation acceptance **passed with 12,288,751 initial records** using the same measured executable. Rename, move, resize, create/delete, Unicode names, and zero size matched independent directory enumeration. Separate queries hit the immutable base cache at sequences 0, 1, and 2. Base/cache SHA256 stayed unchanged, the two-frame delta log was 215,895 bytes, and pending refresh was zero.

After both real-disk deltas, all nine native query groups also passed the linear count/ID-order oracle. This additionally exercises the common ASCII path on the fresh volume's actual committed overlay. The active-disk acceptance corpus differs from the two frozen timing corpora; it is not used for speed ratios.

Hosted CI, power loss, a long-running service, other hardware, and GUI are outside this verification. Full size coverage and additional hard-link names remain unimplemented.

## Reproduce and provenance

```powershell
cargo build --release --bins --examples --locked
.\scripts\benchmark-hot.ps1 `
  -Index .temp_files\validation-20261117\current.rsidx `
  -BaselineExe .temp_files\common-20261117\baseline.exe `
  -RunDirectory D:\Codebox\RustSearch\.temp_files\common-new-hot -Trials 5
# Repeat with .temp_files\validation-20261117\delta.rsidx for the delta corpus.
# Elevated PowerShell:
.\scripts\ntfs-acceptance.ps1 -Volume D:
```

Local raw metadata evidence is excluded from the public repository: [main query comparison](../.temp_files/common-20261117/hot/summary.json), [delta query comparison](../.temp_files/common-20261117/delta-hot/summary.json), [startup](../.temp_files/common-20261117/startup/startup-summary.json), [delta startup](../.temp_files/common-20261117/delta-startup/startup-summary.json), [real NTFS acceptance](../.temp_files/ntfs-common-20261117/acceptance.json).

Baseline source commit: `50142d1666c5a81927c1735d4879f0c361afa549`.

Baseline executable SHA256: `4085B45F58CFDBCD87BCD7FD59461B67CAD123C3C5FD5A79BCB95C259E57F7C2`.

Measured/delivered 0.0.1 SHA256: `671859EDD38F1928824F8BBCEBBFF66543EA69542CF728E818DE18BAF4A5E63E`. A copy of the measured executable is retained locally alongside the inputs, logs, and hash manifests.
