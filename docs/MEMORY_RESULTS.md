# Compact metadata memory — 17 November 2026

Version remains **0.0.1**. On the same frozen 12,285,803-record D: corpus, median private commit fell from **1,086,283,776 to 887,255,040 bytes**: **18.3% less**, or **189.8 MiB saved**. Working set and process peak private commit fell as well. This is an actual reduction of stored heap data, not a change to memory mapping or working-set trimming.

## Data layout

- Per-file records are **40 bytes instead of 48**: an 8-byte saving per record, **98,286,424 bytes** on the main corpus. Complete 64-bit IDs and parents, name/key references, and public flags remain intact.
- A zero raw size stores the full signed USN inline. Nonzero raw sizes use a separately referenced variable-length size/zigzag-USN pair. The known-size flag remains independent: unknown and known zero are distinct. Full u64 sizes and signed i64 USNs, including extrema, survive. Even an unknown-but-nonzero raw persisted size round-trips unchanged.
- Original UTF-16 uses 4096-unit blocks. Narrow blocks store low bytes plus sparse indexed high bytes; dense non-Latin blocks stay ordinary UTF-16. All code units, including unpaired surrogates, survive. Decoding occurs for display/path/persistence, not the timed name-query loop. No filenames are removed or case-folded in this original-name store.
- Normalized search keys remain directly accessible. Character/trigram summaries use a separate dense array so rejected candidates need not load all group metadata. The combined table payload is unchanged: 24-byte group + 16-byte summaries replaces the original 40-byte group.
- Snapshot v1, cache v2, and the delta protocol remain unchanged. Existing cache bytes are reused without migration, rebuilding, or publication. CRC, range, ID, membership, mask, coverage, ordering, and EOF validation remain enabled.

The source's original name arena has **106,456,537 UTF-16 units**, and the key arena **106,347,974 bytes**. Name compression and variable metadata savings depend on actual character content and size coverage. This corpus has only **5499 known-size files**; it does not establish a memory budget for a fully sized volume. Worst-case values and dense Unicode may compress less. First index/cache construction and compaction have temporary interning/sort/publication allocations; the tables below measure new-process cache-hit search, not those build peaks.

## Measurement method

Windows 11 Pro build 26300, AMD Ryzen 9 9950X3D, Samsung SSD 990 EVO Plus 4 TB NVMe, D: NTFS, approximately 126 GiB visible RAM; Rust 1.93/MSVC release. Each variant used five new processes in alternating order, child affinity mask 1 (CPU0), the same zero-match query and limit 0. Both read exactly the same source and cache files. Base/cache hashes stayed fixed; all samples were hits with zero build/write time.

Times are median (min–max). OS/file cache was not flushed; these are not cold-SSD results. No startup p95/p99 is inferred from five processes. Process transfer counters are not physical disk I/O. Earlier historical timing values are not mixed into the new comparison: the retained binary was rerun in this session.

## Main frozen corpus: 12,285,803 records, no delta

| Measurement | Retained 0.0.1 | Compact 0.0.1 |
| --- | --- | --- |
| Private commit | 1,086,283,776 bytes (1036.0 MiB) | 887,255,040 bytes (846.2 MiB) |
| Process peak private commit | 1,086,283,776 bytes (1036.0 MiB) | 887,255,040 bytes (846.2 MiB) |
| Working set | 1,089,224,704 bytes (1038.8 MiB) | 887,406,592 bytes (846.3 MiB) |
| Snapshot + delta load | 0.287 s (0.275–0.308) | 0.326 s (0.323–0.334) |
| Cache preparation | 0.782 s (0.732–0.896) | 0.718 s (0.689–0.799) |
| End-to-end CLI | 1.150 s (1.113–1.295) | 1.132 s (1.099–1.225) |
| Process CPU time | 1.047 s (0.984–1.203) | 1.047 s (0.984–1.125) |

Snapshot loading does slightly more work to encode names; cache preparation benefits from the smaller records/layout. Overall CLI startup stayed close: **1.150 → 1.132 s** in this run. Read transfer remains **1,082,419,459 bytes** per variant. The source remains 908,979,979 bytes and cache 173,439,480 bytes.

## Warm query speed

Five alternating processes per variant, each with five warmups and 100 measurements per group. The table gives the median of the five process p50 values. Full counts and returned ID order were checked against each binary's linear reference outside timing. All nine cases are retained; no slower group was removed.

| Case | Before p50, ms | After p50, ms |
| --- | ---: | ---: |
| rare_contains | 5.4870 | 5.1870 |
| common_contains | 11.3152 | 11.6696 |
| no_match | 5.7812 | 4.9695 |
| prefix | 0.0065 | 0.0075 |
| exact | 0.0023 | 0.0020 |
| extension | 0.0151 | 0.0204 |
| large_files | 0.0003 | 0.0004 |
| largest_100 | 0.0006 | 0.0006 |
| all | 0.0002 | 0.0003 |

Rare substring improved about 5.5%, and no-match about 14%. The common one-character case was **3.1% slower** (about 0.35 ms). Exact name improved; prefix/extension differences are approximately 1–5 microseconds. Large-file and all-record microbenchmarks are near timer resolution; percentage ratios there would be misleading. Search remains in the same practical latency range, but this is not a claim that every case is identical or faster.

The ordinary indexed query loop performs no name decompression or disk I/O. Path/name materialization is separate and may allocate a short decoded buffer; it is not included in warm core query timings. Separate native fixture checks cover result paths and Unicode display.

## Separate retained corpus with two committed deltas

**12,274,534 current records**, sequence 2, 22 live overlay records, 7 shadowed base rows. Same five-process protocol and byte-identical inputs, with stable base/delta/cache hashes. This is a separate corpus, not a direct timing comparison with the no-delta table.

| Measurement | Retained 0.0.1 | Compact 0.0.1 |
| --- | --- | --- |
| Private commit | 1,097,846,784 bytes (1047.0 MiB) | 899,063,808 bytes (857.4 MiB) |
| Process peak private commit | 1,097,863,168 bytes (1047.0 MiB) | 899,076,096 bytes (857.4 MiB) |
| Working set | 1,099,264,000 bytes (1048.3 MiB) | 886,165,504 bytes (845.1 MiB) |
| Snapshot + delta load | 0.285 s (0.281–0.296) | 0.327 s (0.320–0.341) |
| Cache preparation | 0.780 s (0.752–0.806) | 0.684 s (0.684–0.738) |
| End-to-end CLI | 1.175 s (1.140–1.251) | 1.147 s (1.092–1.168) |
| Process CPU time | 1.062 s (1.016–1.094) | 1.016 s (0.984–1.047) |

Delta-inclusive private commit fell about **18.1%**. No whole name arena is recopied for a tiny delta: existing blocks remain stable, new units append through the tail, and staged metadata payload offsets are rebased before publication.

| Case | Before p50, ms | After p50, ms |
| --- | ---: | ---: |
| rare_contains | 5.6332 | 5.1810 |
| common_contains | 11.3523 | 11.8335 |
| no_match | 5.8407 | 5.0863 |
| prefix | 0.0067 | 0.0074 |
| exact | 0.0025 | 0.0021 |
| extension | 0.0162 | 0.0227 |
| large_files | 0.0002 | 0.0004 |
| largest_100 | 0.0095 | 0.0101 |
| all | 0.0006 | 0.0006 |

Rare/no-match queries improved; the common case was about 4.2% slower (0.48 ms), and extension lookup differed by about 6.5 microseconds. Size-filtered cases still apply only to known sizes. All counts/orders matched the linear oracle.

## Correctness and checks

- **40 tests passed**, no failed/ignored tests. Zero doc-tests are not added to that count.
- UTF-16 tests cover block edges, cross-block slices, sparse high bytes, dense fallback, every possible u16 value, unpaired surrogates, and appending at a partial tail.
- Metadata tests cover i64/u64 extrema, unknown versus known zero, nonzero sizes, updates, interning, and consolidation. An existing differential query test exposed a missing overlay metadata rebase during the refactor; it passed after both base and overlay references were handled.
- A storage test saves, reloads, and saves again, requiring **byte-identical snapshots**, including negative USN, Unicode blocks, and unknown raw size metadata.
- The full real corpus comparison verified **12,285,803 logical records**: IDs, parents, USNs, raw sizes, flags, UTF-16 names, and search keys unchanged between retained dense and interned snapshots.
- Twenty final startup samples and twenty final warm-benchmark processes passed hash/count/sequence checks. Across the two corpora, **180 query-group oracle checks** passed, with 100 measured queries per group per process.
- `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --locked -- -D warnings`, `cargo test --workspace --locked`, and `cargo build --release --bins --examples --locked` passed.

Fresh native D: acceptance **passed with 12,287,026 initial records** using the measured implementation binary. Rename, move, resize, create/delete, Unicode names, and zero size matched independent directory enumeration. Separate processes hit the unchanged cache at sequences 0, 1, and 2; base/cache SHA256 remained fixed. The delta log was 25,754 bytes and pending refresh was zero. This active-disk corpus differs from both frozen benchmark corpora and is not used for speed ratios.

The retained previous 0.0.1 executable also read that newly written snapshot, cache, and sequence-2 delta view: cache hit and all fixture paths/sizes matched the measured executable. The fresh index command's peak private commit was **1,985,007,616 bytes (about 1.85 GiB)**; total wall time was 88.739 s. Initial construction therefore remains substantially more expensive than cache-hit search. No controlled before/after initial-index speed or peak-memory ratio is claimed from differing active-disk runs.

Hosted CI, physical power loss, long-running daemon, full-volume size coverage, GUI, and other hardware are not verified by this round.

## English documentation and version

README, CLI guide, product plan, benchmark protocol, and every historical result report were rewritten in English while retaining historical measurement tables and provenance. Non-English filenames remain intentionally in Unicode test fixtures. Package versions stay 0.0.1. No release tag or version bump is needed.

## Reproduce

```powershell
cargo build --release --bins --examples --locked
.\scripts\benchmark-startup.ps1 `
  -Index .temp_files\validation-20261117\current.rsidx `
  -BaselineExe .temp_files\memory-20261117\baseline.exe `
  -RunDirectory D:\Codebox\RustSearch\.temp_files\memory-new-startup -Trials 5
.\scripts\benchmark-hot.ps1 `
  -Index .temp_files\validation-20261117\current.rsidx `
  -BaselineExe .temp_files\memory-20261117\baseline.exe `
  -RunDirectory D:\Codebox\RustSearch\.temp_files\memory-new-hot -Trials 5
# Elevated PowerShell for real-volume mutation acceptance:
.\scripts\ntfs-acceptance.ps1 -Volume D:
```

Local evidence is excluded from the public repository: [startup](../.temp_files/memory-20261117/accepted/startup/startup-summary.json), [delta startup](../.temp_files/memory-20261117/accepted/delta-startup/startup-summary.json), [warm queries](../.temp_files/memory-20261117/accepted/hot/summary.json), [delta queries](../.temp_files/memory-20261117/accepted/delta-hot/summary.json), [all-record comparison](../.temp_files/memory-20261117/logical-records.txt), [native acceptance](../.temp_files/ntfs-memory-20261117/acceptance.json). Exploratory runs are retained separately; only the `accepted` runs supply the measured implementation binary and numbers in these tables.

Baseline SHA256: `C44E3B67D96299B47D9396997D425C02274319D031C504B8F09A02DE492ED344`.

Measured implementation 0.0.1 SHA256: `E4C90911FA5C134755E3FBC2A70232A0DB33DAC6BD451DE272FACBBD5217C2B7`.

Delivery rebuild SHA256: `4085B45F58CFDBCD87BCD7FD59461B67CAD123C3C5FD5A79BCB95C259E57F7C2`. After the recorded measurements, a two-line storage comment was corrected to describe block names and key headroom. Executable identity changed on relink; no executable source behavior changed. The delivery build reran all 40 tests, both nine-case linear oracles, and the native sequence-2 fixture query. The performance tables retain their original measured-binary identity rather than relabeling those samples.
