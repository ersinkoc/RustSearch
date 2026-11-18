use anyhow::{Context, Result, ensure};
use notify::{
    Event, EventKind,
    event::{ModifyKind, RenameMode},
};
use rustsearch_core::{DIRECTORY, Delta, Index, REPARSE, Record, SIZE_KNOWN};
use rustsearch_query::SearchIndex;
use std::os::windows::{
    ffi::{OsStrExt, OsStringExt},
    fs::MetadataExt,
};
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
    parent_runs: Option<Vec<ParentRun>>,
}
struct ParentRun {
    parent: u64,
    first: u32,
    end: u32,
}
#[derive(Clone, Copy)]
struct Job {
    recursive: bool,
    hot: bool,
}
#[derive(Default)]
pub struct Recovery {
    folders: Vec<(PathBuf, bool)>,
    hot: std::collections::VecDeque<u64>,
    cold: std::collections::VecDeque<u64>,
    queued: HashMap<u64, Job>,
    completed: HashMap<u64, bool>,
    parent_runs: Option<Vec<ParentRun>>,
    base: Option<(Option<[u8; 24]>, usize, usize)>,
    checked_directories: u64,
}
impl Recovery {
    pub fn schedule(&mut self, path: PathBuf, recursive: bool) {
        self.folders.push((path, recursive));
    }
    pub fn is_empty(&self) -> bool {
        self.folders.is_empty() && self.queued.is_empty()
    }
    /// Aggregate recovery diagnostics; no changed-file paths are retained.
    pub fn progress(&self) -> (u64, usize) {
        (
            self.checked_directories,
            self.queued.len() + self.folders.len(),
        )
    }
    fn enqueue(&mut self, id: u64, job: Job, external: bool) {
        if external {
            if job.recursive {
                self.completed.clear();
            } else {
                self.completed.remove(&id);
            }
        } else if self
            .completed
            .get(&id)
            .is_some_and(|done| *done || !job.recursive)
        {
            return;
        }
        if let Some(old) = self.queued.get_mut(&id) {
            old.recursive |= job.recursive;
            if job.hot {
                let was_hot = old.hot;
                old.hot = true;
                if external {
                    self.hot.push_front(id);
                } else if !was_hot {
                    self.hot.push_back(id);
                }
            }
        } else {
            self.queued.insert(id, job);
            if job.hot {
                if external {
                    self.hot.push_front(id);
                } else {
                    self.hot.push_back(id);
                }
            } else {
                self.cold.push_back(id);
            }
        }
    }
    fn pop(&mut self) -> Option<(u64, Job)> {
        loop {
            let (id, hot) = if let Some(id) = self.hot.pop_front() {
                (id, true)
            } else {
                (self.cold.pop_front()?, false)
            };
            let Some(job) = self.queued.get(&id).copied() else {
                continue;
            };
            if job.hot && !hot {
                continue;
            }
            self.queued.remove(&id);
            return Some((id, job));
        }
    }
    pub fn step(
        &mut self,
        index: &Index,
        search: &SearchIndex<'_>,
        events: &[Event],
        folders: &[(PathBuf, bool)],
        sizes: bool,
        exclude: &Path,
    ) -> Result<Delta> {
        self.step_with_limit(index, search, events, folders, sizes, exclude, 32)
    }
    #[allow(clippy::too_many_arguments)]
    fn step_with_limit(
        &mut self,
        index: &Index,
        search: &SearchIndex<'_>,
        events: &[Event],
        folders: &[(PathBuf, bool)],
        sizes: bool,
        exclude: &Path,
        limit: usize,
    ) -> Result<Delta> {
        let base = (
            index.snapshot_identity,
            index.entries.as_ptr() as usize,
            index.entries.len(),
        );
        if self.base != Some(base) {
            self.hot.clear();
            self.cold.clear();
            self.queued.clear();
            self.completed.clear();
            self.parent_runs = None;
            self.base = Some(base);
            self.checked_directories = 0;
        }
        let delta = changes_inner(
            index,
            search,
            events,
            folders,
            sizes,
            exclude,
            Some((&mut *self, limit)),
        )?;
        if self.is_empty() {
            self.parent_runs = None;
            self.completed = HashMap::new();
            self.hot.clear();
            self.cold.clear();
        }
        Ok(delta)
    }
}
fn parent_runs(index: &Index) -> Option<Vec<ParentRun>> {
    // Accessible snapshots enumerate each parent's children together. Keep only row
    // boundaries, capped at 64 MiB and dropped after this recovery transaction.
    let rows = &index.entries;
    if rows.is_empty() {
        return Some(vec![]);
    }
    let count = 1 + rows
        .windows(2)
        .filter(|pair| pair[0].parent != pair[1].parent)
        .count();
    if rows.len() >= u32::MAX as usize
        || count > 64 * 1024 * 1024 / std::mem::size_of::<ParentRun>()
    {
        return None;
    }
    let mut runs = Vec::with_capacity(count);
    let mut first = 0;
    for end in 1..=rows.len() {
        if end == rows.len() || rows[first].parent != rows[end].parent {
            runs.push(ParentRun {
                parent: rows[first].parent,
                first: first as u32,
                end: end as u32,
            });
            first = end;
        }
    }
    runs.sort_unstable_by_key(|run| (run.parent, run.first));
    Some(runs)
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
        let entry = if let Some(runs) = &self.parent_runs {
            let first = runs.partition_point(|run| run.parent < parent);
            let end = runs.partition_point(|run| run.parent <= parent);
            if end - first > 64 {
                // Fragmented legacy layouts can have more runs than name candidates.
                self.search
                    .child_in_ranges(parent, name, std::iter::once((0, u32::MAX)))
            } else {
                self.search.child_in_ranges(
                    parent,
                    name,
                    runs[first..end].iter().map(|run| (run.first, run.end)),
                )
            }
        } else {
            self.search
                .child_in_ranges(parent, name, std::iter::once((0, u32::MAX)))
        };
        entry
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
        let mut work = Recovery::default();
        work.folders.extend_from_slice(folders);
        self.reconcile_chunk(&mut work, exclude, usize::MAX, std::time::Duration::MAX)
    }
    fn reconcile_chunk(
        &mut self,
        work: &mut Recovery,
        exclude: &Path,
        limit: usize,
        time: std::time::Duration,
    ) -> Result<()> {
        let folders = std::mem::take(&mut work.folders);
        let relevant: Vec<_> = folders
            .iter()
            .filter(|(path, _)| {
                is_within(path, Path::new(&self.index.metadata.root)) && !is_within(path, exclude)
            })
            .collect();
        if self.parent_runs.is_none()
            && (relevant.len() >= 64 || relevant.iter().any(|(_, r)| *r) || !work.queued.is_empty())
        {
            self.parent_runs = parent_runs(self.index);
        }
        for (path, recursive) in relevant {
            self.upsert(path, false, None)?;
            if let Some(id) = self.resolve(path) {
                work.enqueue(
                    id,
                    Job {
                        recursive: *recursive,
                        hot: !*recursive
                            || (id != self.index.metadata.root_id && self.index.get(id).is_none()),
                    },
                    true,
                );
            }
        }
        let started = std::time::Instant::now();
        let mut scanned = HashMap::new();
        let mut present = HashSet::new();
        let mut visited = 0;
        while visited < limit && (visited == 0 || started.elapsed() < time) {
            let Some((id, job)) = work.pop() else { break };
            visited += 1;
            work.checked_directories = work.checked_directories.saturating_add(1);
            if id != self.index.metadata.root_id && self.record(id).is_none() {
                continue;
            }
            let Some(path) = self.path(id)? else {
                continue;
            };
            if is_within(&path, exclude) {
                continue;
            }
            // Recheck directory attributes when dequeued; cached child metadata must not
            // permit following a directory that became a reparse point between batches.
            let metadata = match std::fs::symlink_metadata(&path) {
                Ok(metadata) => metadata,
                Err(error) => {
                    if id != self.index.metadata.root_id
                        && error.kind() == std::io::ErrorKind::NotFound
                    {
                        self.erase(id);
                    } else {
                        self.pending.insert(id);
                    }
                    work.completed.insert(id, job.recursive);
                    continue;
                }
            };
            if id == self.index.metadata.root_id {
                if metadata.file_attributes() & 0x10 == 0 || metadata.file_attributes() & 0x400 != 0
                {
                    self.pending.insert(id);
                    continue;
                }
            } else {
                let old = self.record(id).context("missing queued directory")?;
                self.put_metadata(old.parent, old.name, &metadata, Some(id))?;
            }
            if id != self.index.metadata.root_id
                && self
                    .record(id)
                    .is_none_or(|r| r.flags & (DIRECTORY | REPARSE) != DIRECTORY)
            {
                continue;
            }
            let entries = match std::fs::read_dir(&path) {
                Ok(entries) => entries,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    if id != self.index.metadata.root_id {
                        self.erase(id);
                    } else {
                        self.pending.insert(id);
                    }
                    continue;
                }
                Err(_) => {
                    self.pending.insert(id);
                    work.completed.insert(id, job.recursive);
                    continue;
                }
            };
            let mut complete = true;
            for item in entries {
                let entry = match item {
                    Ok(entry) => entry,
                    Err(_) => {
                        complete = false;
                        continue;
                    }
                };
                let child_path = entry.path();
                if is_within(&child_path, exclude) {
                    continue;
                }
                let name: Vec<_> = entry.file_name().encode_wide().collect();
                let old = self.child(id, &name);
                let retry = old.is_some_and(|id| self.pending.contains(&id));
                let metadata = match entry.metadata() {
                    Ok(metadata) => metadata,
                    Err(_) => {
                        complete = false;
                        if let Some(old) = old {
                            present.insert(old);
                            self.pending.insert(old);
                        }
                        continue;
                    }
                };
                let child = self.put_metadata(id, name, &metadata, old)?;
                present.insert(child);
                if (job.recursive || old.is_none() || retry)
                    && self
                        .record(child)
                        .is_some_and(|r| r.flags & (DIRECTORY | REPARSE) == DIRECTORY)
                {
                    work.enqueue(
                        child,
                        Job {
                            recursive: true,
                            hot: job.hot && old.is_none(),
                        },
                        false,
                    );
                }
            }
            work.completed.insert(id, job.recursive);
            if complete {
                scanned.insert(id, job.recursive);
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
        let stale_entry = |e: &rustsearch_core::Entry| {
            let parent = self
                .records
                .get(&e.id)
                .and_then(Option::as_ref)
                .map_or(e.parent, |r| r.parent);
            scanned.contains_key(&parent)
                && !present.contains(&e.id)
                && !self.records.get(&e.id).is_some_and(Option::is_none)
        };
        let mut stale = Vec::new();
        if let Some(runs) = &self.parent_runs {
            for parent in scanned.keys() {
                let from = runs.partition_point(|run| run.parent < *parent);
                let to = runs.partition_point(|run| run.parent <= *parent);
                for run in &runs[from..to] {
                    for e in &self.index.entries[run.first as usize..run.end as usize] {
                        if !self.index.updates.contains_key(&e.id) && stale_entry(e) {
                            stale.push(e.id);
                        }
                    }
                }
            }
            for parent in scanned.keys() {
                stale.extend(
                    self.search
                        .overlay_children(*parent)
                        .iter()
                        .filter(|e| stale_entry(e))
                        .map(|e| e.id),
                );
            }
        } else {
            stale.extend(
                self.index
                    .live_entries()
                    .filter(|e| stale_entry(e))
                    .map(|e| e.id),
            );
        }
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
        self.put_metadata(parent, name, &metadata, old)?;
        if recursive && metadata.is_dir() && metadata.file_attributes() & 0x400 == 0 {
            for entry in std::fs::read_dir(path)? {
                self.upsert(&entry?.path(), true, None)?;
            }
        }
        Ok(())
    }
    fn put_metadata(
        &mut self,
        parent: u64,
        name: Vec<u16>,
        metadata: &std::fs::Metadata,
        old: Option<u64>,
    ) -> Result<u64> {
        let reparse = metadata.file_attributes() & 0x400 != 0;
        let directory = metadata.file_attributes() & 0x10 != 0;
        let flags = if directory { DIRECTORY } else { 0 } | if reparse { REPARSE } else { 0 };
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
            size: (self.sizes && !directory && !reparse).then_some(metadata.file_size()),
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
        Ok(id)
    }
    fn path(&self, id: u64) -> Result<Option<PathBuf>> {
        let mut current = id;
        let mut names = Vec::new();
        let mut seen = HashSet::new();
        while current != self.index.metadata.root_id {
            ensure!(seen.insert(current), "cycle in recovery path");
            if self.records.get(&current).is_some_and(Option::is_none) {
                return Ok(None);
            }
            let record = self.record(current).context("missing recovery parent")?;
            if current != id && record.flags & (DIRECTORY | REPARSE) != DIRECTORY {
                return Ok(None);
            }
            names.push(record.name);
            current = record.parent;
        }
        let mut path = PathBuf::from(&self.index.metadata.root);
        for name in names.into_iter().rev() {
            path.push(std::ffi::OsString::from_wide(&name));
        }
        Ok(Some(path))
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
    changes_inner(index, search, events, folders, sizes, exclude, None)
}
#[allow(clippy::too_many_arguments)]
fn changes_inner(
    index: &Index,
    search: &SearchIndex<'_>,
    events: &[Event],
    folders: &[(PathBuf, bool)],
    sizes: bool,
    exclude: &Path,
    mut work: Option<(&mut Recovery, usize)>,
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
        parent_runs: work.as_mut().and_then(|(work, _)| work.parent_runs.take()),
    };
    if edit.parent_runs.is_none()
        && (work.as_ref().is_some_and(|(work, _)| !work.is_empty())
            || folders.len() >= 64
            || folders.iter().any(|(_, recursive)| *recursive))
    {
        edit.parent_runs = parent_runs(index);
    }
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
    if let Some((work, limit)) = work {
        work.folders.extend(folders);
        edit.reconcile_chunk(work, exclude, limit, std::time::Duration::from_millis(25))?;
        work.parent_runs = edit.parent_runs.take();
    } else if !folders.is_empty() {
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
            parent_runs: None,
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
            parent_runs: None,
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
    fn directory_reparse_metadata_keeps_the_snapshot_directory_classification() {
        use std::os::windows::process::CommandExt;
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source");
        let target = dir.path().join("target");
        std::fs::create_dir(&source).unwrap();
        std::fs::create_dir(&target).unwrap();
        let link = source.join("junction");
        let output = std::process::Command::new("cmd.exe")
            .args(["/C", "mklink", "/J"])
            .arg(&link)
            .arg(&target)
            .creation_flags(0x08000000)
            .output()
            .unwrap();
        assert!(output.status.success());
        let (mut index, _) = rustsearch_windows::walk(&source, false).unwrap();
        let search = SearchIndex::build(&index);
        let id = search
            .child(
                index.metadata.root_id,
                &"junction".encode_utf16().collect::<Vec<_>>(),
            )
            .unwrap()
            .id;
        assert_eq!(
            index.flags(index.get(id).unwrap()) & (DIRECTORY | REPARSE),
            DIRECTORY | REPARSE
        );
        let delta = delta(
            &index,
            &search,
            &[event(EventKind::Modify(ModifyKind::Any), vec![link])],
            false,
            &dir.path().join("excluded"),
        )
        .unwrap();
        drop(search);
        index.apply_delta(delta).unwrap();
        assert_eq!(
            index.flags(index.get(id).unwrap()) & (DIRECTORY | REPARSE),
            DIRECTORY | REPARSE,
            "metadata refresh must not turn a directory junction into a file"
        );
    }

    #[test]
    fn unavailable_root_keeps_saved_rows_and_marks_pending_coverage() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("source");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("saved.txt"), b"saved").unwrap();
        let (mut index, _) = rustsearch_windows::walk(&root, false).unwrap();
        std::fs::remove_dir_all(&root).unwrap();
        let mut work = Recovery::default();
        work.schedule(root, true);
        let search = SearchIndex::build(&index);
        let delta = work
            .step(
                &index,
                &search,
                &[],
                &[],
                false,
                &dir.path().join("excluded"),
            )
            .unwrap();
        drop(search);
        index.apply_delta(delta).unwrap();
        assert_eq!(index.counts().0, 1);
        assert!(
            index
                .metadata
                .pending_refresh
                .contains(&index.metadata.root_id)
        );
        assert!(work.is_empty());
    }

    #[test]
    fn live_directory_changes_jump_ahead_of_startup_recovery() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("source");
        for n in 0..80 {
            std::fs::create_dir_all(root.join(format!("dir-{n}"))).unwrap();
        }
        let (mut index, _) = rustsearch_windows::walk(&root, false).unwrap();
        let mut work = Recovery::default();
        work.schedule(root.clone(), true);
        std::fs::write(root.join("trigger"), b"x").unwrap();
        let search = SearchIndex::build(&index);
        let update = work
            .step_with_limit(
                &index,
                &search,
                &[event(
                    EventKind::Create(notify::event::CreateKind::File),
                    vec![root.join("trigger")],
                )],
                &[],
                false,
                &dir.path().join("excluded"),
                1,
            )
            .unwrap();
        drop(search);
        index.apply_delta(update).unwrap();
        let target = *work.hot.back().or_else(|| work.cold.back()).unwrap();
        let path = PathBuf::from(std::ffi::OsString::from_wide(
            &index.path_utf16(index.get(target).unwrap()).unwrap(),
        ))
        .join("urgent.txt");
        std::fs::write(&path, b"urgent").unwrap();
        let search = SearchIndex::build(&index);
        let update = work
            .step_with_limit(
                &index,
                &search,
                &[event(
                    EventKind::Create(notify::event::CreateKind::File),
                    vec![path],
                )],
                &[],
                false,
                &dir.path().join("excluded"),
                1,
            )
            .unwrap();
        drop(search);
        index.apply_delta(update).unwrap();
        assert_eq!(
            rustsearch_query::Query {
                text: "urgent.txt".into(),
                ..Default::default()
            }
            .execute(&index)
            .total,
            1,
            "live changes must not wait behind startup directories"
        );
        assert!(!work.is_empty());
    }

    #[test]
    fn deleting_an_ancestor_during_recovery_does_not_invalidate_queued_paths() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("source");
        std::fs::create_dir_all(root.join("B/deep/deeper")).unwrap();
        std::fs::write(root.join("B/deep/deeper/old.txt"), b"old").unwrap();
        let (mut index, _) = rustsearch_windows::walk(&root, false).unwrap();
        let mut work = Recovery::default();
        work.schedule(root.clone(), true);
        for events in [
            vec![],
            vec![event(
                EventKind::Create(notify::event::CreateKind::Any),
                vec![root.join("B/trigger")],
            )],
        ] {
            let search = SearchIndex::build(&index);
            let update = work
                .step_with_limit(
                    &index,
                    &search,
                    &events,
                    &[],
                    false,
                    &dir.path().join("excluded"),
                    1,
                )
                .unwrap();
            drop(search);
            index.apply_delta(update).unwrap();
        }
        std::fs::remove_dir_all(root.join("B")).unwrap();
        let search = SearchIndex::build(&index);
        let update=work.step_with_limit(&index,&search,&[event(EventKind::Remove(notify::event::RemoveKind::Folder),vec![root.join("B")])],&[],false,&dir.path().join("excluded"),32).expect("deleting an ancestor must discard queued descendant jobs instead of forcing a rebuild");
        drop(search);
        index.apply_delta(update).unwrap();
        assert_eq!(index.counts().0, 0);
    }

    #[test]
    fn bounded_recovery_preserves_queued_directory_moves_and_finishes_all_children() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("source");
        for folder in ["A", "B", "C"] {
            std::fs::create_dir_all(root.join(folder)).unwrap();
            for file in 0..4 {
                std::fs::write(root.join(folder).join(format!("file-{file}")), b"x").unwrap();
            }
        }
        let (mut index, _) = rustsearch_windows::walk(&root, false).unwrap();
        let search = SearchIndex::build(&index);
        let c = search
            .child(
                index.metadata.root_id,
                &"C".encode_utf16().collect::<Vec<_>>(),
            )
            .unwrap()
            .id;
        let child = search
            .child(c, &"file-0".encode_utf16().collect::<Vec<_>>())
            .unwrap()
            .id;
        let mut work = Recovery::default();
        work.schedule(root.clone(), true);
        let update = work
            .step_with_limit(
                &index,
                &search,
                &[],
                &[],
                false,
                &dir.path().join("excluded"),
                1,
            )
            .unwrap();
        drop(search);
        index.apply_delta(update).unwrap();
        assert!(!work.is_empty());
        std::fs::remove_dir_all(root.join("B")).unwrap();
        std::fs::remove_file(root.join("A/file-0")).unwrap();
        std::fs::rename(root.join("C"), root.join("Ω")).unwrap();
        std::fs::create_dir_all(root.join("Ω/new/deep")).unwrap();
        std::fs::write(root.join("Ω/new/deep/added.txt"), b"new").unwrap();
        let events = [event(
            EventKind::Modify(ModifyKind::Name(RenameMode::Both)),
            vec![root.join("C"), root.join("Ω")],
        )];
        for step in 0..20 {
            let search = SearchIndex::build(&index);
            let update = work
                .step_with_limit(
                    &index,
                    &search,
                    if step == 0 { &events } else { &[] },
                    &[],
                    false,
                    &dir.path().join("excluded"),
                    1,
                )
                .unwrap();
            drop(search);
            index.apply_delta(update).unwrap();
            if work.is_empty() {
                break;
            }
        }
        assert!(
            work.is_empty(),
            "bounded recovery must finish rather than restart its root"
        );
        assert!(
            work.parent_runs.is_none(),
            "temporary lookup memory must be released"
        );
        assert_eq!(
            index.path_utf16(index.get(child).unwrap()).unwrap(),
            root.join("Ω/file-0")
                .canonicalize()
                .unwrap()
                .as_os_str()
                .encode_wide()
                .collect::<Vec<_>>()
        );
        let (fresh, _) = rustsearch_windows::walk(&root, false).unwrap();
        let paths = |index: &Index| {
            index
                .live_entries()
                .map(|e| index.path_utf16(e).unwrap())
                .collect::<std::collections::BTreeSet<_>>()
        };
        assert_eq!(paths(&index), paths(&fresh));
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
