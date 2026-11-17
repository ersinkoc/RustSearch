use anyhow::{Context, Result, ensure};
use notify::{
    Event, EventKind,
    event::{ModifyKind, RenameMode},
};
use rustsearch_core::{DIRECTORY, Delta, Index, REPARSE, Record, SIZE_KNOWN};
use rustsearch_query::SearchIndex;
use std::os::windows::{ffi::OsStrExt, fs::MetadataExt};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, atomic::AtomicBool},
};

#[derive(Default)]
pub struct Control {
    pub paused: AtomicBool,
    pub sizes: AtomicBool,
}
const MAX_EVENTS: usize = 4096;
const MAX_FOLDERS: usize = 4096;
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct WatchStats {
    pub queue_recoveries: u64,
    pub backend_losses: u64,
    pub last_batch_folders: usize,
}
#[derive(Default)]
pub struct Changes {
    pub events: Vec<Event>,
    pub folders: Vec<(PathBuf, bool)>,
    pub stats: WatchStats,
}
impl Changes {
    pub fn is_empty(&self) -> bool {
        self.events.is_empty() && self.folders.is_empty()
    }
}
mod windows_watch;

#[derive(Default)]
struct Queue {
    events: Vec<Event>,
    modified: HashSet<Vec<PathBuf>>,
    folders: BTreeMap<PathBuf, bool>,
    overflow: bool,
    pending_renames: BTreeMap<PathBuf, (Event, std::time::Instant)>,
    stats: WatchStats,
}
impl Queue {
    fn invalidate(&mut self) {
        self.overflow = true;
        self.events.clear();
        self.modified.clear();
        self.folders.clear();
        self.pending_renames.clear();
        self.stats.backend_losses += 1;
    }
    fn folder(&mut self, path: PathBuf, recursive: bool) {
        if path.ancestors().any(|p| self.folders.get(p) == Some(&true)) {
            return;
        }
        if recursive {
            self.folders.retain(|p, _| !p.starts_with(&path));
        }
        self.folders
            .entry(path)
            .and_modify(|r| *r |= recursive)
            .or_insert(recursive);
        while self.folders.len() > MAX_FOLDERS {
            let old = std::mem::take(&mut self.folders);
            for (path, _) in old {
                let parent = path.parent().unwrap_or(&path).to_owned();
                if parent
                    .ancestors()
                    .any(|p| self.folders.get(p) == Some(&true))
                {
                    continue;
                }
                self.folders.retain(|p, _| !p.starts_with(&parent));
                self.folders.insert(parent, true);
            }
        }
    }
    fn fold(&mut self, event: &Event) {
        for path in &event.paths {
            self.folder(path.parent().unwrap_or(path).to_owned(), false);
        }
    }
    fn retain(&mut self, event: Event) {
        if self.overflow {
            return;
        }
        if !self.folders.is_empty() {
            self.fold(&event);
            // Preserve paired rename identity even when neighboring data events coalesce.
            if matches!(
                event.kind,
                EventKind::Modify(ModifyKind::Name(RenameMode::Both))
            ) && self.events.len() < MAX_EVENTS
            {
                self.events.push(event);
            }
            return;
        }
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
        if self.events.len() >= MAX_EVENTS {
            self.stats.queue_recoveries += 1;
            let old = std::mem::take(&mut self.events);
            for item in old {
                self.fold(&item);
                if matches!(
                    item.kind,
                    EventKind::Modify(ModifyKind::Name(RenameMode::Both))
                ) {
                    self.events.push(item);
                }
            }
            self.modified.clear();
            self.fold(&event);
            if matches!(
                event.kind,
                EventKind::Modify(ModifyKind::Name(RenameMode::Both))
            ) && self.events.len() < MAX_EVENTS
            {
                self.events.push(event);
            }
        } else {
            self.events.push(event);
        }
    }
    #[cfg(test)]
    fn push(&mut self, event: Event) {
        let scope = event
            .paths
            .first()
            .and_then(|p| p.ancestors().last())
            .map(Path::to_path_buf)
            .unwrap_or_default();
        self.push_in_scope(scope, event);
    }
    fn push_in_scope(&mut self, scope: PathBuf, event: Event) {
        if event.need_rescan() {
            if event.paths.is_empty() {
                self.invalidate();
            } else {
                self.stats.backend_losses += 1;
                for path in event.paths {
                    self.pending_renames.remove(&path);
                    self.folder(path, true);
                }
            }
            return;
        }
        if self.overflow {
            return;
        }
        if let Some((old, _)) = self.pending_renames.remove(&scope) {
            if matches!(
                event.kind,
                EventKind::Modify(ModifyKind::Name(RenameMode::To))
            ) && old.paths.len() == 1
                && event.paths.len() == 1
            {
                let mut both = Event::new(EventKind::Modify(ModifyKind::Name(RenameMode::Both)));
                both.paths = vec![old.paths[0].clone(), event.paths[0].clone()];
                self.retain(both);
                return;
            }
            self.retain(old);
        }
        if matches!(
            event.kind,
            EventKind::Modify(ModifyKind::Name(RenameMode::From))
        ) {
            self.pending_renames
                .insert(scope, (event, std::time::Instant::now()));
        } else {
            self.retain(event);
        }
    }
    fn drain(&mut self, roots: &[PathBuf]) -> Changes {
        let expired: Vec<_> = self
            .pending_renames
            .iter()
            .filter(|(_, (_, at))| at.elapsed() >= std::time::Duration::from_millis(100))
            .map(|(path, _)| path.clone())
            .collect();
        for path in expired {
            let (event, _) = self.pending_renames.remove(&path).unwrap();
            self.retain(event);
        }
        if std::mem::take(&mut self.overflow) {
            for root in roots {
                self.folder(root.clone(), true);
            }
        }
        self.modified.clear();
        self.stats.last_batch_folders = self.folders.len();
        Changes {
            events: std::mem::take(&mut self.events),
            folders: std::mem::take(&mut self.folders).into_iter().collect(),
            stats: self.stats.clone(),
        }
    }
}
pub struct LiveWatch {
    _watchers: Vec<windows_watch::NativeWatch>,
    queue: Arc<Mutex<Queue>>,
    roots: Vec<PathBuf>,
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
        let mut errors = vec![];
        let mut watchers = vec![];
        for root in roots {
            let queued = queue.clone();
            let exclude = exclude.clone();
            let scope = root.clone();
            let receive: Arc<dyn Fn(Vec<Event>) + Send + Sync> = Arc::new(move |events| {
                let mut q = queued.lock().unwrap_or_else(|e| e.into_inner());
                for event in events {
                    if matches!(event.kind, EventKind::Access(_))
                        || (!event.paths.is_empty()
                            && event.paths.iter().all(|p| is_within(p, &exclude)))
                    {
                        continue;
                    }
                    q.push_in_scope(scope.clone(), event);
                }
            });
            match windows_watch::NativeWatch::new(root.clone(), receive) {
                Ok(watcher) => watchers.push(watcher),
                Err(error) => errors.push(format!("Watch {}: {error}", root.display())),
            }
        }
        Ok(Self {
            _watchers: watchers,
            queue,
            roots: roots.to_vec(),
            errors,
        })
    }
    pub fn take(&self) -> Changes {
        self.queue
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .drain(&self.roots)
    }
}
fn is_within(path: &Path, root: &Path) -> bool {
    let clean = |p: &Path| {
        crate::presentation::display_path(&p.to_string_lossy())
            .replace('/', "\\")
            .to_lowercase()
    };
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
    children: HashMap<u64, HashMap<Vec<u16>, u64>>,
    removed: HashSet<u64>,
}
impl Edit<'_> {
    fn child(&self, parent: u64, name: &[u16]) -> Option<u64> {
        if let Some(id) = self
            .children
            .get(&parent)
            .and_then(|children| children.get(name))
        {
            return Some(*id);
        }
        self.search
            .child(parent, name)
            .filter(|e| match self.records.get(&e.id) {
                Some(Some(record)) => record.parent == parent && record.name == name,
                Some(None) => false,
                None => true,
            })
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
        self.erase(id);
    }
    fn erase(&mut self, id: u64) {
        let directory = self.record(id).is_some_and(|r| r.flags & DIRECTORY != 0);
        if let Some(Some(old)) = self.records.get(&id)
            && let Some(children) = self.children.get_mut(&old.parent)
        {
            children.remove(&old.name);
        }
        self.records.insert(id, None);
        self.pending.remove(&id);
        if directory {
            self.removed.insert(id);
        }
    }
    fn finish_removals(&mut self) {
        if self.removed.is_empty() {
            return;
        }
        let mut removed = std::mem::take(&mut self.removed);
        loop {
            let mut children = Vec::new();
            for entry in self.index.live_entries() {
                if self.records.get(&entry.id).is_some_and(Option::is_none) {
                    continue;
                }
                let parent = self
                    .records
                    .get(&entry.id)
                    .and_then(Option::as_ref)
                    .map_or(entry.parent, |r| r.parent);
                if removed.contains(&parent) {
                    removed.insert(entry.id);
                    children.push(entry.id);
                }
            }
            for (&id, record) in &self.records {
                if let Some(record) = record
                    && removed.contains(&record.parent)
                {
                    removed.insert(id);
                    children.push(id);
                }
            }
            if children.is_empty() {
                break;
            }
            for id in children {
                self.erase(id);
            }
            self.removed.clear();
        }
    }
    fn reconcile(&mut self, folders: &[(PathBuf, bool)], exclude: &Path) -> Result<()> {
        let mut todo: std::collections::VecDeque<_> = folders.iter().cloned().collect();
        let mut scanned = HashMap::new();
        let mut present = HashSet::new();
        while let Some((path, recursive)) = todo.pop_front() {
            if is_within(&path, exclude) || !is_within(&path, Path::new(&self.index.metadata.root))
            {
                continue;
            }
            self.upsert(&path, false, None)?;
            let Some(id) = self.resolve(&path) else {
                continue;
            };
            if scanned.get(&id).is_some_and(|done| *done || !recursive) {
                continue;
            }
            if id != self.index.metadata.root_id
                && !self
                    .record(id)
                    .is_some_and(|r| r.flags & DIRECTORY != 0 && r.flags & REPARSE == 0)
            {
                continue;
            }
            let entries = match std::fs::read_dir(&path) {
                Ok(entries) => entries,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    self.remove(&path);
                    continue;
                }
                Err(_) => {
                    self.pending.insert(id);
                    continue;
                }
            };
            let mut complete = true;
            for item in entries {
                let entry = match item {
                    Ok(e) => e,
                    Err(_) => {
                        complete = false;
                        continue;
                    }
                };
                let child_path = entry.path();
                if is_within(&child_path, exclude) {
                    continue;
                }
                let old = self.resolve(&child_path);
                let retry = old.is_some_and(|id| self.pending.contains(&id));
                self.upsert(&child_path, false, None)?;
                if let Some(child) = self.resolve(&child_path) {
                    present.insert(child);
                    if (recursive || old.is_none() || retry)
                        && self
                            .record(child)
                            .is_some_and(|r| r.flags & DIRECTORY != 0 && r.flags & REPARSE == 0)
                    {
                        todo.push_back((child_path, true));
                    }
                }
            }
            if complete {
                scanned.insert(id, recursive);
                self.pending.remove(&id);
            } else {
                self.pending.insert(id);
            }
        }
        // One metadata pass handles all reconciled parents. Never materialize every path
        // or repeatedly traverse the complete index for each absent child.
        if scanned.is_empty() {
            return Ok(());
        }
        let mut stale: Vec<_> = self
            .index
            .live_entries()
            .filter(|e| {
                let parent = self
                    .records
                    .get(&e.id)
                    .and_then(Option::as_ref)
                    .map_or(e.parent, |r| r.parent);
                scanned.contains_key(&parent)
                    && !present.contains(&e.id)
                    && !self.records.get(&e.id).is_some_and(Option::is_none)
            })
            .map(|e| e.id)
            .collect();
        stale.extend(self.records.iter().filter_map(|(&id, record)| {
            record
                .as_ref()
                .filter(|r| scanned.contains_key(&r.parent) && !present.contains(&id))
                .map(|_| id)
        }));
        for id in stale {
            self.erase(id);
        }
        Ok(())
    }
    fn upsert(&mut self, path: &Path, recursive: bool, preferred: Option<u64>) -> Result<()> {
        self.upsert_observed(path, recursive, preferred, true)
    }
    fn refresh_metadata(&mut self, path: &Path) -> Result<()> {
        self.upsert_observed(path, false, None, false)
    }
    fn upsert_observed(
        &mut self,
        path: &Path,
        recursive: bool,
        preferred: Option<u64>,
        remove_missing: bool,
    ) -> Result<()> {
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
                if remove_missing {
                    self.remove(path);
                } else if let Some(id) = old {
                    // A queued write can precede a rename/delete already visible on disk.
                    // Its structural notification (or loss recovery) owns identity changes.
                    self.pending.insert(id);
                }
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
            if self.record(id).is_some_and(|r| r.flags & DIRECTORY != 0)
                && (flags & DIRECTORY == 0 || flags & REPARSE != 0)
            {
                self.removed.insert(id);
            }
            if let Some(old) = self.record(id)
                && let Some(children) = self.children.get_mut(&old.parent)
            {
                children.remove(&old.name);
            }
            self.children
                .entry(parent)
                .or_default()
                .insert(record.name.clone(), id);
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
    changes(index, search, events, &[], sizes, exclude)
}
pub fn changes(
    index: &Index,
    search: &SearchIndex<'_>,
    events: &[Event],
    folders: &[(PathBuf, bool)],
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
        children: HashMap::new(),
        removed: HashSet::new(),
    };
    let mut recovery = Queue::default();
    for (path, recursive) in folders {
        recovery.folder(path.clone(), *recursive);
    }
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
                edit.upsert(new, false, id)?;
                if id.is_none() {
                    recovery.folder(new.clone(), true);
                }
            } else {
                edit.remove(old);
            }
            for path in &event.paths {
                recovery.folder(path.parent().unwrap_or(path).to_owned(), false);
            }
            continue;
        }
        for path in &event.paths {
            if is_within(path, exclude) || !is_within(path, Path::new(&index.metadata.root)) {
                continue;
            }
            match event.kind {
                EventKind::Modify(ModifyKind::Name(RenameMode::From)) if paired.contains(path) => {}
                EventKind::Create(_)
                | EventKind::Remove(_)
                | EventKind::Modify(ModifyKind::Name(_)) => {
                    recovery.folder(path.parent().unwrap_or(path).to_owned(), false);
                }
                _ => {
                    let id = edit.resolve(path);
                    if id.is_none() {
                        recovery.folder(path.parent().unwrap_or(path).to_owned(), false);
                    } else {
                        let retry = id.is_some_and(|id| edit.pending.contains(&id));
                        edit.refresh_metadata(path)?;
                        if let Some(id) = id
                            && retry
                            && !edit.pending.contains(&id)
                            && edit
                                .record(id)
                                .is_some_and(|r| r.flags & (DIRECTORY | REPARSE) == DIRECTORY)
                        {
                            recovery.folder(path.clone(), true);
                        }
                    }
                }
            }
        }
    }
    let folders: Vec<_> = recovery.folders.into_iter().collect();
    if !folders.is_empty() {
        edit.reconcile(&folders, exclude)?;
    }
    edit.finish_removals();
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
    #[test]
    fn structural_burst_is_recoverable_without_discarding_the_loaded_index() {
        let mut queue = Queue::default();
        for number in 0..10_000 {
            queue.push(event(
                EventKind::Create(notify::event::CreateKind::Any),
                vec![PathBuf::from(format!(r"C:\fixture\new-{number}.txt"))],
            ));
        }
        assert!(
            !queue.overflow,
            "application queue pressure must preserve folder recovery work"
        );
        assert!(queue.events.len() <= 4096, "recovery must keep RAM bounded");
    }
    #[test]
    fn folder_recovery_reconciles_a_structural_burst_and_real_loss_without_rewriting_base() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("source");
        let storage = dir.path().join("data");
        std::fs::create_dir(&root).unwrap();
        std::fs::create_dir(&storage).unwrap();
        std::fs::create_dir_all(root.join("removed/deep")).unwrap();
        std::fs::write(root.join("removed/deep/old.txt"), b"old").unwrap();
        std::fs::write(root.join("kept.txt"), b"zero").unwrap();
        let (mut index, _) = rustsearch_windows::walk(&root, true).unwrap();
        let output = storage.join("source.rsidx");
        index.snapshot_identity = Some(rustsearch_storage::save(&output, &index).unwrap());
        drop(SearchIndex::prepare(&index, &output, true).0);
        let base = std::fs::read(&output).unwrap();
        let cache = std::fs::read(rustsearch_query::cache_path(&output)).unwrap();
        std::fs::remove_dir_all(root.join("removed")).unwrap();
        let mut queue = Queue::default();
        queue.push(event(
            EventKind::Remove(notify::event::RemoveKind::Folder),
            vec![root.join("removed")],
        ));
        for number in 0..4200 {
            let file = root.join(format!("new-{number}.txt"));
            std::fs::write(&file, b"").unwrap();
            queue.push(event(
                EventKind::Create(notify::event::CreateKind::File),
                vec![file],
            ));
        }
        let batch = queue.drain(std::slice::from_ref(&root));
        assert!(!batch.folders.is_empty());
        assert_eq!(batch.stats.queue_recoveries, 1);
        let search = SearchIndex::prepare(&index, &output, true).0;
        let delta = changes(
            &index,
            &search,
            &batch.events,
            &batch.folders,
            true,
            &storage,
        )
        .unwrap();
        drop(search);
        let guard = rustsearch_storage::WriteGuard::acquire(&output).unwrap();
        rustsearch_storage::append_delta(&guard, &index, &delta).unwrap();
        index.apply_delta(delta).unwrap();
        drop(guard);
        assert_eq!(index.counts().0, 4201);
        assert_eq!(
            rustsearch_query::Query {
                text: "old.txt".into(),
                limit: 10,
                ..Default::default()
            }
            .execute(&index)
            .total,
            0
        );
        std::fs::write(root.join("kept.txt"), b"resized").unwrap();
        std::fs::create_dir(root.join("new-directory")).unwrap();
        std::fs::write(root.join("new-directory/after-loss.txt"), b"loss").unwrap();
        queue.push(Event::new(EventKind::Other).set_flag(notify::event::Flag::Rescan));
        let batch = queue.drain(std::slice::from_ref(&root));
        assert_eq!(batch.stats.backend_losses, 1);
        let search = SearchIndex::prepare(&index, &output, true).0;
        let delta = changes(
            &index,
            &search,
            &batch.events,
            &batch.folders,
            true,
            &storage,
        )
        .unwrap();
        drop(search);
        let guard = rustsearch_storage::WriteGuard::acquire(&output).unwrap();
        rustsearch_storage::append_delta(&guard, &index, &delta).unwrap();
        index.apply_delta(delta).unwrap();
        drop(guard);
        assert_eq!(index.counts().0, 4203);
        let query = rustsearch_query::Query {
            text: "after-loss.txt".into(),
            limit: 10,
            ..Default::default()
        };
        assert_eq!(
            query
                .execute(&rustsearch_storage::load(&output).unwrap())
                .total,
            1
        );
        assert_eq!(std::fs::read(&output).unwrap(), base);
        assert_eq!(
            std::fs::read(rustsearch_query::cache_path(&output)).unwrap(),
            cache
        );
    }
    #[test]
    fn folder_pressure_broadens_coverage_and_unpaired_renames_never_cross_drives() {
        let mut queue = Queue::default();
        for number in 0..12_000 {
            queue.push(event(
                EventKind::Create(notify::event::CreateKind::File),
                vec![PathBuf::from(format!(
                    r"C:\fixture\folder-{number}\file.txt"
                ))],
            ));
        }
        let batch = queue.drain(&[PathBuf::from(r"C:\")]);
        assert!(batch.folders.len() <= MAX_FOLDERS);
        assert!(
            batch
                .folders
                .iter()
                .any(|(p, recursive)| p == Path::new(r"C:\fixture") && *recursive)
        );
        let mut queue = Queue::default();
        queue.push(event(
            EventKind::Modify(ModifyKind::Name(RenameMode::From)),
            vec![PathBuf::from(r"C:\old.txt")],
        ));
        queue.push(event(
            EventKind::Modify(ModifyKind::Name(RenameMode::To)),
            vec![PathBuf::from(r"D:\new.txt")],
        ));
        for (_, at) in queue.pending_renames.values_mut() {
            *at = std::time::Instant::now() - std::time::Duration::from_secs(1);
        }
        let batch = queue.drain(&[]);
        assert_eq!(batch.events.len(), 2);
        assert!(!batch.events.iter().any(|e| matches!(
            e.kind,
            EventKind::Modify(ModifyKind::Name(RenameMode::Both))
        )));
    }
    #[test]
    fn folder_recovery_discards_a_staged_file_that_vanished_before_enumeration() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("source");
        std::fs::create_dir(&root).unwrap();
        let (index, _) = rustsearch_windows::walk(&root, true).unwrap();
        let search = SearchIndex::build(&index);
        let mut edit = Edit {
            index: &index,
            search: &search,
            records: BTreeMap::new(),
            pending: HashSet::new(),
            next: 2,
            sizes: true,
            children: HashMap::new(),
            removed: HashSet::new(),
        };
        let file = root.join("temporary.txt");
        std::fs::write(&file, b"temporary").unwrap();
        edit.upsert(&file, false, None).unwrap();
        let id = edit.resolve(&file).unwrap();
        std::fs::remove_file(file).unwrap();
        edit.reconcile(&[(root, false)], &dir.path().join("excluded"))
            .unwrap();
        assert!(edit.records.get(&id).is_some_and(Option::is_none));
    }
    #[test]
    fn rename_pairing_is_scoped_to_each_reader_even_on_the_same_drive() {
        let mut queue = Queue::default();
        let a = PathBuf::from(r"C:\first");
        let b = PathBuf::from(r"C:\second");
        for root in [&a, &b] {
            queue.push_in_scope(
                root.clone(),
                event(
                    EventKind::Modify(ModifyKind::Name(RenameMode::From)),
                    vec![root.join("old.txt")],
                ),
            );
        }
        for root in [&a, &b] {
            queue.push_in_scope(
                root.clone(),
                event(
                    EventKind::Modify(ModifyKind::Name(RenameMode::To)),
                    vec![root.join("new.txt")],
                ),
            );
        }
        let batch = queue.drain(&[]);
        assert_eq!(batch.events.len(), 2);
        for event in batch.events {
            assert_eq!(event.paths[0].parent(), event.paths[1].parent());
            assert!(matches!(
                event.kind,
                EventKind::Modify(ModifyKind::Name(RenameMode::Both))
            ));
        }
    }
    #[test]
    fn renamed_cached_child_does_not_shadow_a_new_file_at_its_old_name() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("old.txt"), b"original").unwrap();
        let (index, _) = rustsearch_windows::walk(dir.path(), true).unwrap();
        let search = SearchIndex::build(&index);
        let mut edit = Edit {
            index: &index,
            search: &search,
            records: BTreeMap::new(),
            pending: HashSet::new(),
            next: 3,
            sizes: true,
            children: HashMap::new(),
            removed: HashSet::new(),
        };
        let old = dir.path().join("old.txt");
        let new = dir.path().join("new.txt");
        let original = edit.resolve(&old).unwrap();
        std::fs::rename(&old, &new).unwrap();
        edit.upsert(&new, false, Some(original)).unwrap();
        assert_eq!(edit.resolve(&old), None);
        std::fs::write(&old, b"replacement").unwrap();
        edit.upsert(&old, false, None).unwrap();
        assert_ne!(edit.resolve(&old), Some(original));
        assert_eq!(edit.resolve(&new), Some(original));
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
    fn directory_metadata_retries_pending_contents() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source");
        let folder = source.join("folder");
        std::fs::create_dir_all(&folder).unwrap();
        let (mut index, _) = rustsearch_windows::walk(&source, false).unwrap();
        let search = SearchIndex::build(&index);
        let id = search
            .child(
                index.metadata.root_id,
                &"folder".encode_utf16().collect::<Vec<_>>(),
            )
            .unwrap()
            .id;
        drop(search);
        index.metadata.pending_refresh.push(id);
        std::fs::create_dir(folder.join("new")).unwrap();
        std::fs::write(folder.join("new/child.txt"), b"new").unwrap();
        let search = SearchIndex::build(&index);
        let update = delta(
            &index,
            &search,
            &[event(EventKind::Modify(ModifyKind::Any), vec![folder])],
            false,
            &dir.path().join("excluded"),
        )
        .unwrap();
        drop(search);
        index.apply_delta(update).unwrap();
        assert!(index.metadata.pending_refresh.is_empty());
        assert_eq!(index.counts().0, 3);
        let query = rustsearch_query::Query {
            text: "child.txt".into(),
            ..Default::default()
        };
        assert_eq!(query.execute(&index).total, 1);
    }

    #[test]
    fn directory_metadata_waits_for_the_child_rename_notification() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source");
        let parent = source.join("folder");
        std::fs::create_dir_all(&parent).unwrap();
        let old = parent.join("old.txt");
        let new = parent.join("new.txt");
        std::fs::write(&old, b"original").unwrap();
        let (mut index, _) = rustsearch_windows::walk(&source, false).unwrap();
        let search = SearchIndex::build(&index);
        let folder = search
            .child(
                index.metadata.root_id,
                &"folder".encode_utf16().collect::<Vec<_>>(),
            )
            .unwrap()
            .id;
        let id = search
            .child(folder, &"old.txt".encode_utf16().collect::<Vec<_>>())
            .unwrap()
            .id;
        std::fs::rename(&old, &new).unwrap();
        let update = delta(
            &index,
            &search,
            &[event(EventKind::Modify(ModifyKind::Any), vec![parent])],
            false,
            &dir.path().join("excluded"),
        )
        .unwrap();
        drop(search);
        index.apply_delta(update).unwrap();
        assert!(
            index.get(id).is_some(),
            "directory metadata must not consume a later child's rename as delete/create"
        );
        let search = SearchIndex::build(&index);
        let update = delta(
            &index,
            &search,
            &[event(
                EventKind::Modify(ModifyKind::Name(RenameMode::Both)),
                vec![old, new.clone()],
            )],
            false,
            &dir.path().join("excluded"),
        )
        .unwrap();
        drop(search);
        index.apply_delta(update).unwrap();
        assert_eq!(
            index.path_utf16(index.get(id).unwrap()).unwrap(),
            new.canonicalize()
                .unwrap()
                .as_os_str()
                .encode_wide()
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn queued_metadata_does_not_delete_a_file_before_its_rename_notification() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source");
        std::fs::create_dir(&source).unwrap();
        let old = source.join("old.txt");
        let new = source.join("new.txt");
        std::fs::write(&old, b"original").unwrap();
        let (mut index, _) = rustsearch_windows::walk(&source, false).unwrap();
        let search = SearchIndex::build(&index);
        let id = search
            .child(
                index.metadata.root_id,
                &"old.txt".encode_utf16().collect::<Vec<_>>(),
            )
            .unwrap()
            .id;
        std::fs::rename(&old, &new).unwrap();
        let update = delta(
            &index,
            &search,
            &[event(EventKind::Modify(ModifyKind::Any), vec![old.clone()])],
            false,
            &dir.path().join("excluded"),
        )
        .unwrap();
        drop(search);
        index.apply_delta(update).unwrap();
        assert!(
            index.get(id).is_some(),
            "a metadata notification must wait for the queued structural event before deleting identity"
        );
        let search = SearchIndex::build(&index);
        let update = delta(
            &index,
            &search,
            &[event(
                EventKind::Modify(ModifyKind::Name(RenameMode::Both)),
                vec![old, new.clone()],
            )],
            false,
            &dir.path().join("excluded"),
        )
        .unwrap();
        drop(search);
        index.apply_delta(update).unwrap();
        assert_eq!(
            index.path_utf16(index.get(id).unwrap()).unwrap(),
            new.canonicalize()
                .unwrap()
                .as_os_str()
                .encode_wide()
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn queued_metadata_before_a_paired_rename_preserves_identity() {
        for stale_metadata in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let source = dir.path().join("source");
            std::fs::create_dir(&source).unwrap();
            let old = source.join("old.txt");
            let new = source.join("new.txt");
            std::fs::write(&old, b"original").unwrap();
            let (mut index, _) = rustsearch_windows::walk(&source, false).unwrap();
            let search = SearchIndex::build(&index);
            let id = search
                .child(
                    index.metadata.root_id,
                    &"old.txt".encode_utf16().collect::<Vec<_>>(),
                )
                .unwrap()
                .id;
            std::fs::rename(&old, &new).unwrap();
            let mut events = vec![];
            if stale_metadata {
                events.push(event(EventKind::Modify(ModifyKind::Any), vec![old.clone()]));
            }
            events.push(event(
                EventKind::Modify(ModifyKind::Name(RenameMode::Both)),
                vec![old, new.clone()],
            ));
            let delta = delta(
                &index,
                &search,
                &events,
                false,
                &dir.path().join("excluded"),
            )
            .unwrap();
            drop(search);
            index.apply_delta(delta).unwrap();
            let record = index
                .get(id)
                .expect("stale old-path metadata must not erase a paired rename's identity");
            assert_eq!(
                index.path_utf16(record).unwrap(),
                new.canonicalize()
                    .unwrap()
                    .as_os_str()
                    .encode_wide()
                    .collect::<Vec<_>>()
            );
            assert_eq!(index.counts().0, 1);
        }
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
        let mut queue = Queue::default();
        queue.push(event(
            EventKind::Modify(ModifyKind::Name(RenameMode::From)),
            vec![source.join("Before")],
        ));
        queue.push(event(
            EventKind::Modify(ModifyKind::Name(RenameMode::To)),
            vec![source.join("İstanbul-Ω")],
        ));
        commit(&mut index, &queue.events, &path, &output);
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
