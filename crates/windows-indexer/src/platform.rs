use crate::usn::{JournalRecord, parse_records};
use anyhow::{Context, Result, ensure};
use rustsearch_core::{Checkpoint, DIRECTORY, Delta, Index, Metadata, REPARSE, Record, SIZE_KNOWN};
use serde::Serialize;
use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::mem::size_of;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::fs::MetadataExt;
use std::path::Path;
use std::ptr::{null, null_mut};
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use windows_sys::Win32::{
    Foundation::*,
    Storage::FileSystem::*,
    System::{
        IO::DeviceIoControl,
        Ioctl::*,
        ProcessStatus::*,
        Threading::*,
        WindowsProgramming::{DRIVE_FIXED, DRIVE_REMOVABLE},
    },
};

struct Handle(HANDLE);

/// DeviceIoControl buffers must satisfy the filesystem's native alignment requirements.
struct AlignedBuffer(Vec<u64>);
impl AlignedBuffer {
    fn new(bytes: usize) -> Self {
        Self(vec![0; bytes.div_ceil(8)])
    }
}
impl std::ops::Deref for AlignedBuffer {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        // SAFETY: initialized u64 storage is also valid bytes; length is its allocated byte length.
        unsafe { std::slice::from_raw_parts(self.0.as_ptr().cast(), self.0.len() * 8) }
    }
}
impl std::ops::DerefMut for AlignedBuffer {
    fn deref_mut(&mut self) -> &mut [u8] {
        // SAFETY: the exclusive borrow prevents aliasing; every byte pattern is a valid u64.
        unsafe { std::slice::from_raw_parts_mut(self.0.as_mut_ptr().cast(), self.0.len() * 8) }
    }
}
impl Drop for Handle {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}
fn wide(value: impl AsRef<OsStr>) -> Vec<u16> {
    value.as_ref().encode_wide().chain(Some(0)).collect()
}
fn last_error() -> anyhow::Error {
    std::io::Error::last_os_error().into()
}
fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
fn flags(attributes: u32) -> u16 {
    (if attributes & FILE_ATTRIBUTE_DIRECTORY != 0 {
        DIRECTORY
    } else {
        0
    }) | (if attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        REPARSE
    } else {
        0
    })
}

#[derive(Debug, Serialize)]
pub struct VolumeInfo {
    pub mount: String,
    pub volume_id: String,
    pub filesystem: String,
    pub root_id: u64,
}

#[derive(Clone, Debug)]
pub struct LocalDrive {
    pub root: String,
    pub label: String,
    pub filesystem: String,
    pub removable: bool,
    pub total_bytes: u64,
    pub free_bytes: u64,
    pub error: Option<String>,
}

/// Enumerate assigned local fixed/removable drive letters without opening volume data.
/// Network/optical drives and unmounted volumes are deliberately outside this list.
pub fn local_drives() -> Result<Vec<LocalDrive>> {
    let mask = unsafe { GetLogicalDrives() };
    ensure!(
        mask != 0,
        "list local drives: {}",
        std::io::Error::last_os_error()
    );
    let mut drives = Vec::new();
    for letter in b'A'..=b'Z' {
        if mask & (1 << (letter - b'A')) == 0 {
            continue;
        }
        let root = format!("{}:\\", letter as char);
        let name = wide(&root);
        let kind = unsafe { GetDriveTypeW(name.as_ptr()) };
        if !matches!(kind, DRIVE_FIXED | DRIVE_REMOVABLE) {
            continue;
        }
        let mut label = [0u16; 261];
        let mut filesystem = [0u16; 64];
        let mut total = 0;
        let mut free = 0;
        // SAFETY: terminated root and writable buffers have the declared sizes.
        let ready = unsafe {
            GetVolumeInformationW(
                name.as_ptr(),
                label.as_mut_ptr(),
                label.len() as u32,
                null_mut(),
                null_mut(),
                null_mut(),
                filesystem.as_mut_ptr(),
                filesystem.len() as u32,
            ) != 0
                && GetDiskFreeSpaceExW(name.as_ptr(), null_mut(), &mut total, &mut free) != 0
        };
        let error = (!ready).then(|| std::io::Error::last_os_error().to_string());
        let text = |v: &[u16]| {
            String::from_utf16_lossy(&v[..v.iter().position(|&c| c == 0).unwrap_or(v.len())])
        };
        drives.push(LocalDrive {
            root,
            label: text(&label),
            filesystem: text(&filesystem),
            removable: kind == DRIVE_REMOVABLE,
            total_bytes: total,
            free_bytes: free,
            error,
        });
    }
    Ok(drives)
}

pub fn volume_info(path: &Path) -> Result<VolumeInfo> {
    let path = wide(path);
    let mut mount = vec![0u16; 32768];
    // SAFETY: buffers have the declared lengths; all calls are synchronous.
    unsafe {
        if GetVolumePathNameW(path.as_ptr(), mount.as_mut_ptr(), mount.len() as u32) == 0 {
            return Err(last_error()).context("resolve volume mount point");
        }
        let mut filesystem = [0u16; 64];
        if GetVolumeInformationW(
            mount.as_ptr(),
            null_mut(),
            0,
            null_mut(),
            null_mut(),
            null_mut(),
            filesystem.as_mut_ptr(),
            filesystem.len() as u32,
        ) == 0
        {
            return Err(last_error()).context("query filesystem");
        }
        let mut guid = [0u16; 128];
        if GetVolumeNameForVolumeMountPointW(mount.as_ptr(), guid.as_mut_ptr(), guid.len() as u32)
            == 0
        {
            return Err(last_error()).context("query volume identity");
        }
        let handle = CreateFileW(
            mount.as_ptr(),
            FILE_READ_ATTRIBUTES,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
            null_mut(),
        );
        ensure!(
            handle != INVALID_HANDLE_VALUE,
            "open root metadata: {}",
            std::io::Error::last_os_error()
        );
        let handle = Handle(handle);
        let mut info = BY_HANDLE_FILE_INFORMATION::default();
        if GetFileInformationByHandle(handle.0, &mut info) == 0 {
            return Err(last_error());
        }
        let text = |v: &[u16]| {
            String::from_utf16_lossy(&v[..v.iter().position(|&u| u == 0).unwrap_or(v.len())])
        };
        Ok(VolumeInfo {
            mount: text(&mount),
            volume_id: text(&guid),
            filesystem: text(&filesystem),
            root_id: ((info.nFileIndexHigh as u64) << 32) | info.nFileIndexLow as u64,
        })
    }
}

fn open_volume(volume: &str) -> Result<Handle> {
    let letter = volume.trim_end_matches(['\\', '/']);
    ensure!(
        letter.len() == 2 && letter.as_bytes()[0].is_ascii_alphabetic() && letter.ends_with(':'),
        "volume must be a drive letter, for example D:"
    );
    let path = wide(format!("\\\\.\\{}", letter));
    let handle = unsafe {
        CreateFileW(
            path.as_ptr(),
            GENERIC_READ,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            null(),
            OPEN_EXISTING,
            0,
            null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(last_error()).context("open NTFS volume; run this native command from an elevated terminal (no automatic fallback)");
    }
    Ok(Handle(handle))
}

fn ioctl<T>(handle: &Handle, code: u32, input: &T, output: &mut [u8]) -> Result<usize> {
    let mut bytes = 0;
    // SAFETY: T is a Win32 POD input; output is a writable byte buffer, never cast to aligned structs.
    let success = unsafe {
        DeviceIoControl(
            handle.0,
            code,
            (input as *const T).cast(),
            size_of::<T>() as u32,
            output.as_mut_ptr().cast(),
            u32::try_from(output.len())?,
            &mut bytes,
            null_mut(),
        )
    };
    if success == 0 {
        return Err(last_error());
    }
    ensure!(
        bytes as usize <= output.len(),
        "invalid DeviceIoControl output length"
    );
    Ok(bytes as usize)
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct JournalInfo {
    pub journal_id: u64,
    pub first_usn: i64,
    pub next_usn: i64,
    pub lowest_valid_usn: i64,
}
fn journal(handle: &Handle) -> Result<JournalInfo> {
    let mut output = AlignedBuffer::new(128);
    let mut bytes = 0;
    let ok = unsafe {
        DeviceIoControl(
            handle.0,
            FSCTL_QUERY_USN_JOURNAL,
            null(),
            0,
            output.as_mut_ptr().cast(),
            output.len() as u32,
            &mut bytes,
            null_mut(),
        )
    };
    if ok == 0 {
        return Err(last_error())
            .context("query existing USN journal (prototype never creates or modifies it)");
    }
    ensure!(bytes >= 32, "truncated journal information");
    Ok(JournalInfo {
        journal_id: u64::from_le_bytes(output[0..8].try_into()?),
        first_usn: i64::from_le_bytes(output[8..16].try_into()?),
        next_usn: i64::from_le_bytes(output[16..24].try_into()?),
        lowest_valid_usn: i64::from_le_bytes(output[24..32].try_into()?),
    })
}

pub fn probe(volume: &str) -> Result<(VolumeInfo, JournalInfo)> {
    let info = volume_info(Path::new(&format!("{}\\", volume.trim_end_matches('\\'))))?;
    ensure!(
        info.filesystem == "NTFS",
        "native indexing requires NTFS, found {}",
        info.filesystem
    );
    Ok((info, journal(&open_volume(volume)?)?))
}

pub fn check_checkpoint(checkpoint: &Checkpoint, current: &JournalInfo) -> Result<()> {
    ensure!(
        checkpoint.journal_id == current.journal_id,
        "journal identity changed: rebuild required"
    );
    ensure!(
        checkpoint.next_usn >= current.first_usn.max(current.lowest_valid_usn)
            && checkpoint.next_usn <= current.next_usn,
        "journal gap or invalid checkpoint: rebuild required"
    );
    Ok(())
}

#[derive(Debug, Default, Serialize)]
pub struct ScanStats {
    pub enumerate_ms: f64,
    pub size_collection_ms: f64,
    pub replay_ms: f64,
    pub records: u64,
    pub files: u64,
    pub folders: u64,
    pub sized_files: u64,
    pub size_attempts: u64,
    pub size_failures: u64,
    pub journal_events: u64,
    pub refreshed_objects: u64,
    pub deleted_objects: u64,
    pub refresh_failures: BTreeMap<String, u64>,
}
fn summarize(index: &mut Index, stats: &mut ScanStats) {
    index.metadata.reparse_points_not_followed = index
        .entries
        .iter()
        .filter(|e| e.flags & REPARSE != 0)
        .count() as u64;
    stats.records = index.entries.len() as u64;
    stats.folders = index
        .entries
        .iter()
        .filter(|e| e.flags & DIRECTORY != 0)
        .count() as u64;
    stats.files = stats.records - stats.folders;
    stats.sized_files = index
        .entries
        .iter()
        .filter(|e| e.flags & (DIRECTORY | SIZE_KNOWN) == SIZE_KNOWN)
        .count() as u64;
}
fn metadata(info: &VolumeInfo, source: &str, root: String, root_id: u64) -> Metadata {
    Metadata {
        source: source.into(),
        root,
        volume_id: info.volume_id.clone(),
        filesystem: info.filesystem.clone(),
        root_id,
        created_unix_ms: unix_ms(),
        checkpoint: None,
        pending_refresh: vec![],
        skipped_entries: 0,
        reparse_points_not_followed: 0,
    }
}

/// Normal directory enumeration, deliberately separate from the privileged NTFS backend.
pub fn walk(root: &Path, sizes: bool) -> Result<(Index, ScanStats)> {
    walk_controlled(root, sizes, |_| true)
}

/// Cooperatively stop folder traversal; the callback also observes completed record counts.
pub fn walk_controlled(
    root: &Path,
    sizes: bool,
    mut keep_going: impl FnMut(u64) -> bool,
) -> Result<(Index, ScanStats)> {
    let root = root.canonicalize().context("resolve scan root")?;
    ensure!(root.is_dir(), "scan root must be a directory");
    let root_meta = std::fs::symlink_metadata(&root)?;
    ensure!(
        root_meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT == 0,
        "walk root cannot be a reparse point"
    );
    let info = volume_info(&root)?;
    let root_text = root
        .to_str()
        .context("root path must be valid Unicode")?
        .to_owned();
    let mut index = Index::new(metadata(&info, "walk", root_text, 1));
    let start = Instant::now();
    let mut stats = ScanStats::default();
    let mut stack = vec![(root, 1u64)];
    let mut next = 2u64;
    while let Some((path, parent)) = stack.pop() {
        ensure!(keep_going(next - 2), "Folder indexing cancelled");
        let entries = match std::fs::read_dir(path) {
            Ok(entries) => entries,
            Err(_) => {
                index.metadata.skipped_entries += 1;
                continue;
            }
        };
        for entry in entries {
            ensure!(keep_going(next - 2), "Folder indexing cancelled");
            let entry = match entry {
                Ok(e) => e,
                Err(_) => {
                    index.metadata.skipped_entries += 1;
                    continue;
                }
            };
            let meta = match entry.metadata() {
                Ok(m) => m,
                Err(_) => {
                    index.metadata.skipped_entries += 1;
                    continue;
                }
            };
            let attributes = meta.file_attributes();
            let entry_flags = flags(attributes);
            let id = next;
            next += 1;
            if entry_flags & REPARSE != 0 {
                index.metadata.reparse_points_not_followed += 1;
            }
            index.push(Record {
                id,
                parent,
                usn: 0,
                size: (sizes && !meta.is_dir() && entry_flags & REPARSE == 0)
                    .then_some(meta.file_size()),
                flags: entry_flags,
                name: entry.file_name().encode_wide().collect(),
            })?;
            if meta.is_dir() && entry_flags & REPARSE == 0 {
                stack.push((entry.path(), id));
            }
        }
    }
    index.finish()?;
    stats.enumerate_ms = start.elapsed().as_secs_f64() * 1000.;
    summarize(&mut index, &mut stats);
    Ok((index, stats))
}

fn by_id(volume: &Handle, id: u64) -> Result<Handle> {
    let descriptor = FILE_ID_DESCRIPTOR {
        dwSize: size_of::<FILE_ID_DESCRIPTOR>() as u32,
        Type: FileIdType,
        Anonymous: FILE_ID_DESCRIPTOR_0 { FileId: id as i64 },
    };
    let raw = unsafe {
        OpenFileById(
            volume.0,
            &descriptor,
            FILE_READ_ATTRIBUTES,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            null(),
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
        )
    };
    if raw == INVALID_HANDLE_VALUE {
        return Err(last_error());
    }
    Ok(Handle(raw))
}

fn logical_size(file: &Handle) -> Result<Option<u64>> {
    let mut standard = FILE_STANDARD_INFO::default();
    let ok = unsafe {
        GetFileInformationByHandleEx(
            file.0,
            FileStandardInfo,
            (&mut standard as *mut FILE_STANDARD_INFO).cast(),
            size_of::<FILE_STANDARD_INFO>() as u32,
        )
    };
    if ok == 0 {
        return Err(last_error());
    }
    if standard.Directory {
        return Ok(None);
    }
    ensure!(standard.EndOfFile >= 0, "negative file length");
    Ok(Some(standard.EndOfFile as u64))
}

fn current_record(volume: &Handle, id: u64, sizes: bool) -> Result<Record> {
    let file = by_id(volume, id)?;
    let mut buffer = AlignedBuffer::new(65536);
    // Windows requires a DWORD-aligned request; READ_FILE_USN_DATA alone has Rust alignment 2.
    #[repr(C, align(4))]
    struct AlignedRequest(READ_FILE_USN_DATA);
    let n = ioctl(
        &file,
        FSCTL_READ_FILE_USN_DATA,
        &AlignedRequest(READ_FILE_USN_DATA {
            MinMajorVersion: 2,
            MaxMajorVersion: 2,
        }),
        &mut buffer,
    )?;
    let mut current = None;
    parse_records(&buffer[..n], |r| {
        ensure!(current.is_none(), "multiple current USN records");
        current = Some(r);
        Ok(())
    })?;
    let r = current.context("missing current USN record")?;
    ensure!(r.id == id, "file reference changed");
    let entry_flags = flags(r.attributes);
    let size = if sizes && entry_flags & (DIRECTORY | REPARSE) == 0 {
        logical_size(&file)?
    } else {
        None
    };
    Ok(Record {
        id: r.id,
        parent: r.parent,
        usn: r.usn,
        size,
        flags: entry_flags,
        name: r.name,
    })
}

fn collect_changes(
    volume: &Handle,
    checkpoint: &Checkpoint,
) -> Result<(JournalInfo, BTreeMap<u64, JournalRecord>, u64)> {
    let target = journal(volume)?;
    check_checkpoint(checkpoint, &target)?;
    let mut cursor = checkpoint.next_usn;
    let mut changes = BTreeMap::new();
    let mut count = 0u64;
    let mut buffer = AlignedBuffer::new(1024 * 1024);
    while cursor < target.next_usn {
        let input = READ_USN_JOURNAL_DATA_V0 {
            StartUsn: cursor,
            ReasonMask: u32::MAX,
            ReturnOnlyOnClose: 0,
            Timeout: 0,
            BytesToWaitFor: 0,
            UsnJournalID: target.journal_id,
        };
        let n = ioctl(volume, FSCTL_READ_USN_JOURNAL, &input, &mut buffer)
            .context("read USN journal")?;
        ensure!(n >= 8, "truncated journal cursor");
        let next = i64::from_le_bytes(buffer[..8].try_into()?);
        ensure!(next > cursor, "journal made no forward progress");
        parse_records(&buffer[8..n], |record| {
            if record.usn >= cursor && record.usn < target.next_usn {
                count += 1;
                changes.insert(record.id, record);
            }
            Ok(())
        })?;
        cursor = next;
    }
    check_checkpoint(checkpoint, &journal(volume)?)?;
    Ok((target, changes, count))
}

fn replay(volume: &Handle, index: &Index, sizes: bool, stats: &mut ScanStats) -> Result<Delta> {
    let start = Instant::now();
    let checkpoint = index
        .metadata
        .checkpoint
        .as_ref()
        .context("snapshot has no native checkpoint")?;
    let (target, mut events, count) = collect_changes(volume, checkpoint)?;
    for id in &index.metadata.pending_refresh {
        events.entry(*id).or_insert(JournalRecord {
            id: *id,
            parent: 0,
            usn: 0,
            reason: 0,
            attributes: 0,
            name: vec![],
        });
    }
    let mut changes = BTreeMap::new();
    let mut meta = index.metadata.clone();
    meta.pending_refresh.clear();
    for (id, event) in events {
        if id == meta.root_id {
            continue;
        }
        match current_record(
            volume,
            id,
            sizes || index.get(id).is_some_and(|e| e.flags & SIZE_KNOWN != 0),
        ) {
            Ok(record) => {
                changes.insert(id, Some(record));
                stats.refreshed_objects += 1;
            }
            Err(error) => {
                let missing = error
                    .downcast_ref::<std::io::Error>()
                    .and_then(|e| e.raw_os_error())
                    .is_some_and(|code| matches!(code, 2 | 3));
                if missing || event.reason & USN_REASON_FILE_DELETE != 0 {
                    changes.insert(id, None);
                    stats.deleted_objects += 1;
                } else {
                    meta.pending_refresh.push(id);
                    let code = error
                        .downcast_ref::<std::io::Error>()
                        .and_then(|e| e.raw_os_error())
                        .map(|code| format!("os_error_{code}"))
                        .unwrap_or_else(|| "invalid_metadata".into());
                    *stats.refresh_failures.entry(code).or_default() += 1;
                    if let Some(old) = index.get(id) {
                        changes.insert(
                            id,
                            Some(Record {
                                id,
                                parent: old.parent,
                                usn: index.usn(old),
                                size: None,
                                flags: index.flags(old),
                                name: index.name(old).to_vec(),
                            }),
                        );
                    }
                }
            }
        }
    }
    meta.checkpoint = Some(Checkpoint {
        journal_id: target.journal_id,
        next_usn: target.next_usn,
    });
    meta.created_unix_ms = unix_ms();
    let updated = Delta {
        previous_sequence: index.log_sequence,
        previous_checkpoint: index.metadata.checkpoint.clone(),
        metadata: meta,
        records: changes,
    };
    stats.journal_events += count;
    stats.replay_ms += start.elapsed().as_secs_f64() * 1000.;
    Ok(updated)
}

pub fn scan_ntfs(volume: &str, size_limit: Option<usize>) -> Result<(Index, ScanStats)> {
    scan_ntfs_controlled(volume, size_limit, |_| true)
}

/// Cancellable native enumeration, with cancellation checks between IOCTL batches and sizes.
pub fn scan_ntfs_controlled(
    volume: &str,
    size_limit: Option<usize>,
    mut keep_going: impl FnMut(u64) -> bool,
) -> Result<(Index, ScanStats)> {
    ensure!(keep_going(0), "Native indexing cancelled");
    let (info, first) = probe(volume)?;
    let handle = open_volume(volume)?;
    let mut meta = metadata(&info, "ntfs-usn", info.mount.clone(), info.root_id);
    meta.checkpoint = Some(Checkpoint {
        journal_id: first.journal_id,
        next_usn: first.next_usn,
    });
    let mut index = Index::new(meta);
    let mut stats = ScanStats::default();
    let start = Instant::now();
    let mut input = MFT_ENUM_DATA_V0 {
        StartFileReferenceNumber: 0,
        LowUsn: 0,
        HighUsn: i64::MAX,
    };
    let mut buffer = AlignedBuffer::new(1024 * 1024);
    let mut last = Instant::now();
    loop {
        ensure!(
            keep_going(index.entries.len() as u64),
            "Native indexing cancelled"
        );
        let n = match ioctl(&handle, FSCTL_ENUM_USN_DATA, &input, &mut buffer) {
            Ok(n) => n,
            Err(error)
                if error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|e| e.raw_os_error() == Some(ERROR_HANDLE_EOF as i32)) =>
            {
                break;
            }
            Err(error) => return Err(error).context("enumerate NTFS metadata"),
        };
        ensure!(n >= 8, "truncated enumeration cursor");
        let next = u64::from_le_bytes(buffer[..8].try_into()?);
        ensure!(
            next > input.StartFileReferenceNumber,
            "enumeration made no forward progress"
        );
        parse_records(&buffer[8..n], |r| {
            if r.id != info.root_id {
                index.push(Record {
                    id: r.id,
                    parent: r.parent,
                    usn: r.usn,
                    size: None,
                    flags: flags(r.attributes),
                    name: r.name,
                })?;
            }
            Ok(())
        })?;
        input.StartFileReferenceNumber = next;
        if last.elapsed().as_secs() >= 1 {
            eprintln!("enumerated {} NTFS records", index.entries.len());
            last = Instant::now();
        }
    }
    index.finish()?;
    stats.enumerate_ms = start.elapsed().as_secs_f64() * 1000.;
    if let Some(limit) = size_limit {
        let start = Instant::now();
        for row in 0..index.entries.len() {
            ensure!(
                keep_going(index.entries.len() as u64),
                "Native indexing cancelled"
            );
            let entry = index.entries[row];
            if stats.size_attempts >= limit as u64 {
                break;
            }
            if entry.flags & (DIRECTORY | REPARSE) != 0 {
                continue;
            }
            stats.size_attempts += 1;
            match by_id(&handle, entry.id).and_then(|file| logical_size(&file)) {
                Ok(Some(size)) => {
                    index.set_size(row, size);
                }
                _ => stats.size_failures += 1,
            }
        }
        stats.size_collection_ms = start.elapsed().as_secs_f64() * 1000.;
    }
    ensure!(
        keep_going(index.entries.len() as u64),
        "Native indexing cancelled"
    );
    let changes = replay(&handle, &index, size_limit == Some(usize::MAX), &mut stats)?;
    index.apply_delta(changes)?;
    index.consolidate()?;
    summarize(&mut index, &mut stats);
    Ok((index, stats))
}

pub fn refresh(index: &Index, sizes: bool) -> Result<(Delta, ScanStats)> {
    ensure!(
        index.metadata.source == "ntfs-usn",
        "refresh requires an NTFS/USN snapshot; walk snapshots must be rebuilt"
    );
    let (info, _) = probe(index.metadata.root.trim_end_matches('\\'))?;
    ensure!(
        info.volume_id == index.metadata.volume_id && info.root_id == index.metadata.root_id,
        "volume identity changed: rebuild required"
    );
    let handle = open_volume(index.metadata.root.trim_end_matches('\\'))?;
    let mut stats = ScanStats::default();
    let updated = replay(&handle, index, sizes, &mut stats)?;
    Ok((updated, stats))
}

#[derive(Debug, Clone, Serialize)]
pub struct ProcessSample {
    pub private_bytes: u64,
    pub peak_private_bytes: u64,
    pub working_set_bytes: u64,
    pub peak_working_set_bytes: u64,
    pub cpu_ms: f64,
    pub read_transfer_bytes: u64,
    pub write_transfer_bytes: u64,
    pub other_transfer_bytes: u64,
}
pub fn process_sample() -> Result<ProcessSample> {
    unsafe {
        let process = GetCurrentProcess();
        let mut memory = PROCESS_MEMORY_COUNTERS_EX {
            cb: size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32,
            ..Default::default()
        };
        if GetProcessMemoryInfo(
            process,
            (&mut memory as *mut PROCESS_MEMORY_COUNTERS_EX).cast(),
            memory.cb,
        ) == 0
        {
            return Err(last_error());
        }
        let mut creation = FILETIME::default();
        let mut exit = FILETIME::default();
        let mut kernel = FILETIME::default();
        let mut user = FILETIME::default();
        if GetProcessTimes(process, &mut creation, &mut exit, &mut kernel, &mut user) == 0 {
            return Err(last_error());
        }
        let mut io = IO_COUNTERS::default();
        if GetProcessIoCounters(process, &mut io) == 0 {
            return Err(last_error());
        }
        let ticks =
            |time: FILETIME| ((time.dwHighDateTime as u64) << 32) | time.dwLowDateTime as u64;
        Ok(ProcessSample {
            private_bytes: memory.PrivateUsage as u64,
            peak_private_bytes: memory.PeakPagefileUsage as u64,
            working_set_bytes: memory.WorkingSetSize as u64,
            peak_working_set_bytes: memory.PeakWorkingSetSize as u64,
            cpu_ms: (ticks(kernel) + ticks(user)) as f64 / 10000.,
            read_transfer_bytes: io.ReadTransferCount,
            write_transfer_bytes: io.WriteTransferCount,
            other_transfer_bytes: io.OtherTransferCount,
        })
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn native_cancellation_precedes_any_volume_access() {
        let error = super::scan_ntfs_controlled("not-a-volume", None, |_| false).unwrap_err();
        assert!(error.to_string().contains("cancelled"));
    }
    #[test]
    fn controlled_walk_can_cancel_without_modifying_source_files() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["first.txt", "second.txt", "third.txt"] {
            std::fs::write(dir.path().join(name), b"unchanged").unwrap();
        }
        let result = super::walk_controlled(dir.path(), true, |records| records < 2);
        assert!(result.unwrap_err().to_string().contains("cancelled"));
        assert_eq!(
            std::fs::read(dir.path().join("first.txt")).unwrap(),
            b"unchanged"
        );
        assert_eq!(super::walk(dir.path(), true).unwrap().0.entries.len(), 3);
    }
    use super::*;
    #[test]
    fn checkpoint_rejects_wrap_recreation_and_future_cursor() {
        let info = JournalInfo {
            journal_id: 9,
            first_usn: 100,
            next_usn: 200,
            lowest_valid_usn: 110,
        };
        for (id, usn, valid) in [
            (9, 110, true),
            (9, 200, true),
            (9, 109, false),
            (8, 150, false),
            (9, 201, false),
        ] {
            assert_eq!(
                check_checkpoint(
                    &Checkpoint {
                        journal_id: id,
                        next_usn: usn
                    },
                    &info
                )
                .is_ok(),
                valid
            );
        }
    }
    #[test]
    fn real_walk_preserves_zero_sizes_and_reports_missing_root() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("child")).unwrap();
        std::fs::write(dir.path().join("child/empty.txt"), b"").unwrap();
        std::fs::write(dir.path().join("data.bin"), [1u8; 17]).unwrap();
        let (index, stats) = walk(dir.path(), true).unwrap();
        assert_eq!(stats.records, 3);
        assert_eq!(stats.sized_files, 2);
        assert_eq!(index.entries.iter().map(|e| index.size(e)).sum::<u64>(), 17);
        let (_, without) = walk(dir.path(), false).unwrap();
        assert_eq!(without.sized_files, 0);
        assert!(walk(&dir.path().join("missing"), true).is_err());
    }
    #[test]
    fn walk_does_not_follow_a_real_junction_cycle() {
        use std::process::{Command, Stdio};
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("file.txt"), b"x").unwrap();
        let link = dir.path().join("cycle");
        let mut child = Command::new("cmd.exe")
            .args(["/D", "/C", "mklink", "/J"])
            .arg(&link)
            .arg(dir.path())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let start = Instant::now();
        while child.try_wait().unwrap().is_none() {
            if start.elapsed().as_secs() > 10 {
                child.kill().unwrap();
                panic!("mklink deadline exceeded");
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(
            child.wait().unwrap().success(),
            "fixture junction creation failed"
        );
        let result = walk(dir.path(), true);
        // Remove the link itself before TempDir cleanup; never recursively remove its target.
        std::fs::remove_dir(&link).unwrap();
        let (index, stats) = result.unwrap();
        assert_eq!(stats.records, 2);
        assert_eq!(index.metadata.reparse_points_not_followed, 1);
        assert_eq!(stats.sized_files, 1);
    }
    #[test]
    fn process_counters_are_available() {
        let sample = process_sample().unwrap();
        assert!(sample.working_set_bytes > 0);
        assert!(sample.peak_private_bytes >= sample.private_bytes);
    }

    #[test]
    fn current_file_refresh_returns_live_name_parent_and_size() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("before.txt");
        std::fs::write(&path, b"hello").unwrap();
        let encoded = wide(&path);
        let raw = unsafe {
            CreateFileW(
                encoded.as_ptr(),
                FILE_READ_ATTRIBUTES,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                null(),
                OPEN_EXISTING,
                0,
                null_mut(),
            )
        };
        assert_ne!(raw, INVALID_HANDLE_VALUE);
        let hint = Handle(raw);
        let mut info = BY_HANDLE_FILE_INFORMATION::default();
        assert_ne!(unsafe { GetFileInformationByHandle(hint.0, &mut info) }, 0);
        let id = ((info.nFileIndexHigh as u64) << 32) | info.nFileIndexLow as u64;
        let first =
            current_record(&hint, id, true).expect("current file metadata must be readable");
        assert_eq!(String::from_utf16(&first.name).unwrap(), "before.txt");
        assert_eq!(first.size, Some(5));
        std::fs::rename(&path, dir.path().join("after.txt")).unwrap();
        let after = current_record(&hint, id, true).unwrap();
        assert_eq!(String::from_utf16(&after.name).unwrap(), "after.txt");
        assert_eq!(after.id, id);
    }
}
