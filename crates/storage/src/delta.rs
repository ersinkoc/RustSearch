use super::restrict_snapshot;
use anyhow::{Context, Result, ensure};
use rustsearch_core::{Delta, Index};
use serde::Serialize;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

const MAGIC: &[u8; 8] = b"RSDEL001";
const COMMIT: &[u8; 8] = b"COMMIT01";
const LOG_HEADER: usize = 32;
const MAX_LOG: u64 = 64 * 1024 * 1024;

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

/// Nonblocking single-writer lock held across load, NTFS collection and append.
/// Readers need no directory write access: they accept only complete committed frames.
pub struct WriteGuard {
    _file: File,
    path: PathBuf,
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
        })
    }
}
fn base_id(path: &Path) -> Result<[u8; 24]> {
    let mut id = [0; 24];
    File::open(path)?.read_exact(&mut id)?;
    Ok(id)
}

struct Log {
    bytes: Vec<u8>,
    committed: usize,
}
fn read_log(path: &Path, id: &[u8; 24]) -> Result<Option<Log>> {
    let mut file = match File::open(delta_path(path)) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let len = file.metadata()?.len();
    ensure!(
        len <= MAX_LOG,
        "delta log exceeds 64 MiB; compact to a new path"
    );
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    // A crash while creating an empty header has no committed transaction to recover.
    if bytes.len() < LOG_HEADER {
        return Ok(Some(Log {
            bytes,
            committed: 0,
        }));
    }
    ensure!(
        &bytes[..8] == MAGIC && &bytes[8..32] == id,
        "delta base identity mismatch"
    );
    let mut cursor = LOG_HEADER;
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
        cursor = end + 8;
    }
    Ok(Some(Log {
        bytes,
        committed: cursor,
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
    let mut cursor = LOG_HEADER;
    while cursor < log.committed {
        let length = u64::from_le_bytes(log.bytes[cursor..cursor + 8].try_into()?) as usize;
        let delta: Delta = serde_json::from_slice(&log.bytes[cursor + 12..cursor + 12 + length])?;
        index.apply_delta(delta)?;
        cursor += 20 + length;
    }
    Ok(())
}

#[derive(Serialize)]
pub struct AppendStats {
    pub bytes_appended: u64,
    pub delta_file_bytes: u64,
    pub recovered_partial_bytes: u64,
}

pub fn append_delta(guard: &WriteGuard, index: &Index, delta: &Delta) -> Result<AppendStats> {
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
    // Verify the durable checkpoint before accepting a transaction. The lock keeps other
    // cooperating writers from replacing the generation while NTFS collection is in flight.
    let log = read_log(&guard.path, &id)?;
    let mut durable_sequence = 0;
    let mut cursor = LOG_HEADER;
    if let Some(log) = &log {
        while cursor < log.committed {
            let len = u64::from_le_bytes(log.bytes[cursor..cursor + 8].try_into()?) as usize;
            let frame: Delta = serde_json::from_slice(&log.bytes[cursor + 12..cursor + 12 + len])?;
            ensure!(
                frame.previous_sequence == durable_sequence,
                "invalid durable delta sequence"
            );
            durable_sequence += 1;
            cursor += 20 + len;
        }
    }
    ensure!(
        durable_sequence == index.log_sequence,
        "stale writer state; reload index under writer lock"
    );
    let payload = serde_json::to_vec(delta)?;
    let committed = log.as_ref().map(|l| l.committed).unwrap_or(0);
    ensure!(
        committed as u64 + payload.len() as u64 + 20 + LOG_HEADER as u64 <= MAX_LOG,
        "delta budget reached; compact to a new path"
    );
    let recovered = log
        .as_ref()
        .map(|l| l.bytes.len() - l.committed)
        .unwrap_or(0);
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(delta_path(&guard.path))?;
    restrict_snapshot(&delta_path(&guard.path))?;
    file.set_len(committed as u64)?;
    file.seek(SeekFrom::Start(committed as u64))?;
    let mut appended = 0;
    if committed == 0 {
        file.write_all(MAGIC)?;
        file.write_all(&id)?;
        appended += LOG_HEADER as u64;
    }
    file.write_all(&(payload.len() as u64).to_le_bytes())?;
    file.write_all(&crc32fast::hash(&payload).to_le_bytes())?;
    file.write_all(&payload)?;
    // Commit marker and checkpoint become visible together; partial tails never advance replay.
    file.write_all(COMMIT)?;
    file.sync_all()?;
    appended += payload.len() as u64 + 20;
    Ok(AppendStats {
        bytes_appended: appended,
        delta_file_bytes: file.metadata()?.len(),
        recovered_partial_bytes: recovered as u64,
    })
}
