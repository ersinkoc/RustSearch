# Snapshot/cache loading — 16 November 2026

These are historical development measurements, not current performance promises. Earlier 0.2/0.3/0.4 labels identify unpublished experiment binaries; the project version is 0.0.1. The current memory comparison is in [MEMORY_RESULTS.md](MEMORY_RESULTS.md). Raw local evidence under `.temp_files` is excluded from the public repository.

Environment: Windows 11 Pro build 26300, AMD Ryzen 9 9950X3D, Samsung SSD 990 EVO Plus 4 TB NVMe on D: NTFS, approximately 126 GiB visible RAM, Rust 1.93/MSVC release. OS cache was not flushed; these are not controlled cold-SSD results.

## Change

Decode snapshot records, UTF-16 names, cache groups, and u32 tables in reusable 256 KiB blocks. Compute payload CRC while decoding rather than making a second complete read and seek. Check source ID order, flags, and arena ranges during decoding. Keep full cache semantic validation, CRC, length, and EOF checks before exposing results. No mmap, full encoded-payload staging, dependency addition, or wire-format change was introduced.

## Frozen corpus without deltas

12,285,803 identical records; source 908,979,979 bytes and existing cache 173,439,480 bytes. Five new processes per variant, alternating order, affinity mask 1. Same no-match query and limit 0. Median (min–max); component medians need not sum to total median.

| Metric | 0.3.0 | 0.4.0 |
| --- | --- | --- |
| Snapshot load | 1.664 s (1.618–1.709) | 0.323 s (0.311–0.342) |
| Cache read, validation, preparation | 1.267 s (1.193–1.435) | 1.185 s (1.164–1.274) |
| End-to-end CLI | 3.073 s (2.957–3.176) | 1.598 s (1.576–1.699) |
| Process CPU time | 3.000 s | 1.531 s |
| Private commit | 1,086,386,176 byte | 1,086,345,216 byte |
| Process peak private commit | 1,086,386,176 byte | 1,086,345,216 byte |
| Working set | 1,089,314,816 byte | 1,089,245,184 byte |
| Process read transfer | 2,164,838,798 byte | 1,082,419,459 byte |

Total startup fell 48.0%, source load 80.6%, preparation 6.4%, and CPU 49.0%. Private commit differed by only 40 KiB; no meaningful RAM reduction was measured. Process read transfer halved, but this counter does not measure physical-device traffic. Source/cache hashes remained unchanged; every measured command hit cache with no build/write. The old 3.051 s historical sample was not substituted for the fresh baseline rerun.

## Same base plus two committed deltas

Separate retained native corpus: 12,274,534 current records, sequence 2, 22 live overlay records, 7 shadowed base rows. Same five-process protocol. Both binaries read identical base/delta/cache bytes; hashes, sequence, and counts stayed fixed.

| Metric | 0.3.0 | 0.4.0 |
| --- | --- | --- |
| Source and delta load | 1.618 s (1.595–1.642) | 0.274 s (0.269–0.328) |
| Cache preparation and delta binding | 1.116 s (1.099–1.277) | 1.059 s (0.985–1.118) |
| End-to-end CLI | 2.873 s (2.807–3.035) | 1.426 s (1.342–1.544) |
| Private commit | 1,097,949,184 byte | 1,097,908,224 byte |
| Process peak private commit | 1,097,973,760 byte | 1,097,969,664 byte |
| Process CPU time | 2.734 s | 1.359 s |
| Process read transfer | 2,161,665,296 byte | 1,080,836,060 byte |

Delta-inclusive startup fell 50.4%. This table is not a speed comparison against the different no-delta corpus.

## Correctness

A fresh active D: acceptance run started with 12,277,260 records. First query and both refresh generations hit cache at sequences 0/1/2. Rename/move/resize/create/delete, Unicode, and zero-size results matched independent enumeration. Base/cache hashes were unchanged; delta log was 7131 bytes and pending refresh was zero.

33 tests, format, Clippy, release/example builds, and nine query groups on each frozen corpus against the linear oracle passed. New coverage exercised record/UTF-16/cache block boundaries, full 64-bit references, signed USNs, unpaired UTF-16, valid-CRC malformed fields, short reads, trailing data, and truncation. Fixture checks do not independently prove every volume record. Hosted CI, physical power loss, long-running service, other hardware, and GUI were not tested.

Local evidence: [startup](../.temp_files/load-20261116/startup-summary.json), [delta startup](../.temp_files/load-20261116/delta/startup-summary.json), [warm oracle](../.temp_files/load-20261116/hot.json), [delta oracle](../.temp_files/load-20261116/delta/hot.json), [native acceptance](../.temp_files/ntfs-load-1794864510790/acceptance.json), [machine identity](../.temp_files/ntfs-load-1794864510790/machine.json).

Retained binary SHA256 identities, in historical report order:

- `D31F61409CE23E13A10CBA973D0BFE2F518A89D9A8076E9DEB8FE954B05D4841`
- `E47DF130DCFF8B583FB9D02489E73F363475FCAC3781025DE9F8FFB23C3986F7`
