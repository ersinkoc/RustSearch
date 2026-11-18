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

#[test]
fn retained_guard_validates_once_and_rejects_changed_or_stale_log_state() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("base.rsidx");
    save(&path, &fixture()).unwrap();
    let mut index = load(&path).unwrap();
    let guard = WriteGuard::acquire(&path).unwrap();
    let delta = transaction(&index);
    append_delta(&guard, &index, &delta).unwrap();
    index.apply_delta(delta).unwrap();
    let delta = transaction(&index);
    let stats = append_delta(&guard, &index, &delta).unwrap();
    assert_eq!(stats.validated_log_bytes, 0);
    index.apply_delta(delta).unwrap();
    let stale = load(&path).unwrap();
    let delta = transaction(&index);
    append_delta(&guard, &index, &delta).unwrap();
    index.apply_delta(delta).unwrap();
    assert!(append_delta(&guard, &stale, &transaction(&stale)).is_err());
    drop(guard);
    let guard = WriteGuard::acquire(&path).unwrap();
    let delta = transaction(&index);
    let old_length = std::fs::metadata(delta_path(&path)).unwrap().len();
    assert_eq!(
        append_delta(&guard, &index, &delta)
            .unwrap()
            .validated_log_bytes,
        old_length
    );
    index.apply_delta(delta).unwrap();
    let mut bytes = std::fs::read(delta_path(&path)).unwrap();
    bytes[50] ^= 1;
    std::thread::sleep(std::time::Duration::from_millis(10));
    std::fs::write(delta_path(&path), bytes).unwrap();
    assert!(append_delta(&guard, &index, &transaction(&index)).is_err());
}

#[test]
fn checkpoint_preserves_sequence_tombstones_metadata_and_unknown_sizes() {
    for native in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("base.rsidx");
        let mut source = fixture();
        if native {
            source.metadata.checkpoint = Some(rustsearch_core::Checkpoint {
                journal_id: 7,
                next_usn: 10,
            });
        }
        save(&path, &source).unwrap();
        let base = std::fs::read(&path).unwrap();
        let guard = WriteGuard::acquire(&path).unwrap();
        let mut index = load(&path).unwrap();
        for n in 0..5 {
            let mut delta = transaction(&index);
            delta.metadata.pending_refresh = vec![3];
            delta.metadata.skipped_entries = 9;
            delta.metadata.reparse_points_not_followed = 2;
            if native {
                delta.metadata.checkpoint.as_mut().unwrap().next_usn += 1;
            }
            delta.records.get_mut(&3).unwrap().as_mut().unwrap().size = (n != 4).then_some(n);
            append_delta(&guard, &index, &delta).unwrap();
            index.apply_delta(delta).unwrap();
        }
        let old = std::fs::read(delta_path(&path)).unwrap();
        let sequence = index.log_sequence;
        let stats = checkpoint_delta(&guard, &index).unwrap();
        assert!(stats.checkpointed);
        let log = std::fs::read(delta_path(&path)).unwrap();
        assert_eq!(&log[..8], b"RSDEL002");
        assert!(log.len() < old.len());
        let mut loaded = load(&path).unwrap();
        assert_eq!(loaded.log_sequence, sequence);
        assert_eq!(loaded.metadata.checkpoint, index.metadata.checkpoint);
        assert_eq!(loaded.metadata.pending_refresh, vec![3]);
        assert_eq!(loaded.metadata.skipped_entries, 9);
        assert_eq!(loaded.metadata.reparse_points_not_followed, 2);
        assert!(loaded.get(2).is_none());
        assert_eq!(
            loaded.name(loaded.get(3).unwrap()).as_ref(),
            "updated.txt".encode_utf16().collect::<Vec<_>>()
        );
        assert_eq!(loaded.flags(loaded.get(3).unwrap()) & SIZE_KNOWN, 0);
        let delta = transaction(&loaded);
        assert_eq!(
            append_delta(&guard, &loaded, &delta)
                .unwrap()
                .validated_log_bytes,
            0
        );
        loaded.apply_delta(delta).unwrap();
        assert_eq!(load(&path).unwrap().log_sequence, sequence + 1);
        assert_eq!(std::fs::read(&path).unwrap(), base);
        let before_failed_checkpoint = std::fs::read(delta_path(&path)).unwrap();
        assert!(checkpoint_delta(&guard, &index).is_err());
        assert_eq!(
            std::fs::read(delta_path(&path)).unwrap(),
            before_failed_checkpoint
        );
        assert!(append_delta(&guard, &index, &transaction(&index)).is_err());
    }
}

#[test]
fn checkpoint_requires_committed_first_frame_and_recovers_interrupted_later_append() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("base.rsidx");
    save(&path, &fixture()).unwrap();
    let guard = WriteGuard::acquire(&path).unwrap();
    let mut index = load(&path).unwrap();
    for _ in 0..3 {
        let d = transaction(&index);
        append_delta(&guard, &index, &d).unwrap();
        index.apply_delta(d).unwrap();
    }
    checkpoint_delta(&guard, &index).unwrap();
    let checkpoint = std::fs::read(delta_path(&path)).unwrap();
    let delta = transaction(&index);
    append_delta(&guard, &index, &delta).unwrap();
    let complete = std::fs::read(delta_path(&path)).unwrap();
    for cut in [8, 31, 32, 39, 40, 41, checkpoint.len() - 1] {
        std::fs::write(delta_path(&path), &checkpoint[..cut]).unwrap();
        assert!(load(&path).is_err(), "cut {cut}");
    }
    for cut in [checkpoint.len(), checkpoint.len() + 1, complete.len() - 1] {
        std::fs::write(delta_path(&path), &complete[..cut]).unwrap();
        let recovered = load(&path).unwrap();
        assert_eq!(recovered.log_sequence, index.log_sequence);
        append_delta(&guard, &recovered, &transaction(&recovered)).unwrap();
        assert_eq!(load(&path).unwrap().log_sequence, index.log_sequence + 1);
    }
    for at in [32, 50, checkpoint.len() - 10] {
        let mut corrupt = checkpoint.clone();
        corrupt[at] ^= 1;
        std::fs::write(delta_path(&path), corrupt).unwrap();
        assert!(load(&path).is_err());
    }
}

#[test]
fn automatic_checkpoint_reclaims_history_without_resetting_sequence() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("base.rsidx");
    save(&path, &fixture()).unwrap();
    let guard = WriteGuard::acquire(&path).unwrap();
    let mut index = load(&path).unwrap();
    for _ in 0..3 {
        let d = transaction(&index);
        append_delta(&guard, &index, &d).unwrap();
        index.apply_delta(d).unwrap();
    }
    let seq = index.log_sequence;
    let delta = transaction(&index);
    assert!(
        delta::append_with_threshold(&guard, &index, &delta, 1)
            .unwrap()
            .checkpointed
    );
    index.apply_delta(delta).unwrap();
    assert_eq!(load(&path).unwrap().log_sequence, seq + 1);
    let delta = transaction(&index);
    assert!(!append_delta(&guard, &index, &delta).unwrap().checkpointed);
    index.apply_delta(delta).unwrap();
    assert_eq!(load(&path).unwrap().log_sequence, seq + 2);
}
