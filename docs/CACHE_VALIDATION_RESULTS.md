# Cache layout and validation — 17 November 2026

These are historical development measurements, not current performance promises. Earlier 0.2/0.3/0.4 labels identify unpublished experiment binaries; the project version is 0.0.1. The current memory comparison is in [MEMORY_RESULTS.md](MEMORY_RESULTS.md). Raw local evidence under `.temp_files` is excluded from the public repository.

Environment: Windows 11 Pro build 26300, AMD Ryzen 9 9950X3D, Samsung SSD 990 EVO Plus 4 TB NVMe on D: NTFS, approximately 126 GiB visible RAM, Rust 1.93/MSVC release. OS cache was not flushed; these are not controlled cold-SSD results.

## Change

Cache v2 stores base-row-to-group associations instead of group-ordered posting rows. Loading traverses source records in ID order, validates memberships, and reconstructs the same sorted posting lists. Group fields temporarily serve as cursors, avoiding a separate per-row/per-group cursor allocation on normal cache hits. Size of the persistent relation table is unchanged.

Group key/type order, ranges, representative minimum ID, character/trigram masks, all source memberships, and complete group coverage are checked. V2 group ranges must be canonical prefix sums, and final cursor positions must match every declared count. Extension uniqueness/coverage, size coverage/order, CRC, length, base identity, and EOF checks remain. Character lookup uses a fixed 256-entry table; group character/trigram summaries are computed together. Extension lookup uses the existing `memchr` dependency. Snapshot/delta formats and overlay binding remain unchanged.

## One-time conversion

A v1 cache passes all old validations before atomic conversion under the builder lock: state `upgraded`, build time zero, conversion included in write time. Lock/publication failure retains valid legacy search. Older experiment binaries cannot read v2.

Conversion/publication temporarily allocates one u32 owner per row: 49,143,212 bytes (46.9 MiB) for the main corpus. This is absent from normal v2 hits. On the delta corpus, one conversion took 1.164 s old-cache read/validation and 232.6 ms conversion/publication. Those one-time costs were excluded from hit comparisons.

## Same-disk startup

12,285,803 frozen records; source 908,979,979 bytes; v1/v2 caches both 173,439,480 bytes. Five new processes per variant, alternating order, affinity mask 1, no-match query, limit 0. Separate cache paths used byte-identical source hard links. Source/delta hashes matched across variants and each cache hash stayed unchanged. All samples hit with no build/write.

| Metric | Previous block loader (experiment 0.4) | Cache-layout 0.0.1 |
| --- | --- | --- |
| Snapshot and optional delta load | 0.314 s (0.294–0.351) | 0.308 s (0.278–0.394) |
| Cache preparation | 1.200 s (1.181–1.304) | 0.776 s (0.691–0.927) |
| End-to-end CLI | 1.708 s (1.629–1.905) | 1.187 s (1.092–1.478) |
| Process CPU time | 1.500 s (1.453–1.609) | 1.094 s (0.984–1.312) |
| Private commit | 1,086,324,736 byte | 1,086,308,352 byte |
| Process peak private commit | 1,086,324,736 byte | 1,086,308,352 byte |
| Working set | 1,089,224,704 byte | 1,089,220,608 byte |

Preparation fell 35.4% and total startup 30.5%. Private commit differed by only 16 KiB. Both variants read 1,082,419,459 process-transfer bytes: this change improved validation access/CPU cost, not transfer volume. Median (min–max), uncontrolled OS cache, no startup p95/p99. Prior 1.185 s preparation / 1.598 s total were historical observations; the rerun above is the control.

## Two-delta corpus

Separate retained corpus: 12,274,534 records, sequence 2, 22 live overlay records, 7 shadowed base rows, cache 173,082,616 bytes. Five processes each; identical source and delta bytes and stable hashes.

| Metric | Previous block loader (experiment 0.4) | Cache-layout 0.0.1 |
| --- | --- | --- |
| Snapshot and optional delta load | 0.352 s (0.306–0.364) | 0.353 s (0.294–0.363) |
| Cache preparation | 1.155 s (1.132–1.277) | 0.872 s (0.783–0.900) |
| End-to-end CLI | 1.683 s (1.544–1.742) | 1.375 s (1.186–1.434) |
| Process CPU time | 1.469 s (1.422–1.641) | 1.203 s (1.078–1.234) |
| Private commit | 1,097,887,744 byte | 1,097,875,456 byte |
| Process peak private commit | 1,097,924,608 byte | 1,097,904,128 byte |
| Working set | 1,099,288,576 byte | 1,099,284,480 byte |

Preparation fell 24.5% and total startup 18.3%; steady memory stayed approximately unchanged. Different corpora are not directly compared.

## Verification

Format, Clippy, 36 tests, release/example builds, and linear-oracle checks on nine groups for each corpus passed. New tests covered one-time v1 conversion, builder-lock fallback, committed overlay binding, empty source, and valid-CRC forgeries of groups/IDs/keys/ranges/counts. Existing corruption and two-generation delta cases remained enabled.

Fresh D: native acceptance began with 12,280,655 records. Rename/move/resize/create/delete, Unicode, and zero size matched independent enumeration. Cache hits bound sequences 0/1/2; base/cache hashes were stable, delta size 7593 bytes, pending refresh zero. Hosted CI, power loss, long-running service, other hardware, and GUI were not tested.

Local evidence: [startup](../.temp_files/validation-20261117/startup/startup-summary.json), [delta startup](../.temp_files/validation-20261117/delta-startup/startup-summary.json), [conversion](../.temp_files/validation-20261117/delta-startup/cache-warmup.json), [warm oracle](../.temp_files/validation-20261117/hot.json), [delta oracle](../.temp_files/validation-20261117/delta-hot.json), [native acceptance](../.temp_files/ntfs-validation-20261117/acceptance.json).

Retained binary SHA256 identities, in historical report order:

- `E47DF130DCFF8B583FB9D02489E73F363475FCAC3781025DE9F8FFB23C3986F7`
- `C44E3B67D96299B47D9396997D425C02274319D031C504B8F09A02DE492ED344`
