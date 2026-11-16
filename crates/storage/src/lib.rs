use anyhow::{Context, Result, ensure};
use rustsearch_core::{DIRECTORY, Entry, Index, Metadata, REPARSE, SIZE_KNOWN};
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
mod delta;
mod payload;
pub use delta::{WriteGuard, append_delta, delta_path};
pub use payload::PayloadReader;

const MAGIC: &[u8; 8] = b"RSIDX001";
const HEADER: u64 = 24;
const MAX_FILE: u64 = 8 * 1024 * 1024 * 1024;
const MAX_METADATA: usize = 16 * 1024 * 1024;
static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);
#[cfg(test)]
mod delta_tests;

struct Cleanup(PathBuf);
impl Drop for Cleanup {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

struct DigestWriter<W> {
    inner: W,
    hash: crc32fast::Hasher,
    length: u64,
}
impl<W: Write> Write for DigestWriter<W> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let n = self.inner.write(bytes)?;
        self.hash.update(&bytes[..n]);
        self.length += n as u64;
        Ok(n)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

/// Atomically publish a regenerable derived artifact with the same private ACL as snapshots.
/// The caller may hold a separate WriteGuard; this never changes the base snapshot or its log.
pub fn write_derived_atomic(
    path: &Path,
    write: impl FnOnce(&mut File) -> Result<()>,
) -> Result<()> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut name = path
        .file_name()
        .context("derived path has no filename")?
        .to_os_string();
    name.push(format!(
        ".tmp-{}-{}",
        std::process::id(),
        NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
    ));
    let temp = parent.join(name);
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)?;
    let cleanup = Cleanup(temp.clone());
    restrict_snapshot(&temp)?;
    write(&mut file)?;
    file.sync_all()?;
    drop(file);
    replace(&temp, path)?;
    drop(cleanup);
    Ok(())
}

pub fn save(path: &Path, index: &Index) -> Result<[u8; 24]> {
    let _guard = WriteGuard::acquire(path)?;
    ensure!(
        !delta_path(path).exists(),
        "output has a delta log; compact to a new path instead of overwriting it"
    );
    ensure!(
        index.updates.is_empty(),
        "consolidate before saving a complete snapshot"
    );
    index.validate()?;
    let metadata = serde_json::to_vec(&index.metadata)?;
    ensure!(
        metadata.len() <= MAX_METADATA,
        "metadata exceeds snapshot limit"
    );
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let name = path.file_name().context("snapshot path has no file name")?;
    let mut temp_name = name.to_os_string();
    temp_name.push(format!(
        ".tmp-{}-{}",
        std::process::id(),
        NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
    ));
    let temp = parent.join(temp_name);
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)
        .context("create snapshot temporary file")?;
    let cleanup = Cleanup(temp.clone());
    restrict_snapshot(&temp)?;
    let mut buffer = BufWriter::with_capacity(256 * 1024, file);
    buffer.write_all(&[0u8; HEADER as usize])?;
    let mut writer = DigestWriter {
        inner: buffer,
        hash: crc32fast::Hasher::new(),
        length: 0,
    };
    writer.write_all(&(metadata.len() as u64).to_le_bytes())?;
    writer.write_all(&metadata)?;
    for count in [index.entries.len(), index.names.len(), index.keys.len()] {
        writer.write_all(&(count as u64).to_le_bytes())?;
    }
    for e in &index.entries {
        writer.write_all(&e.id.to_le_bytes())?;
        writer.write_all(&e.parent.to_le_bytes())?;
        writer.write_all(&index.usn(e).to_le_bytes())?;
        writer.write_all(&index.size(e).to_le_bytes())?;
        writer.write_all(&e.name_offset.to_le_bytes())?;
        writer.write_all(&e.key_offset.to_le_bytes())?;
        writer.write_all(&e.name_len.to_le_bytes())?;
        writer.write_all(&index.flags(e).to_le_bytes())?;
        writer.write_all(&e.key_len.to_le_bytes())?;
    }
    let mut chunk = [0u8; 16384];
    for names in index.names.decoded_blocks() {
        for (n, unit) in names.iter().enumerate() {
            chunk[n * 2..n * 2 + 2].copy_from_slice(&unit.to_le_bytes());
        }
        writer.write_all(&chunk[..names.len() * 2])?;
    }
    writer.write_all(&index.keys)?;
    writer.flush()?;
    ensure!(
        writer.length + HEADER <= MAX_FILE,
        "snapshot exceeds 8 GiB format limit"
    );
    let checksum = writer.hash.finalize();
    let length = writer.length;
    let mut buffer = writer.inner;
    buffer.seek(SeekFrom::Start(0))?;
    buffer.write_all(MAGIC)?;
    buffer.write_all(&1u32.to_le_bytes())?;
    buffer.write_all(&length.to_le_bytes())?;
    buffer.write_all(&checksum.to_le_bytes())?;
    buffer.flush()?;
    buffer.get_ref().sync_all()?;
    drop(buffer);
    replace(&temp, path)?;
    drop(cleanup);
    let mut identity = [0; 24];
    identity[..8].copy_from_slice(MAGIC);
    identity[8..12].copy_from_slice(&1u32.to_le_bytes());
    identity[12..20].copy_from_slice(&length.to_le_bytes());
    identity[20..24].copy_from_slice(&checksum.to_le_bytes());
    Ok(identity)
}

#[cfg(windows)]
fn restrict_snapshot(path: &Path) -> Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::{
        Foundation::*,
        Security::{Authorization::*, *},
        System::Threading::*,
    };
    struct LocalAllocation(*mut core::ffi::c_void);
    impl Drop for LocalAllocation {
        fn drop(&mut self) {
            unsafe {
                LocalFree(self.0);
            }
        }
    }
    struct Token(HANDLE);
    impl Drop for Token {
        fn drop(&mut self) {
            unsafe {
                CloseHandle(self.0);
            }
        }
    }
    unsafe {
        let mut token = std::ptr::null_mut();
        ensure!(
            OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) != 0,
            "open owner token: {}",
            std::io::Error::last_os_error()
        );
        let token = Token(token);
        let mut length = 0;
        GetTokenInformation(token.0, TokenUser, std::ptr::null_mut(), 0, &mut length);
        ensure!(
            length as usize >= std::mem::size_of::<TOKEN_USER>(),
            "invalid token user size"
        );
        let mut buffer = vec![0u64; (length as usize).div_ceil(8)];
        ensure!(
            GetTokenInformation(
                token.0,
                TokenUser,
                buffer.as_mut_ptr().cast(),
                length,
                &mut length
            ) != 0,
            "read owner token: {}",
            std::io::Error::last_os_error()
        );
        // SAFETY: successful API call initialized an aligned TOKEN_USER and its in-buffer SID.
        let user = &*buffer.as_ptr().cast::<TOKEN_USER>();
        let mut sid_text = std::ptr::null_mut();
        ensure!(
            ConvertSidToStringSidW(user.User.Sid, &mut sid_text) != 0,
            "format owner SID"
        );
        let sid_allocation = LocalAllocation(sid_text.cast());
        let mut chars = 0;
        while *sid_text.add(chars) != 0 {
            chars += 1;
        }
        let sid = String::from_utf16(std::slice::from_raw_parts(sid_text, chars))?;
        let sddl = format!("D:P(A;;FA;;;SY)(A;;FA;;;BA)(A;;FA;;;{sid})");
        let encoded: Vec<u16> = sddl.encode_utf16().chain(Some(0)).collect();
        let mut descriptor = std::ptr::null_mut();
        ensure!(
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                encoded.as_ptr(),
                SDDL_REVISION_1,
                &mut descriptor,
                std::ptr::null_mut()
            ) != 0,
            "build snapshot ACL"
        );
        let descriptor = LocalAllocation(descriptor);
        let path: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        ensure!(
            SetFileSecurityW(
                path.as_ptr(),
                DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                descriptor.0
            ) != 0,
            "restrict snapshot ACL: {}",
            std::io::Error::last_os_error()
        );
        drop(sid_allocation);
    }
    Ok(())
}
#[cfg(not(windows))]
fn restrict_snapshot(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(windows)]
fn replace(from: &Path, to: &Path) -> Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
    };
    let from: Vec<_> = from.as_os_str().encode_wide().chain(Some(0)).collect();
    let to: Vec<_> = to.as_os_str().encode_wide().chain(Some(0)).collect();
    // SAFETY: both paths are NUL-terminated and live for the synchronous call.
    ensure!(
        unsafe {
            MoveFileExW(
                from.as_ptr(),
                to.as_ptr(),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
        } != 0,
        "publish snapshot: {}",
        std::io::Error::last_os_error()
    );
    Ok(())
}
#[cfg(not(windows))]
fn replace(from: &Path, to: &Path) -> Result<()> {
    std::fs::rename(from, to)?;
    Ok(())
}

fn read_array<const N: usize>(r: &mut impl Read) -> Result<[u8; N]> {
    let mut b = [0; N];
    r.read_exact(&mut b)?;
    Ok(b)
}
fn u64_value(r: &mut impl Read) -> Result<u64> {
    Ok(u64::from_le_bytes(read_array(r)?))
}
fn u32_value(r: &mut impl Read) -> Result<u32> {
    Ok(u32::from_le_bytes(read_array(r)?))
}

pub fn load(path: &Path) -> Result<Index> {
    let mut index = load_base(path)?;
    delta::replay(path, &mut index)?;
    Ok(index)
}

fn load_base(path: &Path) -> Result<Index> {
    let file = File::open(path).context("open snapshot")?;
    let length = file.metadata()?.len();
    ensure!(
        (HEADER..=MAX_FILE).contains(&length),
        "invalid snapshot length"
    );
    let mut reader = file;
    ensure!(
        &read_array::<8>(&mut reader)? == MAGIC,
        "invalid snapshot magic"
    );
    ensure!(u32_value(&mut reader)? == 1, "unsupported snapshot version");
    let payload = u64_value(&mut reader)?;
    ensure!(
        payload == length - HEADER,
        "truncated snapshot or trailing bytes"
    );
    let expected = u32_value(&mut reader)?;
    let mut reader = PayloadReader::new(reader, &[]);
    let metadata_len = usize::try_from(u64_value(&mut reader)?)?;
    ensure!(
        metadata_len <= MAX_METADATA && metadata_len as u64 <= payload.saturating_sub(32),
        "invalid metadata length"
    );
    let mut bytes = vec![0; metadata_len];
    reader.read_exact(&mut bytes)?;
    let metadata: Metadata = serde_json::from_slice(&bytes)?;
    let entries = u64_value(&mut reader)?;
    let names = u64_value(&mut reader)?;
    let keys = u64_value(&mut reader)?;
    let expected_len = entries
        .checked_mul(48)
        .and_then(|n| names.checked_mul(2).and_then(|m| n.checked_add(m)))
        .and_then(|n| n.checked_add(keys));
    ensure!(
        expected_len == Some(payload - 32 - metadata_len as u64),
        "invalid snapshot arena lengths"
    );
    ensure!(
        names <= u32::MAX as u64 && keys <= u32::MAX as u64,
        "snapshot arenas exceed format limits"
    );
    let mut index = Index::new(metadata);
    let mut identity = [0u8; 24];
    identity[..8].copy_from_slice(MAGIC);
    identity[8..12].copy_from_slice(&1u32.to_le_bytes());
    identity[12..20].copy_from_slice(&payload.to_le_bytes());
    identity[20..24].copy_from_slice(&expected.to_le_bytes());
    index.snapshot_identity = Some(identity);
    index.entries.try_reserve_exact(usize::try_from(entries)?)?;
    let mut block = vec![0u8; 256 * 1024];
    let mut remaining = entries;
    let mut previous = None;
    while remaining > 0 {
        let count = remaining.min((block.len() / 48) as u64) as usize;
        reader.read_exact(&mut block[..count * 48])?;
        for bytes in block[..count * 48].chunks_exact(48) {
            let u64_at = |at| u64::from_le_bytes(bytes[at..at + 8].try_into().unwrap());
            let u32_at = |at| u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap());
            let flags = u16::from_le_bytes(bytes[42..44].try_into()?);
            ensure!(
                flags & !(DIRECTORY | SIZE_KNOWN | REPARSE) == 0,
                "unknown entry flags"
            );
            let (value, flags) = index.store_values(
                i64::from_le_bytes(bytes[16..24].try_into()?),
                u64_at(24),
                flags,
            );
            let entry = Entry {
                id: u64_at(0),
                parent: u64_at(8),
                value,
                name_offset: u32_at(32),
                key_offset: u32_at(36),
                name_len: u16::from_le_bytes(bytes[40..42].try_into()?),
                flags,
                key_len: u32_at(44),
            };
            ensure!(
                previous.is_none_or(|id| id < entry.id),
                "duplicate or unsorted file ID"
            );
            previous = Some(entry.id);
            ensure!(
                index.flags(&entry) & !(DIRECTORY | SIZE_KNOWN | REPARSE) == 0,
                "unknown entry flags"
            );
            ensure!(
                entry.name_len > 0 && entry.name_offset as u64 + entry.name_len as u64 <= names,
                "invalid name range"
            );
            ensure!(
                entry.key_offset as u64 + entry.key_len as u64 <= keys,
                "invalid search key range"
            );
            index.entries.push(entry);
        }
        remaining -= count as u64;
    }
    // Compact names append in blocks; reserve directory capacity without a full UTF-16 pool.
    // The separate key arena below retains 64 KiB of headroom for small delta batches.
    index
        .names
        .try_reserve_exact(usize::try_from(names)?.saturating_add(65_536))?;
    let mut units = Vec::with_capacity(block.len() / 2);
    let mut remaining = names;
    while remaining > 0 {
        let count = remaining.min((block.len() / 2) as u64) as usize;
        reader.read_exact(&mut block[..count * 2])?;
        units.clear();
        units.extend(
            block[..count * 2]
                .chunks_exact(2)
                .map(|bytes| u16::from_le_bytes([bytes[0], bytes[1]])),
        );
        index.names.extend_from_slice(&units);
        remaining -= count as u64;
    }
    index
        .keys
        .try_reserve_exact(usize::try_from(keys)?.saturating_add(65_536))?;
    index.keys.resize(keys as usize, 0);
    reader.read_exact(&mut index.keys)?;
    reader.finish(expected)?;
    Ok(index)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustsearch_core::{DIRECTORY, Record};
    #[test]
    fn compact_memory_roundtrip_keeps_snapshot_bytes_including_unknown_raw_size() {
        let dir = tempfile::tempdir().unwrap();
        let before = dir.path().join("before.rsidx");
        let after = dir.path().join("after.rsidx");
        let mut source = fixture();
        let (value, flags) = source.store_values(i64::MIN, u64::MAX, 0);
        source.entries[0].value = value;
        source.entries[0].flags = flags;
        for id in 3..9000 {
            source
                .push(Record {
                    id,
                    parent: 1,
                    usn: i64::MAX,
                    size: Some(id * 65537),
                    flags: 0,
                    name: format!("cross-block-{id}-🙂-Ω").encode_utf16().collect(),
                })
                .unwrap();
        }
        source.finish().unwrap();
        save(&before, &source).unwrap();
        let loaded = load(&before).unwrap();
        save(&after, &loaded).unwrap();
        assert_eq!(
            std::fs::read(before).unwrap(),
            std::fs::read(after).unwrap()
        );
    }
    fn fixture() -> Index {
        let mut index = Index::new(Metadata {
            source: "walk".into(),
            root: "D:\\x".into(),
            volume_id: "test".into(),
            filesystem: "NTFS".into(),
            root_id: 1,
            created_unix_ms: 0,
            checkpoint: None,
            pending_refresh: vec![9],
            skipped_entries: 0,
            reparse_points_not_followed: 0,
        });
        index
            .push(Record {
                id: 2,
                parent: 1,
                usn: 42,
                size: Some(0),
                flags: DIRECTORY,
                name: vec![0xd800, 120],
            })
            .unwrap();
        index.finish().unwrap();
        index
    }
    #[test]
    fn roundtrip_and_atomic_replacement_preserve_utf16_and_checkpoint_metadata() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("test.rsidx");
        let mut index = fixture();
        save(&p, &index).unwrap();
        let read = load(&p).unwrap();
        assert_eq!(read.name(&read.entries[0]).as_ref(), &[0xd800, 120]);
        assert_eq!(read.metadata.pending_refresh, vec![9]);
        assert_eq!(read.usn(&read.entries[0]), 42);
        index.set_size(0, 1024);
        save(&p, &index).unwrap();
        let loaded = load(&p).unwrap();
        assert_eq!(loaded.size(&loaded.entries[0]), 1024);
        assert!(std::fs::read_dir(d.path()).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .contains(".tmp-")
        }));
    }
    #[test]
    fn rejects_truncation_checksum_damage_and_forged_lengths() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("test.rsidx");
        save(&p, &fixture()).unwrap();
        let original = std::fs::read(&p).unwrap();
        for count in [0, 23, original.len() - 1] {
            std::fs::write(&p, &original[..count]).unwrap();
            assert!(load(&p).is_err());
        }
        let mut damaged = original.clone();
        *damaged.last_mut().unwrap() ^= 1;
        std::fs::write(&p, &damaged).unwrap();
        assert!(load(&p).unwrap_err().to_string().contains("checksum"));
        let mut forged = original;
        let metadata_len = u64::from_le_bytes(forged[24..32].try_into().unwrap()) as usize;
        forged[32 + metadata_len..40 + metadata_len].copy_from_slice(&u64::MAX.to_le_bytes());
        let hash = crc32fast::hash(&forged[24..]);
        forged[20..24].copy_from_slice(&hash.to_le_bytes());
        std::fs::write(&p, forged).unwrap();
        assert!(load(&p).is_err());
    }

    #[test]
    fn bulk_snapshot_crosses_record_and_utf16_blocks_without_changing_fields() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("blocks.rsidx");
        let mut expected = fixture();
        for i in 0..6000u64 {
            let mut name = format!("long-name-long-name-long-name-long-name-{i:06}")
                .encode_utf16()
                .collect::<Vec<_>>();
            if i % 113 == 0 {
                name.push(0xd800);
            }
            expected
                .push(Record {
                    id: (7u64 << 48) + i,
                    parent: expected.metadata.root_id,
                    usn: -(i as i64),
                    size: if i % 3 == 0 { None } else { Some(i * 65537) },
                    flags: if i % 7 == 0 { DIRECTORY | REPARSE } else { 0 },
                    name,
                })
                .unwrap();
        }
        expected.finish().unwrap();
        save(&path, &expected).unwrap();
        let actual = load(&path).unwrap();
        assert_eq!(actual.entries.len(), expected.entries.len());
        assert_eq!(actual.names, expected.names);
        assert_eq!(actual.keys, expected.keys);
        for (a, b) in actual.entries.iter().zip(&expected.entries) {
            assert_eq!(
                (
                    a.id,
                    a.parent,
                    actual.usn(a),
                    actual.size(a),
                    actual.flags(a)
                ),
                (
                    b.id,
                    b.parent,
                    expected.usn(b),
                    expected.size(b),
                    expected.flags(b)
                )
            );
            assert_eq!(actual.name(a), expected.name(b));
            assert_eq!(actual.key(a), expected.key(b));
        }
    }
    #[test]
    fn bulk_decode_still_rejects_invalid_ids_flags_and_arena_ranges_with_valid_crc() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("invalid.rsidx");
        let mut index = fixture();
        index
            .push(Record {
                id: 3,
                parent: 1,
                usn: 2,
                size: None,
                flags: 0,
                name: vec![97],
            })
            .unwrap();
        index.finish().unwrap();
        save(&path, &index).unwrap();
        let original = std::fs::read(&path).unwrap();
        let metadata_len = u64::from_le_bytes(original[24..32].try_into().unwrap()) as usize;
        let record = 24 + 32 + metadata_len;
        for (offset, value, message) in [
            (record + 32, u32::MAX.to_le_bytes().to_vec(), "name range"),
            (record + 36, u32::MAX.to_le_bytes().to_vec(), "key range"),
            (record + 42, 0x8000u16.to_le_bytes().to_vec(), "flags"),
            (record + 40, 0u16.to_le_bytes().to_vec(), "name range"),
            (record + 48, 2u64.to_le_bytes().to_vec(), "sorted"),
        ] {
            let mut bytes = original.clone();
            bytes[offset..offset + value.len()].copy_from_slice(&value);
            let crc = crc32fast::hash(&bytes[24..]);
            bytes[20..24].copy_from_slice(&crc.to_le_bytes());
            std::fs::write(&path, bytes).unwrap();
            assert!(
                load(&path).unwrap_err().to_string().contains(message),
                "{message}"
            );
        }
    }
}
