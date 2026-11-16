use super::{DIR_BIT, Group, SearchIndex, extension, summaries};
use anyhow::{Context, Result, ensure};
use rustsearch_core::{DIRECTORY, Index, SIZE_KNOWN};
use rustsearch_storage::{PayloadReader, WriteGuard, write_derived_atomic};
use serde::Serialize;
use std::fs::File;
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;

const MAGIC: &[u8; 8] = b"RSQRY001";
// Bump whenever normalization, group/mask semantics, row coding or layout changes.
const VERSION: u32 = 2;
const HEADER: usize = 96;
const MAX_CACHE: u64 = 8 * 1024 * 1024 * 1024;
#[cfg(test)]
mod tests;

pub fn cache_path(snapshot: &Path) -> PathBuf {
    let mut path = snapshot.as_os_str().to_os_string();
    path.push(".search");
    PathBuf::from(path)
}

#[derive(Debug, Serialize)]
pub struct CacheStats {
    pub state: &'static str,
    pub reason: Option<String>,
    pub read_ms: f64,
    pub build_ms: f64,
    pub write_ms: f64,
    pub delta_bind_ms: f64,
    pub file_bytes: u64,
    pub bound_sequence: u64,
    pub overlay_records: usize,
    pub shadowed_base_rows: usize,
}
impl CacheStats {
    fn new(index: &Index) -> Self {
        Self {
            state: "disabled",
            reason: None,
            read_ms: 0.,
            build_ms: 0.,
            write_ms: 0.,
            delta_bind_ms: 0.,
            file_bytes: 0,
            bound_sequence: index.log_sequence,
            overlay_records: index.updates.values().filter(|e| e.is_some()).count(),
            shadowed_base_rows: 0,
        }
    }
}

impl<'a> SearchIndex<'a> {
    pub fn prepare(index: &'a Index, snapshot: &Path, use_cache: bool) -> (Self, CacheStats) {
        let mut stats = CacheStats::new(index);
        if !use_cache || index.snapshot_identity.is_none() {
            let start = Instant::now();
            let result = Self::build(index);
            stats.build_ms = start.elapsed().as_secs_f64() * 1000.;
            stats.shadowed_base_rows = result.shadowed_count;
            if use_cache {
                stats.state = "built_unsaved";
                stats.reason = Some("index has no persisted base identity".into());
            }
            return (result, stats);
        }
        let path = cache_path(snapshot);
        let start = Instant::now();
        match load(&path, index) {
            Ok((mut cached, version)) => {
                stats.read_ms = start.elapsed().as_secs_f64() * 1000.;
                stats.state = "hit";
                if version == 1 {
                    let upgrade = Instant::now();
                    stats.state = "legacy_hit";
                    if let Ok(_guard) = WriteGuard::acquire(&path) {
                        match save(&path, &cached) {
                            Ok(()) => stats.state = "upgraded",
                            Err(error) => {
                                stats.reason = Some(format!("cache upgrade failed: {error}"))
                            }
                        }
                    }
                    stats.write_ms = upgrade.elapsed().as_secs_f64() * 1000.;
                }
                stats.file_bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                let bind = Instant::now();
                cached.bind_deltas();
                stats.delta_bind_ms = bind.elapsed().as_secs_f64() * 1000.;
                stats.shadowed_base_rows = cached.shadowed_count;
                return (cached, stats);
            }
            Err(error) => {
                stats.read_ms = start.elapsed().as_secs_f64() * 1000.;
                stats.reason = Some(error.to_string());
            }
        }
        // A cache is optional. Read-only directories and another cache builder must not break search.
        let guard = WriteGuard::acquire(&path).ok();
        if guard.is_some()
            && let Ok((mut cached, _)) = load(&path, index)
        {
            stats.state = "hit_after_lock";
            stats.reason = None;
            stats.file_bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
            let bind = Instant::now();
            cached.bind_deltas();
            stats.delta_bind_ms = bind.elapsed().as_secs_f64() * 1000.;
            stats.shadowed_base_rows = cached.shadowed_count;
            return (cached, stats);
        }
        let start = Instant::now();
        let result = Self::build(index);
        stats.build_ms = start.elapsed().as_secs_f64() * 1000.;
        stats.shadowed_base_rows = result.shadowed_count;
        stats.state = "built_unsaved";
        if guard.is_some() {
            let start = Instant::now();
            match save(&path, &result) {
                Ok(()) => {
                    stats.state = "built";
                    stats.file_bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                }
                Err(error) => stats.reason = Some(format!("cache publish failed: {error}")),
            }
            stats.write_ms = start.elapsed().as_secs_f64() * 1000.;
        }
        (result, stats)
    }
}

fn number<const N: usize>(reader: &mut impl Read) -> Result<[u8; N]> {
    let mut bytes = [0; N];
    reader.read_exact(&mut bytes)?;
    Ok(bytes)
}
fn header_u64(header: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(header[at..at + 8].try_into().expect("fixed header range"))
}

fn load<'a>(path: &Path, index: &'a Index) -> Result<(SearchIndex<'a>, u32)> {
    let file = File::open(path).context("search cache missing or unavailable")?;
    let file_len = file.metadata()?.len();
    ensure!(
        (HEADER as u64..=MAX_CACHE).contains(&file_len),
        "invalid search cache size"
    );
    let mut reader = file;
    let header = number::<HEADER>(&mut reader)?;
    let version = u32::from_le_bytes(header[8..12].try_into()?);
    ensure!(
        &header[..8] == MAGIC && matches!(version, 1 | VERSION),
        "search cache format/version mismatch"
    );
    ensure!(
        Some(header[12..36].try_into()?) == index.snapshot_identity,
        "search cache base generation mismatch"
    );
    let entries = header_u64(&header, 36);
    let groups = header_u64(&header, 44);
    let postings = header_u64(&header, 52);
    let extensions = header_u64(&header, 60);
    let sizes = header_u64(&header, 68);
    let directories = header_u64(&header, 76);
    let payload = header_u64(&header, 84);
    ensure!(
        entries == index.entries.len() as u64
            && groups <= entries
            && postings == entries
            && extensions <= groups
            && sizes <= entries
            && directories <= entries,
        "invalid search cache cardinalities"
    );
    let expected = groups.checked_mul(40).and_then(|n| {
        postings
            .checked_add(extensions)
            .and_then(|x| x.checked_add(sizes))
            .and_then(|x| x.checked_mul(4))
            .and_then(|x| n.checked_add(x))
    });
    ensure!(
        expected == Some(payload) && payload == file_len - HEADER as u64,
        "search cache length mismatch"
    );
    let expected_checksum = u32::from_le_bytes(header[92..96].try_into()?);
    let mut reader = PayloadReader::new(reader, &header[..92]);
    let mut result = SearchIndex {
        index,
        overlay: Vec::new(),
        groups: Vec::new(),
        summaries: Vec::new(),
        postings: Vec::new(),
        by_extension: Vec::new(),
        by_size: Vec::new(),
        directories: directories as usize,
        base_directories: directories as usize,
        shadowed: Vec::new(),
        removed_by_group: Vec::new(),
        shadowed_count: 0,
        shadowed_sizes: Vec::new(),
    };
    result.groups.try_reserve_exact(groups as usize)?;
    result.summaries.try_reserve_exact(groups as usize)?;
    let mut block = vec![0u8; 256 * 1024];
    let mut remaining = groups;
    while remaining > 0 {
        let count = remaining.min((block.len() / 40) as u64) as usize;
        reader.read_exact(&mut block[..count * 40])?;
        for bytes in block[..count * 40].chunks_exact(40) {
            let u64_at = |at| u64::from_le_bytes(bytes[at..at + 8].try_into().unwrap());
            let u32_at = |at| u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap());
            result.summaries.push((u64_at(0), u64_at(8)));
            result.groups.push(Group {
                min_id: u64_at(16),
                key_offset: u32_at(24),
                key_len_kind: u32_at(28),
                start: u32_at(32),
                len: u32_at(36),
            });
        }
        remaining -= count as u64;
    }
    let mut expected_sizes = 0usize;
    if version == VERSION {
        validate_groups(&result)?;
        result.postings.try_reserve_exact(postings as usize)?;
        result.postings.resize(postings as usize, 0);
        let mut row = 0usize;
        while row < index.entries.len() {
            let count = (index.entries.len() - row).min(block.len() / 4);
            reader.read_exact(&mut block[..count * 4])?;
            for bytes in block[..count * 4].chunks_exact(4) {
                let id = u32::from_le_bytes(bytes.try_into().unwrap()) as usize;
                let group = result
                    .groups
                    .get_mut(id)
                    .context("search cache row group invalid")?;
                let entry = &index.entries[row];
                let key_len = group.key_len_kind & !DIR_BIT;
                ensure!(
                    (entry.flags & DIRECTORY != 0) == group.directory()
                        && ((entry.key_offset == group.key_offset && entry.key_len == key_len)
                            || index.key(entry) == group.key(index)),
                    "search cache posting membership invalid"
                );
                // The first row is the minimum ID because source rows have strict ID order.
                // The high length bit is free: the bounded payload has < 2^31 rows.
                if group.len & DIR_BIT == 0 {
                    ensure!(
                        entry.id == group.min_id,
                        "search cache representative invalid"
                    );
                    group.len |= DIR_BIT;
                }
                let slot = result
                    .postings
                    .get_mut(group.start as usize)
                    .context("search cache posting overflow")?;
                *slot = row as u32;
                group.start = group
                    .start
                    .checked_add(1)
                    .context("search cache posting overflow")?;
                expected_sizes += usize::from(entry.flags & (DIRECTORY | SIZE_KNOWN) == SIZE_KNOWN);
                row += 1;
            }
        }
        // start temporarily served as the write cursor. Every end must match the
        // declared prefix sum; this proves complete, non-overlapping group coverage.
        let mut end = 0u32;
        for group in &mut result.groups {
            ensure!(group.len & DIR_BIT != 0, "search cache empty group");
            group.len &= !DIR_BIT;
            end = end
                .checked_add(group.len)
                .context("search cache group overflow")?;
            ensure!(group.start == end, "search cache group coverage invalid");
            group.start -= group.len;
        }
    } else {
        read_rows(&mut reader, &mut block, postings, &mut result.postings)?;
    }
    read_rows(
        &mut reader,
        &mut block,
        extensions,
        &mut result.by_extension,
    )?;
    read_rows(&mut reader, &mut block, sizes, &mut result.by_size)?;
    reader.finish(expected_checksum)?;
    if version == VERSION {
        validate_auxiliary(&result, expected_sizes)?;
    } else {
        validate(&result)?;
    }
    Ok((result, version))
}

fn read_rows(
    reader: &mut impl Read,
    block: &mut [u8],
    count: u64,
    rows: &mut Vec<u32>,
) -> Result<()> {
    rows.try_reserve_exact(count as usize)?;
    let mut remaining = count;
    while remaining > 0 {
        let count = remaining.min((block.len() / 4) as u64) as usize;
        reader.read_exact(&mut block[..count * 4])?;
        rows.extend(
            block[..count * 4]
                .chunks_exact(4)
                .map(|bytes| u32::from_le_bytes(bytes.try_into().unwrap())),
        );
        remaining -= count as u64;
    }
    Ok(())
}

fn validate_groups(cache: &SearchIndex<'_>) -> Result<()> {
    let index = cache.index;
    let mut sum = 0u64;
    let mut directories = 0u64;
    let mut previous = None;
    for (group_id, group) in cache.groups.iter().enumerate() {
        let key_len = group.key_len_kind & !DIR_BIT;
        ensure!(
            key_len > 0 && group.key_offset as u64 + key_len as u64 <= index.keys.len() as u64,
            "search cache key range invalid"
        );
        ensure!(
            group.len > 0
                && group.len < DIR_BIT
                && group.start as u64 + group.len as u64 <= index.entries.len() as u64,
            "search cache posting range invalid"
        );
        ensure!(
            group.start as u64 == sum,
            "search cache noncanonical group range"
        );
        let key = group.key(index);
        let order = (key, group.directory());
        ensure!(
            previous.is_none_or(|old| old < order),
            "search cache groups not sorted"
        );
        previous = Some(order);
        ensure!(
            cache.summaries[group_id] == summaries(key),
            "search cache mask invalid"
        );
        sum += group.len as u64;
        if group.directory() {
            directories += group.len as u64;
        }
    }
    ensure!(
        sum == index.entries.len() as u64 && directories == cache.base_directories as u64,
        "search cache counts invalid"
    );
    Ok(())
}

fn validate(cache: &SearchIndex<'_>) -> Result<()> {
    let index = cache.index;
    let mut sum = 0u64;
    let mut directories = 0u64;
    let mut expected_sizes = 0usize;
    let mut previous: Option<(&[u8], bool)> = None;
    for (group_id, group) in cache.groups.iter().enumerate() {
        let key_len = (group.key_len_kind & !DIR_BIT) as usize;
        ensure!(
            key_len > 0
                && (group.key_offset as usize)
                    .checked_add(key_len)
                    .is_some_and(|end| end <= index.keys.len()),
            "search cache key range invalid"
        );
        ensure!(
            group.len > 0
                && group
                    .start
                    .checked_add(group.len)
                    .is_some_and(|end| end as usize <= cache.postings.len()),
            "search cache posting range invalid"
        );
        let key = group.key(index);
        let order = (key, group.directory());
        ensure!(
            previous.is_none_or(|old| old < order),
            "search cache groups not sorted"
        );
        previous = Some(order);
        ensure!(
            cache.summaries[group_id] == summaries(key),
            "search cache mask invalid"
        );
        let rows = &cache.postings[group.start as usize..(group.start + group.len) as usize];
        let mut last = None;
        for &row in rows {
            ensure!(
                (row as usize) < index.entries.len() && last.is_none_or(|old| old < row),
                "search cache posting invalid"
            );
            last = Some(row);
            let entry = &index.entries[row as usize];
            if entry.flags & (DIRECTORY | SIZE_KNOWN) == SIZE_KNOWN {
                expected_sizes += 1;
            }
            ensure!(
                (entry.flags & DIRECTORY != 0) == group.directory()
                    && ((entry.key_offset == group.key_offset
                        && entry.key_len as usize == key_len)
                        || index.key(entry) == key),
                "search cache posting membership invalid"
            );
        }
        let entry = &index.entries[rows[0] as usize];
        ensure!(
            entry.id == group.min_id
                && index.key(entry) == key
                && (entry.flags & DIRECTORY != 0) == group.directory(),
            "search cache representative invalid"
        );
        sum += group.len as u64;
        if group.directory() {
            directories += group.len as u64;
        }
    }
    ensure!(
        sum == cache.postings.len() as u64 && directories == cache.base_directories as u64,
        "search cache counts invalid"
    );
    validate_auxiliary(cache, expected_sizes)
}

fn validate_auxiliary(cache: &SearchIndex<'_>, expected_sizes: usize) -> Result<()> {
    let index = cache.index;
    let expected_extensions = cache
        .groups
        .iter()
        .filter(|group| !group.directory() && extension(group.key(index)).is_some())
        .count();
    let mut previous = None;
    ensure!(
        cache.by_extension.len() == expected_extensions && cache.by_size.len() == expected_sizes,
        "search cache auxiliary coverage invalid"
    );
    let mut seen_extensions = vec![0u64; cache.groups.len().div_ceil(64)];
    for &id in &cache.by_extension {
        let group = cache
            .groups
            .get(id as usize)
            .context("search cache extension group invalid")?;
        let slot = id as usize / 64;
        let bit = 1u64 << (id % 64);
        ensure!(
            seen_extensions[slot] & bit == 0,
            "search cache duplicate extension group"
        );
        seen_extensions[slot] |= bit;
        let ext = extension(group.key(index)).context("search cache extension missing")?;
        ensure!(
            !group.directory() && previous.is_none_or(|old| old <= ext),
            "search cache extensions not sorted"
        );
        previous = Some(ext);
    }
    let mut previous = None;
    for &row in &cache.by_size {
        let e = index
            .entries
            .get(row as usize)
            .context("search cache size row invalid")?;
        let rank = (std::cmp::Reverse(index.size(e)), e.id);
        ensure!(
            e.flags & (DIRECTORY | SIZE_KNOWN) == SIZE_KNOWN
                && previous.is_none_or(|old| old < rank),
            "search cache sizes not sorted"
        );
        previous = Some(rank);
    }
    Ok(())
}

struct HashWriter<W> {
    inner: W,
    hash: crc32fast::Hasher,
}
impl<W: Write> Write for HashWriter<W> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let n = self.inner.write(bytes)?;
        self.hash.update(&bytes[..n]);
        Ok(n)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}
fn save(path: &Path, cache: &SearchIndex<'_>) -> Result<()> {
    let mut header = [0u8; HEADER];
    header[..8].copy_from_slice(MAGIC);
    header[8..12].copy_from_slice(&VERSION.to_le_bytes());
    header[12..36].copy_from_slice(
        &cache
            .index
            .snapshot_identity
            .context("cache needs a base identity")?,
    );
    let payload = cache.groups.len() as u64 * 40
        + (cache.postings.len() + cache.by_extension.len() + cache.by_size.len()) as u64 * 4;
    ensure!(
        payload + HEADER as u64 <= MAX_CACHE,
        "search cache budget exceeded"
    );
    for (at, value) in [
        (36, cache.index.entries.len() as u64),
        (44, cache.groups.len() as u64),
        (52, cache.postings.len() as u64),
        (60, cache.by_extension.len() as u64),
        (68, cache.by_size.len() as u64),
        (76, cache.base_directories as u64),
        (84, payload),
    ] {
        header[at..at + 8].copy_from_slice(&value.to_le_bytes());
    }
    write_derived_atomic(path, |file| {
        file.write_all(&header)?;
        let mut hash = crc32fast::Hasher::new();
        hash.update(&header[..92]);
        let mut writer = HashWriter {
            inner: BufWriter::with_capacity(256 * 1024, &mut *file),
            hash,
        };
        let mut start = 0u32;
        for (group_id, group) in cache.groups.iter().enumerate() {
            let mut bytes = [0; 40];
            for (at, value) in [
                (0, cache.summaries[group_id].0),
                (8, cache.summaries[group_id].1),
                (16, group.min_id),
            ] {
                bytes[at..at + 8].copy_from_slice(&value.to_le_bytes());
            }
            for (at, value) in [
                (24, group.key_offset),
                (28, group.key_len_kind),
                (32, start),
                (36, group.len),
            ] {
                bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
            }
            writer.write_all(&bytes)?;
            start += group.len;
        }
        let mut owners = vec![0u32; cache.postings.len()];
        for (id, group) in cache.groups.iter().enumerate() {
            for &row in &cache.postings[group.start as usize..(group.start + group.len) as usize] {
                owners[row as usize] = id as u32;
            }
        }
        for rows in [&owners, &cache.by_extension, &cache.by_size] {
            let mut chunk = [0u8; 65536];
            for values in rows.chunks(chunk.len() / 4) {
                for (i, value) in values.iter().enumerate() {
                    chunk[i * 4..i * 4 + 4].copy_from_slice(&value.to_le_bytes());
                }
                writer.write_all(&chunk[..values.len() * 4])?;
            }
        }
        writer.flush()?;
        let checksum = writer.hash.clone().finalize();
        drop(writer);
        file.seek(SeekFrom::Start(92))?;
        file.write_all(&checksum.to_le_bytes())?;
        Ok(())
    })
}
