use super::*;
use rustsearch_core::{Delta, Record};
fn fixture() -> Index {
    let mut index = Index::new(Metadata {
        source: "walk".into(),
        root: "D:\\test".into(),
        volume_id: "test".into(),
        filesystem: "NTFS".into(),
        root_id: 1,
        created_unix_ms: 0,
        checkpoint: None,
        pending_refresh: vec![],
        skipped_entries: 0,
        reparse_points_not_followed: 0,
    });
    index
        .push(Record {
            id: 2,
            parent: 1,
            usn: 0,
            size: Some(0),
            flags: 0,
            name: "original.txt".encode_utf16().collect(),
        })
        .unwrap();
    index.finish().unwrap();
    index
}
fn transaction(index: &Index) -> Delta {
    let mut records = std::collections::BTreeMap::new();
    records.insert(2, None);
    records.insert(
        3,
        Some(Record {
            id: 3,
            parent: 1,
            usn: 99,
            size: Some(42),
            flags: 0,
            name: "updated.txt".encode_utf16().collect(),
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
fn delta_append_preserves_base_and_replays_committed_transactions_only() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("base.rsidx");
    save(&path, &fixture()).unwrap();
    let base = std::fs::read(&path).unwrap();
    let guard = WriteGuard::acquire(&path).unwrap();
    let index = load(&path).unwrap();
    let delta = transaction(&index);
    let stats = append_delta(&guard, &index, &delta).unwrap();
    assert!(stats.bytes_appended < 4096);
    assert_eq!(std::fs::read(&path).unwrap(), base);
    let updated = load(&path).unwrap();
    assert!(updated.get(2).is_none());
    assert_eq!(updated.size(updated.get(3).unwrap()), 42);
    assert_eq!(updated.log_sequence, 1);
    assert!(
        append_delta(&guard, &index, &delta).is_err(),
        "stale in-memory state must not append twice"
    );
    let log = std::fs::read(delta_path(&path)).unwrap();
    for cut in [0, 1, 31, 32, 40, log.len() - 1] {
        std::fs::write(delta_path(&path), &log[..cut]).unwrap();
        let recovered = load(&path).unwrap();
        assert_eq!(recovered.log_sequence, 0);
        assert!(recovered.get(2).is_some());
        append_delta(&guard, &recovered, &transaction(&recovered)).unwrap();
        let loaded = load(&path).unwrap();
        assert_eq!(loaded.size(loaded.get(3).unwrap()), 42);
    }
    let mut damaged = log;
    damaged[50] ^= 1;
    std::fs::write(delta_path(&path), damaged).unwrap();
    assert!(load(&path).unwrap_err().to_string().contains("checksum"));
}
#[test]
fn writer_lock_generation_and_new_path_compaction_are_enforced() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("base.rsidx");
    save(&path, &fixture()).unwrap();
    let guard = WriteGuard::acquire(&path).unwrap();
    assert!(WriteGuard::acquire(&path).is_err());
    let index = load(&path).unwrap();
    append_delta(&guard, &index, &transaction(&index)).unwrap();
    drop(guard);
    assert!(save(&path, &fixture()).is_err());
    let mut updated = load(&path).unwrap();
    updated.consolidate().unwrap();
    let new_path = dir.path().join("compacted.rsidx");
    save(&new_path, &updated).unwrap();
    let compacted = load(&new_path).unwrap();
    assert_eq!(compacted.log_sequence, 0);
    assert!(compacted.updates.is_empty());
    assert!(compacted.get(2).is_none());
    assert_eq!(compacted.size(compacted.get(3).unwrap()), 42);
    let mut other = fixture();
    other.set_size(0, 123);
    let another = dir.path().join("another.rsidx");
    save(&another, &other).unwrap();
    std::fs::copy(delta_path(&path), delta_path(&another)).unwrap();
    assert!(load(&another).is_err());
}

#[test]
fn interrupted_second_frame_preserves_first_commit_and_can_be_retried() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("base.rsidx");
    save(&path, &fixture()).unwrap();
    let guard = WriteGuard::acquire(&path).unwrap();
    let first = load(&path).unwrap();
    append_delta(&guard, &first, &transaction(&first)).unwrap();
    let second = load(&path).unwrap();
    let mut delta = transaction(&second);
    delta.records.get_mut(&3).unwrap().as_mut().unwrap().size = Some(99);
    append_delta(&guard, &second, &delta).unwrap();
    let log = std::fs::read(delta_path(&path)).unwrap();
    std::fs::write(delta_path(&path), &log[..log.len() - 2]).unwrap();
    let recovered = load(&path).unwrap();
    assert_eq!(recovered.log_sequence, 1);
    assert_eq!(recovered.size(recovered.get(3).unwrap()), 42);
    append_delta(&guard, &recovered, &delta).unwrap();
    let complete = load(&path).unwrap();
    assert_eq!(complete.log_sequence, 2);
    assert_eq!(complete.size(complete.get(3).unwrap()), 99);
}

#[test]
fn failed_derived_write_preserves_published_artifact() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("derived.search");
    write_derived_atomic(&path, |file| {
        file.write_all(b"complete")?;
        Ok(())
    })
    .unwrap();
    let error = write_derived_atomic(&path, |file| {
        file.write_all(b"partial")?;
        anyhow::bail!("simulated writer I/O failure")
    });
    assert!(error.is_err());
    assert_eq!(std::fs::read(&path).unwrap(), b"complete");
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
}
