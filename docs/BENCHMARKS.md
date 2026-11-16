# Performance and correctness protocol

These are engineering targets, not measured achievements. Historical and current observations are in the linked result reports; the first real-disk run is [Phase A](PHASE_A_RESULTS.md).

## Comparable scope first

Correct results precede speed. Fewer indexed files, skipped results, different matching/order, or stale metadata do not count as performance wins. Compare Everything on the same device, volume, user visibility, corpus, freshness, and equivalent metadata/sort options. Record the tested version/channel/settings; do not combine stable and preview channels. For disk analysis, record WizTree's version, privilege, logical/allocated choice, and hard-link policy separately; see its [guide](https://diskanalyzer.com/guide).

## Initial targets

Reference: Windows 11 x64, 16 GiB RAM, NVMe, one million file/folder name entries. Initial modeling assumes 24 UTF-16 units per name and approximately 10% directories. Record actual distributions, hard links, and index options; different profiles are separate results.

| Metric | Initial target | Boundary |
| --- | --- | --- |
| Warm query, first 100 results | p95 <=10 ms | Core API, equivalent matching and ordering |
| Keystroke to visible results | p95 <=50 ms | Includes debounce, UI queue, rendering |
| Largest-file top 100 | p95 <=20 ms | Size index ready; equivalent ordering |
| Access prepared folder total | p95 <=5 ms | Data lookup, not entire map rendering |
| Open window with running core | p95 <=300 ms | Shortcut to first usable frame |
| Journal update lag | p95 <=500 ms | Finite mutation/close to searchable state; active writes separate |
| Idle CPU | <0.1% of one core on average | Ten minutes, quiet disk, all product processes |
| Total private commit | <=200 MiB | One million entries, UI/core/helper in normal use |
| Total resident memory | <=300 MiB | Disclose method; shared mappings counted once |
| Initial name indexing | <=15 s feasibility target | Defined NVMe fixture; size collection separate |

Candidate memory budget: core/index 140 MiB, visible UI 40 MiB, helper 20 MiB, including names, keys, and search helpers. Report bytes per entry and total use on larger fixtures. Meeting a design budget alone does not prove lower consumption than Everything.

## Separate measurements

Record first name-index readiness; complete size coverage and folder-total readiness; stopped-core load plus journal catch-up; running-core UI open; core query versus visible UI latency; first results versus exact count/final sort; normal memory versus mutation/compaction peaks; CPU time, transfers, persistent bytes, and update backlog.

Private commit, working set, and file-backed cache are different. A memory-mapped index is not free because its private counter is low. Preserve every relevant metadata field in layout comparisons.

Use deterministic synthetic corpora at 100k, one million, and ten million entries plus an authorized real disk. Record seed and fixture version. NVMe, SATA SSD, and HDD are separate profiles; unavailable hardware is untested.

Query groups should include exact, rare/common/no-match substring, one/two-character and empty queries, prefix, path/multi-term filters, extension/size/date intersections, largest/top-100, folder/tree/map navigation, Unicode case/combining characters/emoji, long paths, and concurrent mutations. Future query classes are not current CLI capabilities. Define expected IDs and tie-breaking for every implemented group.

## Repetition and reporting

- Use release builds; record toolchain, source identity, OS, CPU, RAM, disk, power/security settings.
- Run competitors sequentially, vary order, and verify equal index scope.
- Target at least 1000 measured warm samples per query group, with warmup separate. Preliminary 100-sample runs may report p95; fewer than 1000 must not claim p99.
- Target at least ten startup/index repetitions. Smaller development runs must state the actual count and give median/min/max without unsupported tail percentiles.
- Process restart does not establish cold storage. Label uncontrolled OS cache explicitly.
- Do not apply a security/Defender exception to only one competitor. Record throttling and power mode.
- Compare query engines through CLI/SDK when possible; UI end-to-end tests remain separate.
- Retain machine-readable results, commands, hashes, and coverage. Current runners are `benchmark-startup.ps1`, `benchmark-hot.ps1`, and CLI `bench`; they do not implement all planned profiles.

## Correctness acceptance matrix

Cover create/grow/shrink/delete, writes to open files, file/folder rename/move, parent ordering, hard-link creation/removal, sparse/compressed/resident/alternate-stream data, junction/symlink cycles, permission changes, cloud placeholders without content hydration, concurrent enumeration, journal gaps/recreation, sleep/resume, mount/device changes, interruption around checkpoints, reused file references, and corrupt/truncated snapshots.

Use independent Win32 enumeration and metadata probes on controlled fixtures; a competitor is not the sole oracle. An active volume is not an atomic snapshot. Check correctness at a quiet fixture or defined mutation barrier.

Publish speed results only after applicable correctness cases pass. Separate name enumeration from size completion. Retain slower groups in the report. Revisit layout/strategy if the intended advantage is absent. Do not claim product-wide low resource use from a core-only benchmark before measuring UI/helper/compaction costs.
