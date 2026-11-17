use anyhow::{Context, Result, ensure};
use notify::{
    Event, EventKind, RecursiveMode, Watcher,
    event::{ModifyKind, RenameMode},
};
use rustsearch_core::{DIRECTORY, Delta, Index, REPARSE, Record, SIZE_KNOWN};
use rustsearch_query::SearchIndex;
use std::os::windows::{ffi::OsStrExt, fs::MetadataExt};
use std::{
    collections::{BTreeMap, HashSet},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, atomic::AtomicBool},
};

#[derive(Default)]
pub struct Control {
    pub paused: AtomicBool,
    pub sizes: AtomicBool,
}
#[derive(Default)]
struct Queue {
    events: Vec<Event>,
    modified: HashSet<Vec<PathBuf>>,
    overflow: bool,
}
impl Queue {
    fn invalidate(&mut self) {
        self.overflow = true;
        self.events.clear();
        self.modified.clear();
    }
    fn push(&mut self, event: Event) {
        if event.need_rescan() {
            self.invalidate();
            return;
        }
        if self.overflow {
            return;
        }
        // Re-read final metadata once per path between structural events. A rename,
        // create or delete is a barrier: updates must never move across it.
        if matches!(
            event.kind,
            EventKind::Modify(ModifyKind::Any | ModifyKind::Data(_) | ModifyKind::Metadata(_))
        ) && !event.paths.is_empty()
        {
            if !self.modified.insert(event.paths.clone()) {
                return;
            }
        } else {
            self.modified.clear();
        }
        if self.events.len() >= 4096 {
            self.invalidate();
        } else {
            self.events.push(event);
        }
    }
}
pub struct LiveWatch {
    _watcher: notify::RecommendedWatcher,
    queue: Arc<Mutex<Queue>>,
    pub errors: Vec<String>,
}
pub struct BackgroundPriority(bool);
impl BackgroundPriority {
    pub fn begin() -> Self {
        // SAFETY: only this indexer's current thread is changed; query work restores normal priority.
        Self(
            unsafe {
                windows_sys::Win32::System::Threading::SetThreadPriority(
                    windows_sys::Win32::System::Threading::GetCurrentThread(),
                    0x00010000,
                )
            } != 0,
        )
    }
}
impl Drop for BackgroundPriority {
    fn drop(&mut self) {
        if self.0 {
            unsafe {
                windows_sys::Win32::System::Threading::SetThreadPriority(
                    windows_sys::Win32::System::Threading::GetCurrentThread(),
                    0x00020000,
                );
            }
        }
    }
}
impl LiveWatch {
    pub fn new(roots: &[PathBuf], exclude: PathBuf) -> Result<Self> {
        let queue = Arc::new(Mutex::new(Queue::default()));
        let queued = queue.clone();
        let mut watcher = notify::recommended_watcher(move |event: notify::Result<Event>| {
            let mut q = queued.lock().unwrap_or_else(|e| e.into_inner());
            match event {
                Ok(event) => {
                    if matches!(event.kind, EventKind::Access(_))
                        || (!event.paths.is_empty()
                            && event.paths.iter().all(|p| is_within(p, &exclude)))
                    {
                        return;
                    }
                    q.push(event);
                }
                Err(_) => {
                    q.invalidate();
                }
            }
        })?;
        let mut errors = vec![];
        for root in roots {
            if let Err(error) = watcher.watch(root, RecursiveMode::Recursive) {
                errors.push(format!("Watch {}: {error}", root.display()));
            }
        }
        Ok(Self {
            _watcher: watcher,
            queue,
            errors,
        })
    }
    pub fn take(&self) -> (Vec<Event>, bool) {
        let mut q = self.queue.lock().unwrap_or_else(|e| e.into_inner());
        q.modified.clear();
        (
            std::mem::take(&mut q.events),
            std::mem::take(&mut q.overflow),
        )
    }
}
fn is_within(path: &Path, root: &Path) -> bool {
    let clean = |p: &Path| crate::presentation::display_path(&p.to_string_lossy()).to_lowercase();
    let path = clean(path);
    let root = clean(root).trim_end_matches('\\').to_owned();
    path == root || path.starts_with(&(root + "\\"))
}
pub fn exclude_native_storage(index: &Index, delta: &mut Delta, exclude: &Path) {
    use std::os::windows::ffi::OsStringExt;
    let owned = |id: u64| -> bool {
        let mut current = id;
        let mut names = Vec::new();
        let mut seen = HashSet::new();
        let base = loop {
            if !seen.insert(current) || seen.len() > 1024 {
                return false;
            }
            if current == index.metadata.root_id {
                break PathBuf::from(&index.metadata.root);
            }
            if let Some(Some(record)) = delta.records.get(&current) {
                names.push(record.name.clone());
                current = record.parent;
            } else if let Some(entry) = index.get(current) {
                match index.path_utf16(entry) {
                    Ok(path) => break PathBuf::from(std::ffi::OsString::from_wide(&path)),
                    Err(_) => return false,
                }
            } else {
                return false;
            }
        };
        let mut path = base;
        for name in names.into_iter().rev() {
            path.push(std::ffi::OsString::from_wide(&name));
        }
        is_within(&path, exclude)
    };
    let excluded: HashSet<_> = delta
        .records
        .keys()
        .chain(delta.metadata.pending_refresh.iter())
        .copied()
        .filter(|id| owned(*id))
        .collect();
    delta.records.retain(|id, _| !excluded.contains(id));
    delta
        .metadata
        .pending_refresh
        .retain(|id| !excluded.contains(id));
}
fn as_record(index: &Index, id: u64) -> Option<Record> {
    index.get(id).map(|e| Record {
        id,
        parent: e.parent,
        usn: index.usn(e),
        size: (e.flags & SIZE_KNOWN != 0).then(|| index.size(e)),
        flags: index.flags(e),
        name: index.name(e).to_vec(),
    })
}
struct Edit<'a> {
    index: &'a Index,
    search: &'a SearchIndex<'a>,
    records: BTreeMap<u64, Option<Record>>,
    pending: HashSet<u64>,
    next: u64,
    sizes: bool,
}
impl Edit<'_> {
    fn child(&self, parent: u64, name: &[u16]) -> Option<u64> {
        for (&id, record) in &self.records {
            if let Some(r) = record
                && r.parent == parent
                && r.name == name
            {
                return Some(id);
            }
        }
        self.search
            .child(parent, name)
            .filter(|e| !self.records.contains_key(&e.id))
            .map(|e| e.id)
    }
    fn resolve(&self, path: &Path) -> Option<u64> {
        let root = Path::new(&self.index.metadata.root);
        let relative = path
            .strip_prefix(root)
            .or_else(|_| {
                path.strip_prefix(Path::new(&crate::presentation::display_path(
                    &self.index.metadata.root,
                )))
            })
            .ok()?;
        let mut parent = self.index.metadata.root_id;
        for component in relative.components() {
            let std::path::Component::Normal(name) = component else {
                return None;
            };
            parent = self.child(parent, &name.encode_wide().collect::<Vec<_>>())?;
        }
        Some(parent)
    }
    fn record(&self, id: u64) -> Option<Record> {
        self.records
            .get(&id)
            .cloned()
            .unwrap_or_else(|| as_record(self.index, id))
    }
    fn remove(&mut self, path: &Path) {
        let Some(id) = self.resolve(path) else {
            return;
        };
        if id == self.index.metadata.root_id {
            return;
        }
        let mut removed = HashSet::from([id]);
        if self.record(id).is_some_and(|r| r.flags & DIRECTORY != 0) {
            loop {
                let before = removed.len();
                for entry in self.index.live_entries() {
                    let parent = self
                        .records
                        .get(&entry.id)
                        .and_then(Option::as_ref)
                        .map_or(entry.parent, |r| r.parent);
                    if removed.contains(&parent) {
                        removed.insert(entry.id);
                    }
                }
                for record in self.records.values().filter_map(Option::as_ref) {
                    if removed.contains(&record.parent) {
                        removed.insert(record.id);
                    }
                }
                if removed.len() == before {
                    break;
                }
            }
        }
        for id in removed {
            self.records.insert(id, None);
            self.pending.remove(&id);
        }
    }
    fn upsert(&mut self, path: &Path, recursive: bool, preferred: Option<u64>) -> Result<()> {
        if !is_within(path, Path::new(&self.index.metadata.root)) {
            return Ok(());
        }
        if path == Path::new(&self.index.metadata.root) {
            return Ok(());
        }
        let Some(name) = path.file_name() else {
            return Ok(());
        };
        let parent_path = path.parent().context("missing parent")?;
        if self.resolve(parent_path).is_none() {
            self.upsert(parent_path, false, None)?;
        }
        let Some(parent) = self.resolve(parent_path) else {
            return Ok(());
        };
        if self.record(parent).is_some_and(|r| r.flags & REPARSE != 0) {
            return Ok(());
        }
        let name: Vec<u16> = name.encode_wide().collect();
        let old = preferred.or_else(|| self.child(parent, &name));
        let metadata = match std::fs::symlink_metadata(path) {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                self.remove(path);
                return Ok(());
            }
            Err(_) => {
                if let Some(id) = old {
                    self.pending.insert(id);
                }
                return Ok(());
            }
        };
        let reparse = metadata.file_attributes() & 0x400 != 0;
        let flags =
            if metadata.is_dir() { DIRECTORY } else { 0 } | if reparse { REPARSE } else { 0 };
        let id = if let Some(id) = old {
            id
        } else {
            let id = self.next;
            self.next = self.next.checked_add(1).context("file ID exhausted")?;
            id
        };
        let record = Record {
            id,
            parent,
            usn: 0,
            size: (self.sizes && !metadata.is_dir() && !reparse).then_some(metadata.file_size()),
            flags,
            name,
        };
        let changed = self.record(id).is_none_or(|r| {
            r.parent != record.parent
                || r.name != record.name
                || r.size != record.size
                || r.flags & (DIRECTORY | REPARSE) != flags
        });
        if changed {
            self.records.insert(id, Some(record));
        }
        self.pending.remove(&id);
        if recursive && metadata.is_dir() && !reparse {
            for entry in std::fs::read_dir(path)? {
                self.upsert(&entry?.path(), true, None)?;
            }
        }
        Ok(())
    }
}

/// Translate observed changes to a committed delta, preserving directory IDs across paired renames.
pub fn delta(
    index: &Index,
    search: &SearchIndex<'_>,
    events: &[Event],
    sizes: bool,
    exclude: &Path,
) -> Result<Delta> {
    let next = index
        .entries
        .last()
        .map_or(2, |e| e.id.saturating_add(1))
        .max(
            index
                .updates
                .keys()
                .next_back()
                .copied()
                .unwrap_or(1)
                .saturating_add(1),
        );
    let mut edit = Edit {
        index,
        search,
        records: BTreeMap::new(),
        pending: index.metadata.pending_refresh.iter().copied().collect(),
        next,
        sizes,
    };
    let paired: HashSet<_> = events
        .iter()
        .filter(|e| {
            matches!(
                e.kind,
                EventKind::Modify(ModifyKind::Name(RenameMode::Both))
            )
        })
        .filter_map(|e| e.paths.first().cloned())
        .collect();
    for event in events {
        ensure!(
            !event.need_rescan(),
            "watch overflow requires reconciliation"
        );
        if matches!(
            event.kind,
            EventKind::Modify(ModifyKind::Name(RenameMode::Both))
        ) && event.paths.len() == 2
        {
            let old = &event.paths[0];
            let new = &event.paths[1];
            if is_within(old, exclude) && is_within(new, exclude) {
                continue;
            }
            if is_within(new, exclude) {
                edit.remove(old);
                continue;
            }
            let id = edit.resolve(old);
            if is_within(new, Path::new(&index.metadata.root)) {
                edit.upsert(new, id.is_none(), id)?;
            } else {
                edit.remove(old);
            }
            continue;
        }
        for path in &event.paths {
            if is_within(path, exclude) || !is_within(path, Path::new(&index.metadata.root)) {
                continue;
            }
            match event.kind {
                EventKind::Remove(_) => edit.remove(path),
                EventKind::Modify(ModifyKind::Name(RenameMode::From)) if !paired.contains(path) => {
                    edit.remove(path)
                }
                EventKind::Modify(ModifyKind::Name(RenameMode::From)) => {}
                _ => edit.upsert(path, edit.resolve(path).is_none(), None)?,
            }
        }
    }
    let mut metadata = index.metadata.clone();
    for (&id, record) in &edit.records {
        let old = index.get(id).is_some_and(|e| e.flags & REPARSE != 0);
        let new = record.as_ref().is_some_and(|e| e.flags & REPARSE != 0);
        if new && !old {
            metadata.reparse_points_not_followed += 1;
        }
        if old && !new {
            metadata.reparse_points_not_followed =
                metadata.reparse_points_not_followed.saturating_sub(1);
        }
    }
    metadata.pending_refresh = edit.pending.into_iter().collect();
    metadata.pending_refresh.sort_unstable();
    Ok(Delta {
        previous_sequence: index.log_sequence,
        previous_checkpoint: index.metadata.checkpoint.clone(),
        metadata,
        records: edit.records,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn event(kind: EventKind, paths: Vec<PathBuf>) -> Event {
        let mut event = Event::new(kind);
        event.paths = paths;
        event
    }
    #[test]
    fn repeated_metadata_changes_do_not_force_reconciliation_and_keep_rename_order() {
        let mut queue = Queue::default();
        let path = PathBuf::from(r"C:\fixture\busy.log");
        for _ in 0..10_000 {
            queue.push(event(
                EventKind::Modify(ModifyKind::Any),
                vec![path.clone()],
            ));
        }
        assert!(
            !queue.overflow,
            "repeated writes to one file must fit in the bounded queue"
        );
        queue.push(event(
            EventKind::Modify(ModifyKind::Name(RenameMode::Both)),
            vec![path.clone(), PathBuf::from(r"C:\fixture\renamed.log")],
        ));
        queue.push(event(EventKind::Modify(ModifyKind::Any), vec![path]));
        assert_eq!(
            queue.events.len(),
            3,
            "updates on opposite sides of a rename remain ordered"
        );
        queue.push(Event::new(EventKind::Other).set_flag(notify::event::Flag::Rescan));
        assert!(
            queue.overflow,
            "real notification loss still requires reconciliation"
        );
        assert!(queue.events.is_empty());
    }
    fn commit(index: &mut Index, events: &[Event], path: &Path, exclude: &Path) {
        let search = SearchIndex::prepare(index, path, true).0;
        let change = delta(index, &search, events, true, exclude).unwrap();
        drop(search);
        let guard = rustsearch_storage::WriteGuard::acquire(path).unwrap();
        rustsearch_storage::append_delta(&guard, index, &change).unwrap();
        index.apply_delta(change).unwrap();
    }
    #[test]
    fn creates_resizes_moves_unicode_subtrees_and_deletes_without_rewriting_base() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source");
        let output = dir.path().join("data");
        std::fs::create_dir(&source).unwrap();
        std::fs::create_dir(&output).unwrap();
        std::fs::create_dir(source.join("Before")).unwrap();
        std::fs::write(source.join("Before/alpha.txt"), b"zero").unwrap();
        let (mut index, _) = rustsearch_windows::walk(&source, true).unwrap();
        let path = output.join("source.rsidx");
        index.snapshot_identity = Some(rustsearch_storage::save(&path, &index).unwrap());
        let cache = SearchIndex::prepare(&index, &path, true).0;
        let folder = cache
            .child(1, &"Before".encode_utf16().collect::<Vec<_>>())
            .unwrap()
            .id;
        let child = cache
            .child(folder, &"alpha.txt".encode_utf16().collect::<Vec<_>>())
            .unwrap()
            .id;
        drop(cache);
        let base = std::fs::read(&path).unwrap();
        let cached = std::fs::read(rustsearch_query::cache_path(&path)).unwrap();
        std::fs::rename(source.join("Before"), source.join("İstanbul-Ω")).unwrap();
        commit(
            &mut index,
            &[event(
                EventKind::Modify(ModifyKind::Name(RenameMode::Both)),
                vec![source.join("Before"), source.join("İstanbul-Ω")],
            )],
            &path,
            &output,
        );
        assert_eq!(
            index.path_utf16(index.get(child).unwrap()).unwrap(),
            source
                .join("İstanbul-Ω/alpha.txt")
                .canonicalize()
                .unwrap()
                .as_os_str()
                .encode_wide()
                .collect::<Vec<_>>()
        );
        std::fs::write(source.join("İstanbul-Ω/alpha.txt"), b"changed-size").unwrap();
        std::fs::write(source.join("new.txt"), b"").unwrap();
        commit(
            &mut index,
            &[event(
                EventKind::Modify(ModifyKind::Any),
                vec![source.join("İstanbul-Ω/alpha.txt"), source.join("new.txt")],
            )],
            &path,
            &output,
        );
        assert_eq!(index.size(index.get(child).unwrap()), 12);
        std::fs::remove_dir_all(source.join("İstanbul-Ω")).unwrap();
        commit(
            &mut index,
            &[event(
                EventKind::Remove(notify::event::RemoveKind::Folder),
                vec![source.join("İstanbul-Ω")],
            )],
            &path,
            &output,
        );
        assert!(index.get(folder).is_none());
        assert!(index.get(child).is_none());
        let loaded = rustsearch_storage::load(&path).unwrap();
        assert_eq!(loaded.counts().0, 1);
        let q = rustsearch_query::Query {
            text: "new.txt".into(),
            limit: 10,
            ..Default::default()
        };
        assert_eq!(q.execute(&loaded).total, 1);
        assert_eq!(std::fs::read(&path).unwrap(), base);
        assert_eq!(
            std::fs::read(rustsearch_query::cache_path(&path)).unwrap(),
            cached
        );
    }
}
