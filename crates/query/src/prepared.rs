use super::{Kind, Matches, Mode, Query};
use rustsearch_core::{DIRECTORY, Entry, Index, SIZE_KNOWN};
use std::collections::{BinaryHeap, HashMap};
mod cache;
pub use cache::{CacheStats, cache_path};

const DIR_BIT: u32 = 1 << 31;
#[derive(Clone, Copy)]
pub(super) struct Group {
    mask: u64,
    chars: u64,
    min_id: u64,
    key_offset: u32,
    key_len_kind: u32,
    start: u32,
    len: u32,
}
const fn char_bit(byte: u8) -> u64 {
    let bit = match byte {
        b'a'..=b'z' => byte - b'a',
        b'0'..=b'9' => 26 + byte - b'0',
        _ => 36 + byte % 28,
    };
    1u64 << bit
}
const CHAR_BITS: [u64; 256] = {
    let mut bits = [0; 256];
    let mut i = 0;
    while i < 256 {
        bits[i] = char_bit(i as u8);
        i += 1;
    }
    bits
};
fn chars(bytes: &[u8]) -> u64 {
    bytes
        .iter()
        .fold(0, |mask, &byte| mask | CHAR_BITS[byte as usize])
}
fn summaries(bytes: &[u8]) -> (u64, u64) {
    let mut characters = chars(&bytes[..bytes.len().min(2)]);
    let mut trigrams = 0;
    for w in bytes.windows(3) {
        characters |= CHAR_BITS[w[2] as usize];
        let hash = (w[0] as u32 * 961 + w[1] as u32 * 31 + w[2] as u32).wrapping_mul(0x9e3779b1);
        trigrams |= 1u64 << (hash >> 26);
    }
    (trigrams, characters)
}

impl Group {
    fn directory(self) -> bool {
        self.key_len_kind & DIR_BIT != 0
    }
    fn key<'a>(&self, index: &'a Index) -> &'a [u8] {
        &index.keys[self.key_offset as usize
            ..self.key_offset as usize + (self.key_len_kind & !DIR_BIT) as usize]
    }
}
fn grams(bytes: &[u8]) -> u64 {
    bytes.windows(3).fold(0, |mask, w| {
        let hash = (w[0] as u32 * 961 + w[1] as u32 * 31 + w[2] as u32).wrapping_mul(0x9e3779b1);
        mask | (1u64 << (hash >> 26))
    })
}
fn extension(key: &[u8]) -> Option<&[u8]> {
    memchr::memrchr(b'.', key).map(|at| &key[at + 1..])
}

/// O(number of distinct names) substring candidates, logarithmic exact/prefix/extension lookup,
/// and a separate known-size ordering. Query results retain the original ID/tie-break contract.
pub struct SearchIndex<'a> {
    index: &'a Index,
    overlay: Vec<&'a Entry>,
    groups: Vec<Group>,
    postings: Vec<u32>,
    by_extension: Vec<u32>,
    by_size: Vec<u32>,
    directories: usize,
    base_directories: usize,
    shadowed: Vec<u64>,
    removed_by_group: Vec<u32>,
    shadowed_count: usize,
    shadowed_sizes: Vec<u64>,
}
impl<'a> SearchIndex<'a> {
    pub fn build(index: &'a Index) -> Self {
        assert!(
            index.entries.len() + index.updates.len() < u32::MAX as usize,
            "search row ID overflow"
        );
        // Cache only the immutable base. Updated rows are bound separately and never change
        // cached row numbers or name offsets when delta insertions reorder the overlay.
        let overlay = Vec::new();
        let shadowed = vec![0u64; index.entries.len().div_ceil(64)];
        let mut result = Self {
            index,
            overlay,
            groups: Vec::new(),
            postings: Vec::new(),
            by_extension: Vec::new(),
            by_size: Vec::new(),
            directories: 0,
            base_directories: 0,
            shadowed: Vec::new(),
            removed_by_group: Vec::new(),
            shadowed_count: 0,
            shadowed_sizes: Vec::new(),
        };
        let mut ids: HashMap<(&[u8], bool), usize> = HashMap::new();
        let total_rows = index.entries.len() + result.overlay.len();
        for row in 0..total_rows {
            if row < index.entries.len() && shadowed[row / 64] & (1 << (row % 64)) != 0 {
                continue;
            }
            let entry = result.entry(row as u32);
            let key = index.key(entry);
            let dir = entry.flags & DIRECTORY != 0;
            let next = result.groups.len();
            let id = *ids.entry((key, dir)).or_insert_with(|| {
                result.groups.push(Group {
                    mask: grams(key),
                    chars: chars(key),
                    min_id: entry.id,
                    key_offset: entry.key_offset,
                    key_len_kind: entry.key_len | if dir { DIR_BIT } else { 0 },
                    start: 0,
                    len: 0,
                });
                next
            });
            result.groups[id].len += 1;
            result.groups[id].min_id = result.groups[id].min_id.min(entry.id);
            if dir {
                result.directories += 1;
            }
            if !dir && entry.flags & SIZE_KNOWN != 0 {
                result.by_size.push(row as u32);
            }
        }
        let mut count = 0u32;
        for group in &mut result.groups {
            group.start = count;
            count += group.len;
        }
        result.postings.resize(count as usize, 0);
        let mut cursors: Vec<_> = result.groups.iter().map(|g| g.start).collect();
        let mut overlay_groups = Vec::new();
        for row in 0..total_rows {
            if row < index.entries.len() && shadowed[row / 64] & (1 << (row % 64)) != 0 {
                continue;
            }
            let e = result.entry(row as u32);
            let group = ids[&(index.key(e), e.flags & DIRECTORY != 0)];
            result.postings[cursors[group] as usize] = row as u32;
            cursors[group] += 1;
            if row >= index.entries.len() {
                overlay_groups.push(group);
            }
        }
        drop(ids);
        drop(cursors);
        drop(shadowed);
        overlay_groups.sort_unstable();
        overlay_groups.dedup();
        for id in overlay_groups {
            let group = result.groups[id];
            let index = result.index;
            let overlay = &result.overlay;
            result.postings[group.start as usize..(group.start + group.len) as usize]
                .sort_unstable_by_key(|row| {
                    if (*row as usize) < index.entries.len() {
                        index.entries[*row as usize].id
                    } else {
                        overlay[*row as usize - index.entries.len()].id
                    }
                });
        }
        result.groups.sort_unstable_by(|a, b| {
            a.key(index)
                .cmp(b.key(index))
                .then(a.directory().cmp(&b.directory()))
        });
        result.by_extension = (0..result.groups.len() as u32)
            .filter(|&g| {
                !result.groups[g as usize].directory()
                    && extension(result.groups[g as usize].key(index)).is_some()
            })
            .collect();
        let groups = &result.groups;
        result.by_extension.sort_unstable_by(|a, b| {
            extension(groups[*a as usize].key(index))
                .cmp(&extension(groups[*b as usize].key(index)))
        });
        let overlay = &result.overlay;
        result.by_size.sort_unstable_by_key(|&row| {
            let e = if (row as usize) < index.entries.len() {
                &index.entries[row as usize]
            } else {
                overlay[row as usize - index.entries.len()]
            };
            (std::cmp::Reverse(e.size), e.id)
        });
        result.groups.shrink_to_fit();
        result.by_size.shrink_to_fit();
        result.base_directories = result.directories;
        result.bind_deltas();
        result
    }
    fn bind_deltas(&mut self) {
        self.overlay = self
            .index
            .updates
            .values()
            .filter_map(Option::as_ref)
            .collect();
        self.directories = self.base_directories;
        if self.index.updates.is_empty() {
            return;
        }
        self.shadowed = vec![0; self.index.entries.len().div_ceil(64)];
        self.removed_by_group = vec![0; self.groups.len()];
        for id in self.index.updates.keys() {
            if let Ok(row) = self.index.entries.binary_search_by_key(id, |e| e.id) {
                let old = &self.index.entries[row];
                self.shadowed[row / 64] |= 1 << (row % 64);
                self.shadowed_count += 1;
                let key = self.index.key(old);
                let directory = old.flags & DIRECTORY != 0;
                let group = self
                    .groups
                    .binary_search_by(|g| {
                        g.key(self.index)
                            .cmp(key)
                            .then(g.directory().cmp(&directory))
                    })
                    .expect("base group exists");
                self.removed_by_group[group] += 1;
                if directory {
                    self.directories -= 1;
                } else if old.flags & SIZE_KNOWN != 0 {
                    self.shadowed_sizes.push(old.size);
                }
            }
        }
        self.shadowed_sizes.sort_unstable_by(|a, b| b.cmp(a));
    }
    fn is_shadowed(&self, row: u32) -> bool {
        !self.shadowed.is_empty() && self.shadowed[row as usize / 64] & (1 << (row % 64)) != 0
    }
    fn entry(&self, row: u32) -> &'a Entry {
        if (row as usize) < self.index.entries.len() {
            &self.index.entries[row as usize]
        } else {
            self.overlay[row as usize - self.index.entries.len()]
        }
    }
    pub fn allocated_bytes(&self) -> usize {
        self.groups.capacity() * std::mem::size_of::<Group>()
            + self.postings.capacity() * 4
            + self.by_extension.capacity() * 4
            + self.by_size.capacity() * 4
            + self.overlay.capacity() * std::mem::size_of::<&Entry>()
            + self.shadowed.capacity() * 8
            + self.removed_by_group.capacity() * 4
            + self.shadowed_sizes.capacity() * 8
    }
    pub fn distinct_groups(&self) -> usize {
        self.groups.len()
    }
    pub fn execute(&self, query: &Query) -> Matches {
        let mut result = self.execute_base(query);
        if self.overlay.is_empty() {
            return result;
        }
        let key = query.text.to_lowercase().into_bytes();
        let finder = memchr::memmem::Finder::new(&key);
        let ext = query
            .extension
            .as_ref()
            .map(|s| format!(".{}", s.trim_start_matches('.').to_lowercase()).into_bytes());
        for &entry in &self.overlay {
            let directory = entry.flags & DIRECTORY != 0;
            if matches!(query.kind, Kind::File) && directory
                || matches!(query.kind, Kind::Folder) && !directory
            {
                continue;
            }
            if (query.largest || query.min_size.is_some())
                && (directory
                    || entry.flags & SIZE_KNOWN == 0
                    || entry.size < query.min_size.unwrap_or(0))
            {
                continue;
            }
            let name = self.index.key(entry);
            if ext
                .as_ref()
                .is_some_and(|e| directory || !name.ends_with(e))
            {
                continue;
            }
            let matched = match query.mode {
                Mode::Contains => finder.find(name).is_some(),
                Mode::Prefix => name.starts_with(&key),
                Mode::Exact => name == key,
            };
            if !matched {
                continue;
            }
            result.total += 1;
            if query.limit == 0 {
                continue;
            }
            let position = if query.largest {
                let rank = (std::cmp::Reverse(entry.size), entry.id);
                result.rows.partition_point(|&id| {
                    let old = self.index.get(id).expect("live result");
                    (std::cmp::Reverse(old.size), old.id) < rank
                })
            } else {
                result.rows.partition_point(|id| *id < entry.id)
            };
            if position < query.limit {
                result.rows.insert(position, entry.id);
                result.rows.truncate(query.limit);
            }
        }
        result
    }
    fn execute_base(&self, query: &Query) -> Matches {
        let key = query.text.to_lowercase().into_bytes();
        let mask = grams(&key);
        let finder = memchr::memmem::Finder::new(&key);
        let ext = query
            .extension
            .as_ref()
            .map(|s| format!(".{}", s.trim_start_matches('.').to_lowercase()).into_bytes());
        if key.is_empty()
            && !matches!(query.mode, Mode::Exact)
            && ext.is_none()
            && query.min_size.is_none()
            && !query.largest
        {
            let total = match query.kind {
                Kind::Any => self.postings.len() - self.shadowed_count,
                Kind::File => self.postings.len() - self.shadowed_count - self.directories,
                Kind::Folder => self.directories,
            };
            let mut rows = Vec::with_capacity(query.limit.min(total));
            let allowed = |e: &&Entry| match query.kind {
                Kind::Any => true,
                Kind::File => e.flags & DIRECTORY == 0,
                Kind::Folder => e.flags & DIRECTORY != 0,
            };
            rows.extend(
                self.index
                    .entries
                    .iter()
                    .filter(|e| !self.index.updates.contains_key(&e.id))
                    .filter(allowed)
                    .take(query.limit)
                    .map(|e| e.id),
            );
            return Matches { total, rows };
        }
        let char_mask = chars(&key);
        let single_letter = matches!(query.mode, Mode::Contains)
            && key.len() == 1
            && (key[0].is_ascii_lowercase() || key[0].is_ascii_digit());
        let matches_name = |name: &[u8]| match query.mode {
            Mode::Contains => finder.find(name).is_some(),
            Mode::Prefix => name.starts_with(&key),
            Mode::Exact => name == key,
        };
        let mut result = Matches {
            total: 0,
            rows: Vec::with_capacity(query.limit.min(self.postings.len())),
        };
        let mut smallest = BinaryHeap::<u64>::new();
        let mut retain = |id: u64| {
            if query.limit == 0 {
                return;
            }
            if smallest.len() < query.limit {
                smallest.push(id);
            } else if smallest.peek().is_some_and(|worst| id < *worst) {
                smallest.pop();
                smallest.push(id);
            }
        };
        if query.largest || query.min_size.is_some() {
            let end = self
                .by_size
                .partition_point(|&row| self.entry(row).size >= query.min_size.unwrap_or(0));
            if query.largest
                && key.is_empty()
                && !matches!(query.mode, Mode::Exact)
                && ext.is_none()
                && !matches!(query.kind, Kind::Folder)
            {
                return Matches {
                    total: end
                        - self
                            .shadowed_sizes
                            .partition_point(|&size| size >= query.min_size.unwrap_or(0)),
                    rows: self.by_size[..end]
                        .iter()
                        .filter(|&&row| !self.is_shadowed(row))
                        .take(query.limit)
                        .map(|&row| self.entry(row).id)
                        .collect(),
                };
            }
            if !matches!(query.kind, Kind::Folder) {
                for &row in &self.by_size[..end] {
                    if self.is_shadowed(row) {
                        continue;
                    }
                    let entry = self.entry(row);
                    let name = self.index.key(entry);
                    if !matches_name(name) || ext.as_ref().is_some_and(|e| !name.ends_with(e)) {
                        continue;
                    }
                    result.total += 1;
                    if query.largest {
                        if result.rows.len() < query.limit {
                            result.rows.push(entry.id);
                        }
                    } else {
                        retain(entry.id);
                    }
                }
            }
        } else {
            let (start, end) = match query.mode {
                Mode::Contains => (0, self.groups.len()),
                Mode::Prefix | Mode::Exact => {
                    let start = self
                        .groups
                        .partition_point(|g| g.key(self.index) < key.as_slice());
                    let end = if matches!(query.mode, Mode::Exact) {
                        start + self.groups[start..].partition_point(|g| g.key(self.index) == key)
                    } else {
                        start
                            + self.groups[start..]
                                .partition_point(|g| g.key(self.index).starts_with(&key))
                    };
                    (start, end)
                }
            };
            let range = ext.as_ref().filter(|e| !e[1..].contains(&b'.')).map(|e| {
                let suffix = &e[1..];
                let start = self.by_extension.partition_point(|&g| {
                    extension(self.groups[g as usize].key(self.index)).unwrap() < suffix
                });
                let end = start
                    + self.by_extension[start..].partition_point(|&g| {
                        extension(self.groups[g as usize].key(self.index)).unwrap() == suffix
                    });
                start..end
            });
            let candidates: Box<dyn Iterator<Item = usize>> =
                if let Some(range) = range.filter(|r| r.len() < end - start) {
                    Box::new(self.by_extension[range].iter().map(|&id| id as usize))
                } else {
                    Box::new(start..end)
                };
            for group_id in candidates {
                let group = &self.groups[group_id];
                let live_count =
                    group.len - self.removed_by_group.get(group_id).copied().unwrap_or(0);
                if live_count == 0 {
                    continue;
                }
                if matches!(query.kind, Kind::File) && group.directory()
                    || matches!(query.kind, Kind::Folder) && !group.directory()
                    || ext.is_some() && group.directory()
                {
                    continue;
                }
                if group.mask & mask != mask || group.chars & char_mask != char_mask {
                    continue;
                }
                if !single_letter || ext.is_some() {
                    let name = group.key(self.index);
                    if !matches_name(name) || ext.as_ref().is_some_and(|e| !name.ends_with(e)) {
                        continue;
                    }
                }
                result.total += live_count as usize;
                if query.limit == 0 {
                    continue;
                }
                if smallest.len() == query.limit
                    && smallest.peek().is_some_and(|worst| group.min_id >= *worst)
                {
                    continue;
                }
                // IDs within each posting are ascending. Once its smallest remaining ID cannot
                // enter the result heap, no subsequent file in this name group can enter either.
                for &row in &self.postings[group.start as usize..(group.start + group.len) as usize]
                {
                    if self.is_shadowed(row) {
                        continue;
                    }
                    let id = self.entry(row).id;
                    // Inline rather than borrowing retain's heap through two mutable closures.
                    if smallest.len() == query.limit
                        && smallest.peek().is_some_and(|worst| id >= *worst)
                    {
                        break;
                    }
                    if smallest.len() < query.limit {
                        smallest.push(id);
                    } else {
                        smallest.pop();
                        smallest.push(id);
                    }
                }
            }
        }
        if !query.largest {
            result.rows = smallest.into_sorted_vec();
        }
        result
    }
}
