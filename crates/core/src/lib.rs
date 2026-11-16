use anyhow::{Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};

pub const DIRECTORY: u16 = 1;
pub const SIZE_KNOWN: u16 = 2;
pub const REPARSE: u16 = 4;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Checkpoint {
    pub journal_id: u64,
    pub next_usn: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Metadata {
    pub source: String,
    pub root: String,
    pub volume_id: String,
    pub filesystem: String,
    pub root_id: u64,
    pub created_unix_ms: u64,
    pub checkpoint: Option<Checkpoint>,
    pub pending_refresh: Vec<u64>,
    pub skipped_entries: u64,
    pub reparse_points_not_followed: u64,
}

/// One canonical name per NTFS file reference in this prototype; not every hard-link name.
#[derive(Clone, Copy, Debug)]
pub struct Entry {
    pub id: u64,
    pub parent: u64,
    pub usn: i64,
    pub size: u64,
    pub name_offset: u32,
    pub key_offset: u32,
    pub name_len: u16,
    pub flags: u16,
    pub key_len: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Record {
    pub id: u64,
    pub parent: u64,
    pub usn: i64,
    pub size: Option<u64>,
    pub flags: u16,
    pub name: Vec<u16>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Delta {
    pub previous_sequence: u64,
    pub previous_checkpoint: Option<Checkpoint>,
    pub metadata: Metadata,
    pub records: BTreeMap<u64, Option<Record>>,
}

#[derive(Debug)]
pub struct Index {
    pub metadata: Metadata,
    pub entries: Vec<Entry>,
    pub names: Vec<u16>,
    pub keys: Vec<u8>,
    pub updates: BTreeMap<u64, Option<Entry>>,
    pub log_sequence: u64,
    pub snapshot_identity: Option<[u8; 24]>,
}

/// Unicode lowercase, without locale or normalization. Original UTF-16 is preserved separately.
pub fn search_key(name: &[u16]) -> Vec<u8> {
    String::from_utf16_lossy(name).to_lowercase().into_bytes()
}

impl Index {
    pub fn new(metadata: Metadata) -> Self {
        Self {
            metadata,
            entries: Vec::new(),
            names: Vec::new(),
            keys: Vec::new(),
            updates: BTreeMap::new(),
            log_sequence: 0,
            snapshot_identity: None,
        }
    }

    pub fn push(&mut self, record: Record) -> Result<()> {
        ensure!(!record.name.is_empty(), "empty file name");
        ensure!(
            !record.name.iter().any(|c| matches!(*c, 0 | 47 | 92)),
            "invalid file name separator"
        );
        let key = search_key(&record.name);
        let name_offset = u32::try_from(self.names.len())?;
        let key_offset = u32::try_from(self.keys.len())?;
        let name_len = u16::try_from(record.name.len())?;
        let key_len = u32::try_from(key.len())?;
        ensure!(
            self.names
                .len()
                .checked_add(record.name.len())
                .is_some_and(|n| n <= u32::MAX as usize),
            "name arena exceeds 32-bit offsets"
        );
        ensure!(
            self.keys
                .len()
                .checked_add(key.len())
                .is_some_and(|n| n <= u32::MAX as usize),
            "key arena exceeds 32-bit offsets"
        );
        self.entries.push(Entry {
            id: record.id,
            parent: record.parent,
            usn: record.usn,
            size: record.size.unwrap_or(0),
            name_offset,
            key_offset,
            name_len,
            flags: (record.flags & !SIZE_KNOWN)
                | if record.size.is_some() { SIZE_KNOWN } else { 0 },
            key_len,
        });
        self.names.extend_from_slice(&record.name);
        self.keys.extend_from_slice(&key);
        Ok(())
    }

    pub fn finish(&mut self) -> Result<()> {
        self.entries.sort_unstable_by_key(|e| e.id);
        self.validate()
    }

    pub fn validate(&self) -> Result<()> {
        let mut previous = None;
        for entry in &self.entries {
            ensure!(
                previous.is_none_or(|id| id < entry.id),
                "duplicate or unsorted file ID"
            );
            previous = Some(entry.id);
            ensure!(
                entry.flags & !(DIRECTORY | SIZE_KNOWN | REPARSE) == 0,
                "unknown entry flags"
            );
            let end = (entry.name_offset as usize).checked_add(entry.name_len as usize);
            ensure!(
                entry.name_len > 0 && end.is_some_and(|n| n <= self.names.len()),
                "invalid name range"
            );
            let end = (entry.key_offset as usize).checked_add(entry.key_len as usize);
            ensure!(
                end.is_some_and(|n| n <= self.keys.len()),
                "invalid search key range"
            );
        }
        Ok(())
    }

    pub fn name(&self, entry: &Entry) -> &[u16] {
        &self.names
            [entry.name_offset as usize..entry.name_offset as usize + entry.name_len as usize]
    }

    pub fn key(&self, entry: &Entry) -> &[u8] {
        &self.keys[entry.key_offset as usize..entry.key_offset as usize + entry.key_len as usize]
    }

    pub fn get(&self, id: u64) -> Option<&Entry> {
        if let Some(update) = self.updates.get(&id) {
            return update.as_ref();
        }
        self.entries
            .binary_search_by_key(&id, |e| e.id)
            .ok()
            .map(|i| &self.entries[i])
    }

    pub fn live_entries(&self) -> impl Iterator<Item = &Entry> {
        self.entries
            .iter()
            .filter(|e| !self.updates.contains_key(&e.id))
            .chain(self.updates.values().filter_map(Option::as_ref))
    }

    /// Append only changed names and entries; no full generation or arena rebuild.
    pub fn apply_delta(&mut self, delta: Delta) -> Result<()> {
        ensure!(
            self.log_sequence == delta.previous_sequence,
            "delta sequence mismatch"
        );
        ensure!(
            self.metadata.checkpoint == delta.previous_checkpoint,
            "delta checkpoint mismatch"
        );
        ensure!(
            self.metadata.root == delta.metadata.root
                && self.metadata.root_id == delta.metadata.root_id
                && self.metadata.volume_id == delta.metadata.volume_id
                && self.metadata.source == delta.metadata.source,
            "delta volume/scope mismatch"
        );
        if let (Some(old), Some(new)) = (&self.metadata.checkpoint, &delta.metadata.checkpoint) {
            ensure!(
                old.journal_id == new.journal_id && new.next_usn >= old.next_usn,
                "delta checkpoint moved backwards or changed journal"
            );
        } else {
            ensure!(
                self.metadata.checkpoint == delta.metadata.checkpoint,
                "delta changed checkpoint type"
            );
        }
        // Validate and allocate before changing any visible entry or advancing the checkpoint.
        let mut staged = Index::new(self.metadata.clone());
        for (id, record) in &delta.records {
            if let Some(record) = record {
                ensure!(*id == record.id, "delta record ID mismatch");
                staged.push(record.clone())?;
            }
        }
        staged.finish()?;
        let name_base = u32::try_from(self.names.len())?;
        let key_base = u32::try_from(self.keys.len())?;
        ensure!(
            self.names
                .len()
                .checked_add(staged.names.len())
                .is_some_and(|n| n <= u32::MAX as usize),
            "delta name arena overflow"
        );
        ensure!(
            self.keys
                .len()
                .checked_add(staged.keys.len())
                .is_some_and(|n| n <= u32::MAX as usize),
            "delta key arena overflow"
        );
        self.names.try_reserve_exact(staged.names.len())?;
        self.keys.try_reserve_exact(staged.keys.len())?;
        self.names.extend(staged.names);
        self.keys.extend(staged.keys);
        let mut records = staged.entries.into_iter();
        for (id, record) in delta.records {
            let entry = record.map(|_| {
                let mut e = records.next().expect("validated staged record");
                e.name_offset += name_base;
                e.key_offset += key_base;
                e
            });
            self.updates.insert(id, entry);
        }
        self.metadata = delta.metadata;
        self.log_sequence += 1;
        self.metadata.pending_refresh.sort_unstable();
        self.metadata.pending_refresh.dedup();
        Ok(())
    }

    /// Explicit compaction only. Existing Entry identities and every original UTF-16 spelling survive.
    pub fn consolidate(&mut self) -> Result<()> {
        if !self.updates.is_empty() {
            let mut updates = std::mem::take(&mut self.updates).into_iter().peekable();
            let mut entries = Vec::with_capacity(self.entries.len() + updates.len());
            for old in std::mem::take(&mut self.entries) {
                while updates.peek().is_some_and(|(id, _)| *id < old.id) {
                    if let Some(e) = updates.next().unwrap().1 {
                        entries.push(e);
                    }
                }
                if updates.peek().is_some_and(|(id, _)| *id == old.id) {
                    if let Some(e) = updates.next().unwrap().1 {
                        entries.push(e);
                    }
                } else {
                    entries.push(old);
                }
            }
            for (_, entry) in updates {
                if let Some(e) = entry {
                    entries.push(e);
                }
            }
            entries.shrink_to_fit();
            self.entries = entries;
        }
        self.intern_names()?;
        self.log_sequence = 0;
        self.snapshot_identity = None;
        self.validate()
    }

    /// Share repeated names and search keys. No per-file persistent hash table is retained.
    pub fn intern_names(&mut self) -> Result<()> {
        let old_names = std::mem::take(&mut self.names);
        let old_keys = std::mem::take(&mut self.keys);
        let mut names: HashMap<&[u16], u32> = HashMap::new();
        let mut keys: HashMap<&[u8], u32> = HashMap::new();
        for entry in self
            .entries
            .iter_mut()
            .chain(self.updates.values_mut().filter_map(Option::as_mut))
        {
            let name = &old_names
                [entry.name_offset as usize..entry.name_offset as usize + entry.name_len as usize];
            entry.name_offset = if let Some(&offset) = names.get(name) {
                offset
            } else {
                let offset = u32::try_from(self.names.len())?;
                self.names.extend_from_slice(name);
                names.insert(name, offset);
                offset
            };
            let key = &old_keys
                [entry.key_offset as usize..entry.key_offset as usize + entry.key_len as usize];
            entry.key_offset = if let Some(&offset) = keys.get(key) {
                offset
            } else {
                let offset = u32::try_from(self.keys.len())?;
                self.keys.extend_from_slice(key);
                keys.insert(key, offset);
                offset
            };
        }
        self.names.shrink_to_fit();
        self.keys.shrink_to_fit();
        Ok(())
    }

    pub fn path_utf16(&self, entry: &Entry) -> Result<Vec<u16>> {
        let mut parts = vec![self.name(entry)];
        let mut parent = entry.parent;
        let mut seen = HashSet::new();
        seen.insert(entry.id);
        while parent != self.metadata.root_id {
            ensure!(
                seen.len() < 4096 && seen.insert(parent),
                "cyclic/deep parent chain"
            );
            let Some(ancestor) = self.get(parent) else {
                bail!("unresolved parent reference {parent}");
            };
            parts.push(self.name(ancestor));
            parent = ancestor.parent;
        }
        let mut path: Vec<u16> = self
            .metadata
            .root
            .trim_end_matches(['\\', '/'])
            .encode_utf16()
            .collect();
        for part in parts.into_iter().rev() {
            path.push(92);
            path.extend_from_slice(part);
        }
        Ok(path)
    }

    /// One-shot prototype rebuild: bounds delta lifetime, but temporarily holds two generations.
    pub fn apply(
        &self,
        mut metadata: Metadata,
        mut changes: BTreeMap<u64, Option<Record>>,
    ) -> Result<Self> {
        let mut result = Self::new(metadata.clone());
        // Account for replacements/deletions before reserving. Reserving the old length then
        // appending one extra name can otherwise double every large arena during compaction.
        let mut entry_count = self.entries.len();
        let mut name_units = self.names.len();
        let mut key_bytes = self.keys.len();
        for (id, change) in &changes {
            if let Some(old) = self.get(*id) {
                entry_count -= 1;
                name_units -= old.name_len as usize;
                key_bytes -= old.key_len as usize;
            }
            if let Some(record) = change {
                ensure!(*id == record.id, "change key does not match record ID");
                entry_count = entry_count
                    .checked_add(1)
                    .ok_or_else(|| anyhow::anyhow!("record count overflow"))?;
                name_units = name_units
                    .checked_add(record.name.len())
                    .ok_or_else(|| anyhow::anyhow!("name arena overflow"))?;
                key_bytes = key_bytes
                    .checked_add(search_key(&record.name).len())
                    .ok_or_else(|| anyhow::anyhow!("key arena overflow"))?;
            }
        }
        result.entries.try_reserve_exact(entry_count)?;
        result.names.try_reserve_exact(name_units)?;
        result.keys.try_reserve_exact(key_bytes)?;
        for entry in &self.entries {
            if !changes.contains_key(&entry.id) {
                result.push(Record {
                    id: entry.id,
                    parent: entry.parent,
                    usn: entry.usn,
                    size: (entry.flags & SIZE_KNOWN != 0).then_some(entry.size),
                    flags: entry.flags,
                    name: self.name(entry).to_vec(),
                })?;
            }
        }
        for (_, record) in std::mem::take(&mut changes) {
            if let Some(record) = record {
                result.push(record)?;
            }
        }
        metadata.pending_refresh.sort_unstable();
        metadata.pending_refresh.dedup();
        result.metadata = metadata;
        result.finish()?;
        Ok(result)
    }

    pub fn allocated_bytes(&self) -> usize {
        self.entries.capacity() * std::mem::size_of::<Entry>()
            + self.names.capacity() * 2
            + self.keys.capacity()
            + self.updates.len() * (std::mem::size_of::<Entry>() + 48)
    }

    pub fn counts(&self) -> (usize, usize, usize, usize) {
        let mut counts = (self.entries.len(), 0usize, 0usize, 0usize);
        let adjust = |counts: &mut (usize, usize, usize, usize), entry: &Entry, add: bool| {
            let values = [
                1,
                usize::from(entry.flags & DIRECTORY != 0),
                usize::from(entry.flags & (DIRECTORY | SIZE_KNOWN) == SIZE_KNOWN),
                usize::from(entry.flags & REPARSE != 0),
            ];
            if add {
                counts.0 += values[0];
                counts.1 += values[1];
                counts.2 += values[2];
                counts.3 += values[3];
            } else {
                counts.0 -= values[0];
                counts.1 -= values[1];
                counts.2 -= values[2];
                counts.3 -= values[3];
            }
        };
        counts.0 = 0;
        for entry in &self.entries {
            adjust(&mut counts, entry, true);
        }
        for (id, entry) in &self.updates {
            if let Ok(row) = self.entries.binary_search_by_key(id, |e| e.id) {
                adjust(&mut counts, &self.entries[row], false);
            }
            if let Some(entry) = entry {
                adjust(&mut counts, entry, true);
            }
        }
        counts
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    pub fn fixture() -> Index {
        Index::new(Metadata {
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
        })
    }
    fn record(id: u64, parent: u64, name: &str) -> Record {
        Record {
            id,
            parent,
            usn: 0,
            size: None,
            flags: 0,
            name: name.encode_utf16().collect(),
        }
    }
    #[test]
    fn resolves_paths_and_rejects_missing_or_cyclic_parents() {
        let mut i = fixture();
        i.push(record(2, 1, "folder")).unwrap();
        i.push(record(3, 2, "file.rs")).unwrap();
        i.finish().unwrap();
        assert_eq!(
            String::from_utf16(&i.path_utf16(i.get(3).unwrap()).unwrap()).unwrap(),
            "D:\\fixture\\folder\\file.rs"
        );
        i.entries[0].parent = 3;
        assert!(
            i.path_utf16(i.get(3).unwrap())
                .unwrap_err()
                .to_string()
                .contains("cyclic")
        );
        i.entries[0].parent = 999;
        assert!(
            i.path_utf16(i.get(3).unwrap())
                .unwrap_err()
                .to_string()
                .contains("unresolved")
        );
    }
    #[test]
    fn preserves_unpaired_utf16_and_updates_without_duplicate_ids() {
        let mut i = fixture();
        let mut r = record(2, 1, "x");
        r.name = vec![0xd800, 120];
        i.push(r).unwrap();
        i.finish().unwrap();
        assert_eq!(i.name(i.get(2).unwrap()), &[0xd800, 120]);
        let mut updates = BTreeMap::new();
        updates.insert(2, Some(record(2, 1, "renamed")));
        let updated = i.apply(i.metadata.clone(), updates).unwrap();
        assert_eq!(updated.entries.len(), 1);
        assert_eq!(updated.key(updated.get(2).unwrap()), b"renamed");
        let mut updates = BTreeMap::new();
        updates.insert(2, None);
        assert!(
            updated
                .apply(updated.metadata.clone(), updates)
                .unwrap()
                .entries
                .is_empty()
        );
    }
    #[test]
    fn rejects_duplicate_ids_and_invalid_arena_ranges() {
        let mut i = fixture();
        i.push(record(2, 1, "a")).unwrap();
        i.push(record(2, 1, "b")).unwrap();
        assert!(i.finish().is_err());
        i.entries.pop();
        i.entries[0].key_len = u32::MAX;
        assert!(i.validate().is_err());
    }
    #[test]
    fn a_single_insert_does_not_double_the_repacked_index_memory() {
        let mut index = fixture();
        for id in 2..1002 {
            index.push(record(id, 1, &format!("item{id:05}"))).unwrap();
        }
        index.finish().unwrap();
        let mut changes = BTreeMap::new();
        changes.insert(1002, Some(record(1002, 1, "item01002")));
        let updated = index.apply(index.metadata.clone(), changes).unwrap();
        let payload = updated.entries.len() * std::mem::size_of::<Entry>()
            + updated.names.len() * 2
            + updated.keys.len();
        assert!(
            updated.allocated_bytes() <= payload * 5 / 4,
            "one inserted record must not cause a second full arena allocation: allocated {}, payload {}",
            updated.allocated_bytes(),
            payload
        );
        assert_eq!(updated.entries.len(), 1001);
    }
}
