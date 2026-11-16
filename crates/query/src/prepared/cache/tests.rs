use super::*;
use crate::{Kind, Mode, Query};
use rustsearch_core::{Delta, Metadata, Record};
use std::collections::BTreeMap;

fn fixture() -> Index {
    let mut index = Index::new(Metadata {
        source: "walk".into(),
        root: "D:\\fixture".into(),
        volume_id: "test".into(),
        filesystem: "NTFS".into(),
        root_id: 1,
        created_unix_ms: 0,
        checkpoint: None,
        pending_refresh: vec![],
        skipped_entries: 0,
        reparse_points_not_followed: 0,
    });
    for id in 2..82 {
        let name = ["old.pdf", "A.PDF", "İstanbul.tar.gz", "🙂.txt", "nodot"][(id as usize) % 5];
        index
            .push(Record {
                id,
                parent: 1,
                usn: 0,
                size: (id % 7 != 0).then_some(id % 13),
                flags: if id % 11 == 0 { DIRECTORY } else { 0 },
                name: name.encode_utf16().collect(),
            })
            .unwrap();
    }
    index.finish().unwrap();
    index
}
fn check(index: &Index, cached: &SearchIndex<'_>) {
    for text in ["", "a", "old", "new", "İ", "🙂", "missing"] {
        for mode in [Mode::Contains, Mode::Prefix, Mode::Exact] {
            for kind in [Kind::Any, Kind::File, Kind::Folder] {
                for ext in [None, Some("pdf"), Some("tar.gz")] {
                    for min_size in [None, Some(0), Some(9)] {
                        for largest in [false, true] {
                            for limit in [0, 1, 5] {
                                let query = Query {
                                    text: text.into(),
                                    mode,
                                    kind,
                                    extension: ext.map(String::from),
                                    min_size,
                                    largest,
                                    limit,
                                };
                                assert_eq!(
                                    cached.execute(&query),
                                    query.execute(index),
                                    "{query:?}"
                                );
                            }
                        }
                    }
                }
            }
        }
    }
}
fn delta(index: &Index) -> Delta {
    let mut records = BTreeMap::new();
    records.insert(2, None);
    records.insert(
        3,
        Some(Record {
            id: 3,
            parent: 1,
            usn: 10,
            size: Some(99),
            flags: 0,
            name: "renamed-new.bin".encode_utf16().collect(),
        }),
    );
    records.insert(
        100,
        Some(Record {
            id: 100,
            parent: 1,
            usn: 10,
            size: Some(0),
            flags: 0,
            name: "created-new.pdf".encode_utf16().collect(),
        }),
    );
    Delta {
        previous_sequence: index.log_sequence,
        previous_checkpoint: index.metadata.checkpoint.clone(),
        metadata: index.metadata.clone(),
        records,
    }
}

#[test]
fn persistent_cache_roundtrip_and_two_delta_generations_keep_base_cache_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("base.rsidx");
    rustsearch_storage::save(&path, &fixture()).unwrap();
    let first = rustsearch_storage::load(&path).unwrap();
    let (built, state) = SearchIndex::prepare(&first, &path, true);
    assert_eq!(state.state, "built");
    check(&first, &built);
    drop(built);
    let base_cache = std::fs::read(cache_path(&path)).unwrap();
    let (hit, state) = SearchIndex::prepare(&first, &path, true);
    assert_eq!(state.state, "hit");
    check(&first, &hit);
    drop(hit);
    let guard = WriteGuard::acquire(&path).unwrap();
    rustsearch_storage::append_delta(&guard, &first, &delta(&first)).unwrap();
    drop(guard);
    let second = rustsearch_storage::load(&path).unwrap();
    let (hit, state) = SearchIndex::prepare(&second, &path, true);
    assert_eq!(state.state, "hit");
    assert_eq!(state.bound_sequence, 1);
    assert_eq!(state.shadowed_base_rows, 2);
    check(&second, &hit);
    drop(hit);
    assert_eq!(std::fs::read(cache_path(&path)).unwrap(), base_cache);
    let mut next = delta(&second);
    next.records.clear();
    next.records.insert(3, None);
    next.records.insert(
        100,
        Some(Record {
            id: 100,
            parent: 1,
            usn: 20,
            size: None,
            flags: DIRECTORY,
            name: "new-folder".encode_utf16().collect(),
        }),
    );
    next.records.insert(
        4,
        Some(Record {
            id: 4,
            parent: 100,
            usn: 20,
            size: Some(1),
            flags: 0,
            name: "moved-new.txt".encode_utf16().collect(),
        }),
    );
    let guard = WriteGuard::acquire(&path).unwrap();
    rustsearch_storage::append_delta(&guard, &second, &next).unwrap();
    drop(guard);
    let third = rustsearch_storage::load(&path).unwrap();
    let (hit, state) = SearchIndex::prepare(&third, &path, true);
    assert_eq!(state.state, "hit");
    assert_eq!(state.bound_sequence, 2);
    check(&third, &hit);
    assert_eq!(
        String::from_utf16(&third.path_utf16(third.get(4).unwrap()).unwrap()).unwrap(),
        "D:\\fixture\\new-folder\\moved-new.txt"
    );
    assert_eq!(std::fs::read(cache_path(&path)).unwrap(), base_cache);
    // An interrupted next append cannot introduce uncommitted cache-visible records.
    use std::io::Write;
    let mut log = std::fs::OpenOptions::new()
        .append(true)
        .open(rustsearch_storage::delta_path(&path))
        .unwrap();
    log.write_all(&[1, 2, 3]).unwrap();
    drop(log);
    let recovered = rustsearch_storage::load(&path).unwrap();
    let (hit, state) = SearchIndex::prepare(&recovered, &path, true);
    assert_eq!(state.state, "hit");
    assert_eq!(state.bound_sequence, 2);
    check(&recovered, &hit);
}

fn repair_crc(bytes: &mut [u8]) {
    let mut hash = crc32fast::Hasher::new();
    hash.update(&bytes[..92]);
    hash.update(&bytes[96..]);
    bytes[92..96].copy_from_slice(&hash.finalize().to_le_bytes());
}

fn legacy_bytes(path: &Path, index: &Index) -> Vec<u8> {
    let (cache, _) = load(&cache_path(path), index).unwrap();
    let mut bytes = std::fs::read(cache_path(path)).unwrap();
    bytes[8..12].copy_from_slice(&1u32.to_le_bytes());
    let offset = HEADER + cache.groups.len() * 40;
    for (at, row) in cache.postings.iter().enumerate() {
        bytes[offset + at * 4..offset + at * 4 + 4].copy_from_slice(&row.to_le_bytes());
    }
    repair_crc(&mut bytes);
    bytes
}

#[test]
fn legacy_cache_upgrades_once_and_rebinds_committed_deltas_without_rebuilding() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("legacy.rsidx");
    rustsearch_storage::save(&path, &fixture()).unwrap();
    let base = rustsearch_storage::load(&path).unwrap();
    SearchIndex::prepare(&base, &path, true);
    let original = legacy_bytes(&path, &base);
    std::fs::write(cache_path(&path), &original).unwrap();
    let guard = WriteGuard::acquire(&path).unwrap();
    rustsearch_storage::append_delta(&guard, &base, &delta(&base)).unwrap();
    drop(guard);
    let index = rustsearch_storage::load(&path).unwrap();
    let guard = WriteGuard::acquire(&cache_path(&path)).unwrap();
    let (cached, stats) = SearchIndex::prepare(&index, &path, true);
    assert_eq!(stats.state, "legacy_hit");
    assert_eq!(stats.build_ms, 0.);
    check(&index, &cached);
    assert_eq!(std::fs::read(cache_path(&path)).unwrap(), original);
    drop(guard);
    let (cached, stats) = SearchIndex::prepare(&index, &path, true);
    assert_eq!(stats.state, "upgraded");
    assert_eq!(stats.build_ms, 0.);
    assert_eq!(stats.bound_sequence, 1);
    check(&index, &cached);
    let upgraded = std::fs::read(cache_path(&path)).unwrap();
    assert_eq!(u32::from_le_bytes(upgraded[8..12].try_into().unwrap()), 2);
    assert_eq!(upgraded.len(), original.len());
    let (cached, stats) = SearchIndex::prepare(&index, &path, true);
    assert_eq!(stats.state, "hit");
    check(&index, &cached);
    assert_eq!(std::fs::read(cache_path(&path)).unwrap(), upgraded);
}

#[test]
fn valid_crc_cannot_hide_wrong_groups_representatives_ranges_or_coverage() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("corrupt.rsidx");
    rustsearch_storage::save(&path, &fixture()).unwrap();
    let index = rustsearch_storage::load(&path).unwrap();
    SearchIndex::prepare(&index, &path, true);
    let original = std::fs::read(cache_path(&path)).unwrap();
    let groups = header_u64(&original, 44) as usize;
    let mut cases = Vec::new();
    for (offset, bytes) in [
        (HEADER + 8, 0u64.to_le_bytes().to_vec()),
        (HEADER + 16, u64::MAX.to_le_bytes().to_vec()),
        (HEADER + 24, u32::MAX.to_le_bytes().to_vec()),
        (HEADER + 32, 1u32.to_le_bytes().to_vec()),
        (HEADER + 36, DIR_BIT.to_le_bytes().to_vec()),
        (HEADER + groups * 40, u32::MAX.to_le_bytes().to_vec()),
    ] {
        let mut case = original.clone();
        case[offset..offset + bytes.len()].copy_from_slice(&bytes);
        cases.push(case);
    }
    // Keep ranges canonical and total coverage unchanged, but forge two group counts.
    let mut coverage = original.clone();
    let mut start = 0u32;
    for id in 0..groups {
        let at = HEADER + id * 40;
        let mut len = u32::from_le_bytes(coverage[at + 36..at + 40].try_into().unwrap());
        if id == 0 {
            len += 1;
        }
        if id == 1 {
            len -= 1;
        }
        coverage[at + 32..at + 36].copy_from_slice(&start.to_le_bytes());
        coverage[at + 36..at + 40].copy_from_slice(&len.to_le_bytes());
        start += len;
    }
    cases.push(coverage);
    // Legacy membership still has to be validated before conversion.
    let mut legacy = legacy_bytes(&path, &index);
    legacy[HEADER + groups * 40..HEADER + groups * 40 + 4].copy_from_slice(&u32::MAX.to_le_bytes());
    cases.push(legacy);
    for mut bytes in cases {
        repair_crc(&mut bytes);
        std::fs::write(cache_path(&path), bytes).unwrap();
        let (cached, stats) = SearchIndex::prepare(&index, &path, true);
        assert_eq!(stats.state, "built");
        assert!(stats.reason.is_some());
        check(&index, &cached);
    }
}

#[test]
fn empty_snapshot_has_a_reusable_cache() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("empty.rsidx");
    let index = Index::new(fixture().metadata);
    rustsearch_storage::save(&path, &index).unwrap();
    let index = rustsearch_storage::load(&path).unwrap();
    SearchIndex::prepare(&index, &path, true);
    let (cached, stats) = SearchIndex::prepare(&index, &path, true);
    assert_eq!(stats.state, "hit");
    assert_eq!(
        cached.execute(&Query::default()),
        Query::default().execute(&index)
    );
}
#[test]
fn damaged_truncated_versioned_and_forged_caches_rebuild_instead_of_returning_bad_results() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("base.rsidx");
    rustsearch_storage::save(&path, &fixture()).unwrap();
    let index = rustsearch_storage::load(&path).unwrap();
    SearchIndex::prepare(&index, &path, true);
    let original = std::fs::read(cache_path(&path)).unwrap();
    let mut cases = vec![
        vec![],
        original[..95].to_vec(),
        original[..original.len() - 1].to_vec(),
    ];
    let mut version = original.clone();
    version[8..12].copy_from_slice(&99u32.to_le_bytes());
    cases.push(version);
    let mut shape = original.clone();
    shape[44..52].copy_from_slice(&u64::MAX.to_le_bytes());
    repair_crc(&mut shape);
    cases.push(shape);
    let mut checksum = original.clone();
    *checksum.last_mut().unwrap() ^= 1;
    cases.push(checksum);
    let mut posting = original.clone();
    let groups = header_u64(&posting, 44) as usize;
    posting[96 + groups * 40..100 + groups * 40].copy_from_slice(&u32::MAX.to_le_bytes());
    repair_crc(&mut posting);
    cases.push(posting);
    let mut mask = original.clone();
    mask[96] ^= 1;
    repair_crc(&mut mask);
    cases.push(mask);
    for bytes in cases {
        std::fs::write(cache_path(&path), bytes).unwrap();
        let (rebuilt, state) = SearchIndex::prepare(&index, &path, true);
        assert_eq!(state.state, "built");
        assert!(state.reason.is_some());
        check(&index, &rebuilt);
    }
}

#[test]
fn new_base_identity_and_compaction_cannot_reuse_an_old_cache() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("base.rsidx");
    rustsearch_storage::save(&path, &fixture()).unwrap();
    let first = rustsearch_storage::load(&path).unwrap();
    SearchIndex::prepare(&first, &path, true);
    let old_cache = std::fs::read(cache_path(&path)).unwrap();
    let mut replacement = fixture();
    replacement.entries.pop();
    rustsearch_storage::save(&path, &replacement).unwrap();
    let second = rustsearch_storage::load(&path).unwrap();
    let (built, state) = SearchIndex::prepare(&second, &path, true);
    assert_eq!(state.state, "built");
    assert!(state.reason.unwrap().contains("generation"));
    check(&second, &built);
    drop(built);
    let mut compacted = second;
    compacted.consolidate().unwrap();
    let new_path = dir.path().join("new.rsidx");
    rustsearch_storage::save(&new_path, &compacted).unwrap();
    std::fs::write(cache_path(&new_path), old_cache).unwrap();
    let final_index = rustsearch_storage::load(&new_path).unwrap();
    let (built, state) = SearchIndex::prepare(&final_index, &new_path, true);
    assert_eq!(state.state, "built");
    check(&final_index, &built);
}

#[test]
fn unavailable_cache_and_contended_builder_fall_back_without_breaking_search() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("base.rsidx");
    rustsearch_storage::save(&path, &fixture()).unwrap();
    let index = rustsearch_storage::load(&path).unwrap();
    let guard = WriteGuard::acquire(&cache_path(&path)).unwrap();
    let (built, state) = SearchIndex::prepare(&index, &path, true);
    assert_eq!(state.state, "built_unsaved");
    check(&index, &built);
    drop(guard);
    std::fs::create_dir(cache_path(&path)).unwrap();
    let (built, state) = SearchIndex::prepare(&index, &path, true);
    assert_eq!(state.state, "built_unsaved");
    check(&index, &built);
    let (built, state) = SearchIndex::prepare(&index, &path, false);
    assert_eq!(state.state, "disabled");
    check(&index, &built);
}

#[test]
fn duplicate_extension_postings_with_valid_checksum_are_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("base.rsidx");
    rustsearch_storage::save(&path, &fixture()).unwrap();
    let index = rustsearch_storage::load(&path).unwrap();
    SearchIndex::prepare(&index, &path, true);
    let (decoded, _) = load(&cache_path(&path), &index).unwrap();
    let pair = decoded
        .by_extension
        .windows(2)
        .position(|ids| {
            extension(decoded.groups[ids[0] as usize].key(&index))
                == extension(decoded.groups[ids[1] as usize].key(&index))
        })
        .unwrap();
    let mut bytes = std::fs::read(cache_path(&path)).unwrap();
    let start = 96 + header_u64(&bytes, 44) as usize * 40 + header_u64(&bytes, 52) as usize * 4;
    let duplicate = bytes[start + pair * 4..start + pair * 4 + 4].to_vec();
    bytes[start + (pair + 1) * 4..start + (pair + 2) * 4].copy_from_slice(&duplicate);
    repair_crc(&mut bytes);
    std::fs::write(cache_path(&path), bytes).unwrap();
    let (prepared, state) = SearchIndex::prepare(&index, &path, true);
    assert_eq!(
        state.state, "built",
        "a duplicate group would count one extension twice and omit another"
    );
    check(&index, &prepared);
}

#[test]
fn bulk_cache_decodes_multi_block_groups_postings_and_size_rows() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("large.rsidx");
    let mut source = fixture();
    for id in 100..70100u64 {
        source
            .push(Record {
                id,
                parent: 1,
                usn: id as i64,
                size: Some(id % 109),
                flags: 0,
                name: format!("boundary-{:05}.txt", id % 7000)
                    .encode_utf16()
                    .collect(),
            })
            .unwrap();
    }
    source.finish().unwrap();
    rustsearch_storage::save(&path, &source).unwrap();
    let source = rustsearch_storage::load(&path).unwrap();
    let (built, state) = SearchIndex::prepare(&source, &path, true);
    assert_eq!(state.state, "built");
    drop(built);
    let (loaded, state) = SearchIndex::prepare(&source, &path, true);
    assert_eq!(state.state, "hit");
    for query in [
        Query {
            text: "boundary-000".into(),
            mode: Mode::Prefix,
            limit: 100,
            ..Default::default()
        },
        Query {
            extension: Some("txt".into()),
            min_size: Some(100),
            limit: 100,
            ..Default::default()
        },
        Query {
            largest: true,
            limit: 100,
            ..Default::default()
        },
    ] {
        assert_eq!(loaded.execute(&query), query.execute(&source));
    }
}
