use super::restrict_snapshot;
use anyhow::{Context, Result, ensure};
use rustsearch_core::{Checkpoint, Delta, Index, Metadata, Record, SIZE_KNOWN};
use serde::Serialize;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

const MAGIC: &[u8; 8] = b"RSDEL001";
const CHECKPOINT_MAGIC: &[u8; 8] = b"RSDEL002";
const COMMIT: &[u8; 8] = b"COMMIT01";
const LOG_HEADER: usize = 32;
const CHECKPOINT_HEADER: usize = 40;
const MAX_LOG: u64 = 64 * 1024 * 1024;
const CHECKPOINT_AFTER: u64 = 32 * 1024 * 1024;

pub fn delta_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(".delta");
    PathBuf::from(name)
}
fn lock_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(".lock");
    PathBuf::from(name)
}
#[derive(Clone, Copy, PartialEq, Eq)]
struct Stamp {
    len: u64,
    modified: Option<SystemTime>,
    created: Option<SystemTime>,
}
fn stamp(path: &Path) -> Result<Option<Stamp>> {
    match std::fs::metadata(delta_path(path)) {
        Ok(m) => Ok(Some(Stamp {
            len: m.len(),
            modified: m.modified().ok(),
            created: m.created().ok(),
        })),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}
#[derive(Clone)]
struct Durable {
    identity: [u8; 24],
    stamp: Option<Stamp>,
    committed: usize,
    checkpoint_end: usize,
    sequence: u64,
    checkpoint: Option<Checkpoint>,
}
/// Nonblocking single-writer lock. Reuse it across updates to retain validated durable state.
/// Changes to log metadata invalidate that state; a new guard always validates the log.
pub struct WriteGuard {
    _file: File,
    path: PathBuf,
    durable: Mutex<Option<Durable>>,
}
impl WriteGuard {
    pub fn acquire(path: &Path) -> Result<Self> {
        let guard_path = lock_path(path);
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&guard_path)?;
        restrict_snapshot(&guard_path)?;
        file.try_lock()
            .map_err(|e| anyhow::anyhow!("another writer owns this index: {e}"))?;
        Ok(Self {
            _file: file,
            path: path.to_owned(),
            durable: Mutex::new(None),
        })
    }
}
fn base_id(path: &Path) -> Result<[u8; 24]> {
    let mut id = [0; 24];
    File::open(path)?.read_exact(&mut id)?;
    Ok(id)
}
fn base_metadata(path: &Path, id: &[u8; 24]) -> Result<Metadata> {
    let mut file = File::open(path)?;
    let mut found = [0; 24];
    file.read_exact(&mut found)?;
    ensure!(
        &found == id,
        "snapshot generation changed during log validation"
    );
    let mut length = [0; 8];
    file.read_exact(&mut length)?;
    let length = usize::try_from(u64::from_le_bytes(length))?;
    ensure!(
        length <= super::MAX_METADATA,
        "invalid base metadata length"
    );
    let mut bytes = vec![0; length];
    file.read_exact(&mut bytes)?;
    Ok(serde_json::from_slice(&bytes)?)
}
struct Log {
    bytes: Vec<u8>,
    committed: usize,
    header: usize,
    start_sequence: u64,
    checkpoint_end: usize,
}
fn read_log(path: &Path, id: &[u8; 24]) -> Result<Option<Log>> {
    let mut file = match File::open(delta_path(path)) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    ensure!(
        file.metadata()?.len() <= MAX_LOG,
        "delta log exceeds 64 MiB; compact to a new path"
    );
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    if bytes.len() < LOG_HEADER {
        ensure!(
            !bytes.starts_with(CHECKPOINT_MAGIC),
            "truncated checkpoint log header"
        );
        return Ok(Some(Log {
            bytes,
            committed: 0,
            header: LOG_HEADER,
            start_sequence: 0,
            checkpoint_end: 0,
        }));
    }
    ensure!(
        (&bytes[..8] == MAGIC || &bytes[..8] == CHECKPOINT_MAGIC) && &bytes[8..32] == id,
        "delta base identity mismatch"
    );
    let checkpointed = &bytes[..8] == CHECKPOINT_MAGIC;
    let (header, start_sequence) = if checkpointed {
        ensure!(
            bytes.len() >= CHECKPOINT_HEADER,
            "truncated checkpoint log header"
        );
        (
            CHECKPOINT_HEADER,
            u64::from_le_bytes(bytes[32..40].try_into()?),
        )
    } else {
        (LOG_HEADER, 0)
    };
    let mut cursor = header;
    let mut checkpoint_end = header;
    while cursor < bytes.len() {
        if bytes.len() - cursor < 12 {
            break;
        }
        let length = u64::from_le_bytes(bytes[cursor..cursor + 8].try_into()?);
        ensure!(length <= MAX_LOG, "invalid delta frame length");
        let end = cursor
            .checked_add(12)
            .and_then(|n| n.checked_add(length as usize))
            .context("delta length overflow")?;
        if end.checked_add(8).is_none_or(|end| end > bytes.len()) {
            break;
        }
        ensure!(
            &bytes[end..end + 8] == COMMIT,
            "invalid delta commit marker"
        );
        let expected = u32::from_le_bytes(bytes[cursor + 8..cursor + 12].try_into()?);
        ensure!(
            crc32fast::hash(&bytes[cursor + 12..end]) == expected,
            "committed delta checksum mismatch"
        );
        if cursor == header && checkpointed {
            checkpoint_end = end + 8;
        }
        cursor = end + 8;
    }
    ensure!(
        !checkpointed || cursor > header,
        "missing committed checkpoint frame"
    );
    Ok(Some(Log {
        bytes,
        committed: cursor,
        header,
        start_sequence,
        checkpoint_end,
    }))
}
pub(super) fn replay(path: &Path, index: &mut Index) -> Result<()> {
    let Some(log) = read_log(
        path,
        &index
            .snapshot_identity
            .context("missing loaded snapshot identity")?,
    )?
    else {
        return Ok(());
    };
    index.log_sequence = log.start_sequence;
    let mut cursor = log.header;
    while cursor < log.committed {
        let length = u64::from_le_bytes(log.bytes[cursor..cursor + 8].try_into()?) as usize;
        let delta: Delta = serde_json::from_slice(&log.bytes[cursor + 12..cursor + 12 + length])?;
        ensure!(index.log_sequence < u64::MAX, "delta sequence exhausted");
        index.apply_delta(delta)?;
        cursor += 20 + length;
    }
    Ok(())
}
fn validated(guard: &WriteGuard, id: [u8; 24], cached: Option<Durable>) -> Result<(Durable, u64)> {
    let before = stamp(&guard.path)?;
    if let Some(state) = cached
        && state.identity == id
        && state.stamp == before
        && before.is_none_or(|s| s.modified.is_some())
    {
        return Ok((state, 0));
    }
    let log = read_log(&guard.path, &id)?;
    let mut sequence = log.as_ref().map_or(0, |l| l.start_sequence);
    let mut checkpoint = base_metadata(&guard.path, &id)?.checkpoint;
    if let Some(log) = &log {
        let mut cursor = log.header;
        while cursor < log.committed {
            let len = u64::from_le_bytes(log.bytes[cursor..cursor + 8].try_into()?) as usize;
            let frame: Delta = serde_json::from_slice(&log.bytes[cursor + 12..cursor + 12 + len])?;
            ensure!(
                frame.previous_sequence == sequence,
                "invalid durable delta sequence"
            );
            ensure!(
                frame.previous_checkpoint == checkpoint,
                "invalid durable delta checkpoint"
            );
            checkpoint = frame.metadata.checkpoint;
            sequence = sequence
                .checked_add(1)
                .context("delta sequence exhausted")?;
            cursor += 20 + len;
        }
    }
    ensure!(
        stamp(&guard.path)? == before,
        "delta log changed during validation"
    );
    let bytes = log.as_ref().map_or(0, |l| l.bytes.len()) as u64;
    if before.is_some() {
        restrict_snapshot(&delta_path(&guard.path))?;
    }
    Ok((
        Durable {
            identity: id,
            stamp: before,
            committed: log.as_ref().map_or(0, |l| l.committed),
            checkpoint_end: log.as_ref().map_or(0, |l| l.checkpoint_end),
            sequence,
            checkpoint,
        },
        bytes,
    ))
}
#[derive(Serialize)]
pub struct AppendStats {
    pub bytes_appended: u64,
    pub delta_file_bytes: u64,
    pub recovered_partial_bytes: u64,
    pub validated_log_bytes: u64,
    pub checkpointed: bool,
}
fn check_writer(index: &Index, state: &Durable) -> Result<()> {
    ensure!(
        state.sequence == index.log_sequence && state.checkpoint == index.metadata.checkpoint,
        "stale writer state; reload index under writer lock"
    );
    Ok(())
}
fn write_frame(file: &mut File, payload: &[u8]) -> Result<()> {
    file.write_all(&(payload.len() as u64).to_le_bytes())?;
    file.write_all(&crc32fast::hash(payload).to_le_bytes())?;
    file.write_all(payload)?;
    file.write_all(COMMIT)?;
    Ok(())
}
fn checkpoint(guard: &WriteGuard, index: &Index, id: [u8; 24], reserve: u64) -> Result<Durable> {
    ensure!(
        index.log_sequence > 0,
        "cannot checkpoint an empty delta history"
    );
    let records = index
        .updates
        .iter()
        .map(|(&id, entry)| {
            (
                id,
                entry.as_ref().map(|e| Record {
                    id,
                    parent: e.parent,
                    usn: index.usn(e),
                    size: (e.flags & SIZE_KNOWN != 0).then(|| index.size(e)),
                    flags: index.flags(e),
                    name: index.name(e).into_owned(),
                }),
            )
        })
        .collect();
    let delta = Delta {
        previous_sequence: index.log_sequence - 1,
        previous_checkpoint: base_metadata(&guard.path, &id)?.checkpoint,
        metadata: index.metadata.clone(),
        records,
    };
    let payload = serde_json::to_vec(&delta)?;
    let length = CHECKPOINT_HEADER as u64 + 20 + payload.len() as u64;
    ensure!(
        length.checked_add(reserve).is_some_and(|n| n <= MAX_LOG),
        "delta budget reached; compact to a new path"
    );
    super::write_private_atomic(&delta_path(&guard.path), |file| {
        file.write_all(CHECKPOINT_MAGIC)?;
        file.write_all(&id)?;
        file.write_all(&(index.log_sequence - 1).to_le_bytes())?;
        write_frame(file, &payload)
    })?;
    Ok(Durable {
        identity: id,
        stamp: stamp(&guard.path)?,
        committed: length as usize,
        checkpoint_end: length as usize,
        sequence: index.log_sequence,
        checkpoint: index.metadata.checkpoint.clone(),
    })
}
/// Atomically fold committed history into one overlay checkpoint without changing IDs,
/// sequence numbers, the base snapshot, or its search cache. Older logs remain readable.
pub fn checkpoint_delta(guard: &WriteGuard, index: &Index) -> Result<AppendStats> {
    let mut cache = guard
        .durable
        .lock()
        .map_err(|_| anyhow::anyhow!("delta writer state poisoned"))?;
    let id = base_id(&guard.path)?;
    ensure!(
        index.snapshot_identity == Some(id),
        "snapshot generation changed; reload before appending"
    );
    let (state, validated_log_bytes) = validated(guard, id, cache.take())?;
    check_writer(index, &state)?;
    let recovered = state
        .stamp
        .map_or(0, |s| s.len.saturating_sub(state.committed as u64));
    let checkpointed = index.log_sequence > 0;
    let next = if checkpointed {
        checkpoint(guard, index, id, 0)?
    } else {
        state
    };
    let size = next.stamp.map_or(0, |s| s.len);
    *cache = Some(next);
    Ok(AppendStats {
        bytes_appended: 0,
        delta_file_bytes: size,
        recovered_partial_bytes: recovered,
        validated_log_bytes,
        checkpointed,
    })
}
pub fn append_delta(guard: &WriteGuard, index: &Index, delta: &Delta) -> Result<AppendStats> {
    append_with_threshold(guard, index, delta, CHECKPOINT_AFTER)
}
pub(super) fn append_with_threshold(
    guard: &WriteGuard,
    index: &Index,
    delta: &Delta,
    threshold: u64,
) -> Result<AppendStats> {
    ensure!(
        index.metadata.checkpoint == delta.previous_checkpoint,
        "append checkpoint mismatch"
    );
    let mut staging = Index::new(index.metadata.clone());
    staging.log_sequence = index.log_sequence;
    staging.apply_delta(delta.clone())?;
    let id = base_id(&guard.path)?;
    ensure!(
        index.snapshot_identity == Some(id),
        "snapshot generation changed; reload before appending"
    );
    let mut cache = guard
        .durable
        .lock()
        .map_err(|_| anyhow::anyhow!("delta writer state poisoned"))?;
    let (mut state, validated_log_bytes) = validated(guard, id, cache.take())?;
    check_writer(index, &state)?;
    let recovered = state
        .stamp
        .map_or(0, |s| s.len.saturating_sub(state.committed as u64));
    let payload = serde_json::to_vec(delta)?;
    let frame = payload.len() as u64 + 20;
    let mut checkpointed = false;
    let header = if state.committed == 0 {
        LOG_HEADER as u64
    } else {
        0
    };
    if index.log_sequence > 0
        && (state.committed.saturating_sub(state.checkpoint_end) as u64 >= threshold
            || (state.committed as u64)
                .checked_add(header)
                .and_then(|n| n.checked_add(frame))
                .is_none_or(|n| n > MAX_LOG))
    {
        state = checkpoint(guard, index, id, frame)?;
        checkpointed = true;
    }
    let header = if state.committed == 0 {
        LOG_HEADER as u64
    } else {
        0
    };
    ensure!(
        (state.committed as u64)
            .checked_add(header)
            .and_then(|n| n.checked_add(frame))
            .is_some_and(|n| n <= MAX_LOG),
        "delta budget reached; compact to a new path"
    );
    let log_path = delta_path(&guard.path);
    let existed = state.stamp.is_some();
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&log_path)?;
    if !existed {
        restrict_snapshot(&log_path)?;
    }
    file.set_len(state.committed as u64)?;
    file.seek(SeekFrom::Start(state.committed as u64))?;
    if state.committed == 0 {
        file.write_all(MAGIC)?;
        file.write_all(&id)?;
    }
    write_frame(&mut file, &payload)?;
    file.sync_all()?;
    drop(file);
    state.committed += header as usize + frame as usize;
    state.sequence += 1;
    state.checkpoint = delta.metadata.checkpoint.clone();
    state.stamp = stamp(&guard.path)?;
    let size = state.stamp.context("committed delta log disappeared")?.len;
    ensure!(
        size == state.committed as u64,
        "delta log changed during append"
    );
    *cache = Some(state);
    Ok(AppendStats {
        bytes_appended: header + frame,
        delta_file_bytes: size,
        recovered_partial_bytes: recovered,
        validated_log_bytes,
        checkpointed,
    })
}
