use crate::catalog::Catalog;
use anyhow::{Context, Result, ensure};
use rustsearch_core::{DIRECTORY, Index, SIZE_KNOWN};
use rustsearch_query::{Kind, Mode, Query, SearchIndex};
use std::{
    path::PathBuf,
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Instant,
};

#[derive(Clone, Debug, Default)]
pub struct SearchSpec {
    pub text: String,
    pub kind: Kind,
    pub mode: Mode,
    pub extension: Option<String>,
    pub min_size: Option<u64>,
    pub largest: bool,
    pub limit: usize,
    pub source: Option<usize>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScanMode {
    Accessible,
    Native,
}
#[derive(Debug)]
pub enum Request {
    Cancel,
    Open {
        path: PathBuf,
        generation: u64,
        search: SearchSpec,
    },
    ScanFolder {
        root: PathBuf,
        output: PathBuf,
        generation: u64,
        search: SearchSpec,
    },
    ScanComputer {
        roots: Vec<PathBuf>,
        directory: PathBuf,
        mode: ScanMode,
        sizes: bool,
        generation: u64,
        search: SearchSpec,
    },
    Search {
        generation: u64,
        search: SearchSpec,
    },
}
#[derive(Clone, Debug)]
pub struct VolumeSummary {
    pub root: String,
    pub records: usize,
    pub sequence: u64,
    pub skipped: u64,
    pub reparse: u64,
}
#[derive(Clone, Debug)]
pub struct SourceInfo {
    pub path: PathBuf,
    pub root: String,
    pub backend: String,
    pub records: usize,
    pub files: usize,
    pub folders: usize,
    pub sized_files: usize,
    pub pending: usize,
    pub sequence: u64,
    pub memory_bytes: usize,
    pub load_ms: f64,
    pub prepare_ms: f64,
    pub cache_state: String,
    pub volumes: Vec<VolumeSummary>,
    pub warnings: Vec<String>,
}
#[derive(Clone, Debug)]
pub struct Row {
    pub id: u64,
    pub source: usize,
    pub name: String,
    pub location: String,
    pub path: Option<Vec<u16>>,
    pub path_error: Option<String>,
    pub directory: bool,
    pub size: Option<u64>,
    pub lossy_display: bool,
}
#[derive(Debug)]
pub enum Event {
    Progress {
        generation: u64,
        records: u64,
        stage: String,
    },
    Results {
        generation: u64,
        source: Arc<SourceInfo>,
        total: usize,
        rows: Vec<Row>,
        query_ms: f64,
    },
    Error {
        generation: u64,
        message: String,
    },
}
#[derive(Default)]
struct Pending {
    request: Option<Request>,
    stopped: bool,
}
#[derive(Default)]
struct Mailbox {
    pending: Mutex<Pending>,
    wake: Condvar,
    generation: AtomicU64,
}
impl Mailbox {
    fn take(&self) -> Option<Request> {
        let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            if pending.stopped {
                return None;
            }
            if let Some(request) = pending.request.take() {
                return Some(request);
            }
            pending = self.wake.wait(pending).unwrap_or_else(|e| e.into_inner());
        }
    }
    fn current(&self, generation: u64) -> bool {
        self.generation.load(Ordering::Acquire) == generation
    }
}
/// Owns each volume exactly once. Only bounded hydrated rows cross to the UI.
pub struct Engine {
    mailbox: Arc<Mailbox>,
    pub events: async_channel::Receiver<Event>,
}
impl Default for Engine {
    fn default() -> Self {
        Self::new()
    }
}
type Reply<'a> = (
    &'a async_channel::Sender<Event>,
    &'a async_channel::Receiver<Event>,
);
struct Loaded {
    indexes: Vec<Index>,
    paths: Vec<PathBuf>,
    path: PathBuf,
    warnings: Vec<String>,
}
impl Engine {
    pub fn new() -> Self {
        let mailbox = Arc::new(Mailbox::default());
        let worker = mailbox.clone();
        let (tx, rx) = async_channel::bounded(1);
        let drain = rx.clone();
        std::thread::Builder::new()
            .name("rustsearch-index".into())
            .spawn(move || {
                let reply = (&tx, &drain);
                let mut next = worker.take();
                while let Some(request) = next {
                    let start = Instant::now();
                    let (generation, search, loaded) = match request {
                        Request::Open {
                            path,
                            generation,
                            search,
                        } => (
                            generation,
                            search,
                            load_source(path, generation, &worker, reply),
                        ),
                        Request::ScanFolder {
                            root,
                            output,
                            generation,
                            search,
                        } => {
                            let loaded = (|| -> Result<Loaded> {
                                let index =
                                    scan_folder(&root, &output, true, generation, &worker, reply)?;
                                Ok(Loaded {
                                    indexes: vec![index],
                                    paths: vec![output.clone()],
                                    path: output,
                                    warnings: vec![],
                                })
                            })();
                            (generation, search, loaded)
                        }
                        Request::ScanComputer {
                            roots,
                            directory,
                            mode,
                            sizes,
                            generation,
                            search,
                        } => {
                            let loaded = scan_computer(
                                roots, directory, mode, sizes, generation, &worker, reply,
                            );
                            (generation, search, loaded)
                        }
                        Request::Search { .. } | Request::Cancel => {
                            next = worker.take();
                            continue;
                        }
                    };
                    match loaded {
                        Ok(loaded) => {
                            if !worker.current(generation) {
                                next = worker.take();
                                continue;
                            }
                            let load_ms = start.elapsed().as_secs_f64() * 1000.;
                            let start = Instant::now();
                            let mut prepared = Vec::with_capacity(loaded.indexes.len());
                            let mut cache_states = Vec::new();
                            for (index, path) in loaded.indexes.iter().zip(&loaded.paths) {
                                if !worker.current(generation) {
                                    break;
                                }
                                progress(
                                    generation,
                                    index.entries.len() as u64,
                                    format!("Preparing search · {}", index.metadata.root),
                                    reply,
                                );
                                let (search, cache) = SearchIndex::prepare(index, path, true);
                                prepared.push(search);
                                cache_states.push(cache.state);
                            }
                            if !worker.current(generation) {
                                next = worker.take();
                                continue;
                            }
                            let mut info = SourceInfo {
                                path: loaded.path,
                                root: if loaded.indexes.len() == 1 {
                                    loaded.indexes[0].metadata.root.clone()
                                } else {
                                    "This computer".into()
                                },
                                backend: if loaded.indexes.len() == 1 {
                                    loaded.indexes[0].metadata.source.clone()
                                } else {
                                    "multi-volume".into()
                                },
                                records: 0,
                                files: 0,
                                folders: 0,
                                sized_files: 0,
                                pending: 0,
                                sequence: 0,
                                memory_bytes: 0,
                                load_ms,
                                prepare_ms: start.elapsed().as_secs_f64() * 1000.,
                                cache_state: if cache_states.iter().all(|s| *s == "hit") {
                                    "hit".into()
                                } else {
                                    cache_states.join(", ")
                                },
                                volumes: vec![],
                                warnings: loaded.warnings,
                            };
                            for (index, search) in loaded.indexes.iter().zip(&prepared) {
                                let (records, folders, sized, _) = index.counts();
                                info.records += records;
                                info.files += records - folders;
                                info.folders += folders;
                                info.sized_files += sized;
                                info.pending += index.metadata.pending_refresh.len();
                                info.sequence = info.sequence.max(index.log_sequence);
                                info.memory_bytes +=
                                    index.allocated_bytes() + search.allocated_bytes();
                                info.volumes.push(VolumeSummary {
                                    root: index.metadata.root.clone(),
                                    records,
                                    sequence: index.log_sequence,
                                    skipped: index.metadata.skipped_entries,
                                    reparse: index.metadata.reparse_points_not_followed,
                                });
                            }
                            let info = Arc::new(info);
                            respond(
                                &loaded.indexes,
                                &prepared,
                                info.clone(),
                                generation,
                                search,
                                &worker,
                                reply,
                            );
                            loop {
                                match worker.take() {
                                    Some(Request::Search { generation, search }) => respond(
                                        &loaded.indexes,
                                        &prepared,
                                        info.clone(),
                                        generation,
                                        search,
                                        &worker,
                                        reply,
                                    ),
                                    other => {
                                        next = other;
                                        break;
                                    }
                                }
                            }
                            // Borrowing search tables drop before their source indexes, then the next source loads.
                        }
                        Err(error) => {
                            if worker.current(generation) {
                                send_latest(
                                    Event::Error {
                                        generation,
                                        message: format!("{error:#}"),
                                    },
                                    reply,
                                );
                            }
                            next = worker.take();
                        }
                    }
                }
            })
            .expect("start search worker");
        Self {
            mailbox,
            events: rx,
        }
    }
    pub fn submit(&self, request: Request, generation: u64) {
        self.mailbox.generation.store(generation, Ordering::Release);
        self.mailbox
            .pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .request = Some(request);
        self.mailbox.wake.notify_one();
    }
}
impl Drop for Engine {
    fn drop(&mut self) {
        self.mailbox
            .pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .stopped = true;
        self.mailbox.generation.fetch_add(1, Ordering::AcqRel);
        self.mailbox.wake.notify_one();
    }
}
fn send_latest(event: Event, (tx, drain): Reply<'_>) {
    if let Err(async_channel::TrySendError::Full(event)) = tx.try_send(event) {
        let _ = drain.try_recv();
        let _ = tx.try_send(event);
    }
}
fn progress(generation: u64, records: u64, stage: String, reply: Reply<'_>) {
    send_latest(
        Event::Progress {
            generation,
            records,
            stage,
        },
        reply,
    );
}
fn load_source(
    path: PathBuf,
    generation: u64,
    mailbox: &Mailbox,
    reply: Reply<'_>,
) -> Result<Loaded> {
    let (paths, warnings) = if path.extension().is_some_and(|e| e == "rscatalog") {
        let catalog = Catalog::load(&path)?;
        (catalog.snapshots, catalog.warnings)
    } else {
        (vec![path.clone()], vec![])
    };
    let mut indexes = Vec::with_capacity(paths.len());
    for snapshot in &paths {
        ensure!(mailbox.current(generation), "Index loading cancelled");
        progress(
            generation,
            0,
            format!("Loading {}", snapshot.display()),
            reply,
        );
        indexes.push(
            rustsearch_storage::load(snapshot)
                .with_context(|| format!("load {}", snapshot.display()))?,
        );
    }
    Ok(Loaded {
        indexes,
        paths,
        path,
        warnings,
    })
}
fn scan_folder(
    root: &std::path::Path,
    output: &std::path::Path,
    sizes: bool,
    generation: u64,
    mailbox: &Mailbox,
    reply: Reply<'_>,
) -> Result<Index> {
    std::fs::create_dir_all(output.parent().context("missing index directory")?)?;
    let mut pulse = Instant::now();
    progress(generation, 0, format!("Scanning {}", root.display()), reply);
    let (mut index, _) = rustsearch_windows::walk_controlled(root, sizes, |records| {
        if !mailbox.current(generation) {
            return false;
        }
        if pulse.elapsed().as_millis() >= 200 {
            progress(
                generation,
                records,
                format!("Scanning {}", root.display()),
                reply,
            );
            pulse = Instant::now();
        }
        true
    })?;
    ensure!(mailbox.current(generation), "Folder indexing cancelled");
    index.consolidate()?;
    ensure!(mailbox.current(generation), "Folder indexing cancelled");
    index.snapshot_identity = Some(rustsearch_storage::save(output, &index)?);
    Ok(index)
}
fn scan_computer(
    roots: Vec<PathBuf>,
    directory: PathBuf,
    mode: ScanMode,
    sizes: bool,
    generation: u64,
    mailbox: &Mailbox,
    reply: Reply<'_>,
) -> Result<Loaded> {
    ensure!(!roots.is_empty() && roots.len() <= 26, "select 1–26 drives");
    let mut unique = std::collections::HashSet::new();
    for root in &roots {
        ensure!(root.is_absolute(), "scan roots must be absolute");
        let normalized = root.canonicalize().unwrap_or_else(|_| root.clone());
        ensure!(unique.insert(normalized), "duplicate scan root");
    }
    std::fs::create_dir_all(&directory)?;
    let mut catalog = Catalog {
        version: 1,
        snapshots: vec![],
        warnings: vec![],
    };
    for (ix, root) in roots.iter().enumerate() {
        ensure!(mailbox.current(generation), "Computer indexing cancelled");
        let output = directory.join(format!("drive-{ix}.rsidx"));
        let scanned = if mode == ScanMode::Native {
            let mut pulse = Instant::now();
            progress(
                generation,
                0,
                format!(
                    "NTFS · {} · drive {} of {}",
                    root.display(),
                    ix + 1,
                    roots.len()
                ),
                reply,
            );
            (|| -> Result<()> {
                let text = root
                    .to_str()
                    .context("invalid volume name")?
                    .trim_end_matches('\\');
                let (mut index, _) = rustsearch_windows::scan_ntfs_controlled(
                    text,
                    sizes.then_some(usize::MAX),
                    |records| {
                        if !mailbox.current(generation) {
                            return false;
                        }
                        if pulse.elapsed().as_millis() >= 200 {
                            progress(
                                generation,
                                records,
                                format!(
                                    "NTFS · {} · drive {} of {}",
                                    root.display(),
                                    ix + 1,
                                    roots.len()
                                ),
                                reply,
                            );
                            pulse = Instant::now();
                        }
                        true
                    },
                )?;
                ensure!(mailbox.current(generation), "Computer indexing cancelled");
                index.snapshot_identity = Some(rustsearch_storage::save(&output, &index)?);
                Ok(())
            })()
        } else {
            scan_folder(root, &output, sizes, generation, mailbox, reply).map(|_| ())
        };
        ensure!(mailbox.current(generation), "Computer indexing cancelled");
        match scanned {
            Ok(()) => catalog.snapshots.push(output),
            Err(error) => catalog
                .warnings
                .push(format!("{}: {error:#}", root.display())),
        }
        // Each scan is dropped before the next drive. Published sources are loaded once afterwards.
    }
    ensure!(
        !catalog.snapshots.is_empty(),
        "No drives could be indexed. {}",
        catalog.warnings.join("; ")
    );
    let path = directory.join("computer.rscatalog");
    catalog.save(&path)?;
    load_source(path, generation, mailbox, reply)
}
fn respond(
    indexes: &[Index],
    prepared: &[SearchIndex<'_>],
    source: Arc<SourceInfo>,
    generation: u64,
    search: SearchSpec,
    mailbox: &Mailbox,
    reply: Reply<'_>,
) {
    if !mailbox.current(generation) {
        return;
    }
    let start = Instant::now();
    let limit = search.limit.min(10_000);
    let mut candidates = Vec::with_capacity(limit.saturating_mul(2));
    let mut total = 0;
    for (ix, table) in prepared.iter().enumerate() {
        if search.source.is_some_and(|selected| selected != ix) {
            continue;
        }
        if !mailbox.current(generation) {
            return;
        }
        let result = table.execute(&Query {
            text: search.text.clone(),
            kind: search.kind,
            mode: search.mode,
            extension: search.extension.clone(),
            min_size: search.min_size,
            largest: search.largest,
            limit: if search.largest {
                limit
            } else {
                limit.saturating_sub(candidates.len())
            },
        });
        total += result.total;
        candidates.extend(result.rows.into_iter().map(|id| {
            let size = if search.largest {
                indexes[ix].get(id).map_or(0, |e| indexes[ix].size(e))
            } else {
                0
            };
            (ix, id, size)
        }));
        if search.largest {
            candidates.sort_unstable_by(|&(a, aid, asize), &(b, bid, bsize)| {
                bsize.cmp(&asize).then(a.cmp(&b)).then(aid.cmp(&bid))
            });
            candidates.truncate(limit);
        }
    }
    let query_ms = start.elapsed().as_secs_f64() * 1000.;
    let mut rows = Vec::with_capacity(candidates.len());
    for (source_ix, id, _) in candidates {
        if !mailbox.current(generation) {
            return;
        }
        let index = &indexes[source_ix];
        let Some(entry) = index.get(id) else {
            continue;
        };
        let name = index.name(entry);
        let path = index.path_utf16(entry);
        let location = path
            .as_ref()
            .ok()
            .map(|p| {
                let end = p.iter().rposition(|c| *c == 92).unwrap_or(p.len());
                String::from_utf16_lossy(&p[..end])
            })
            .unwrap_or_else(|| "Unresolved parent".into());
        let path_error = path.as_ref().err().map(ToString::to_string);
        rows.push(Row {
            id,
            source: source_ix,
            name: String::from_utf16_lossy(&name),
            location,
            path: path.ok(),
            path_error,
            directory: entry.flags & DIRECTORY != 0,
            size: (entry.flags & SIZE_KNOWN != 0).then(|| index.size(entry)),
            lossy_display: String::from_utf16(&name).is_err(),
        });
    }
    if mailbox.current(generation) {
        send_latest(
            Event::Results {
                generation,
                source,
                total,
                rows,
                query_ms,
            },
            reply,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustsearch_core::{Delta, Metadata, Record};
    use std::{collections::BTreeMap, sync::mpsc, time::Duration};

    fn fixture(path: &std::path::Path) {
        let mut index = Index::new(Metadata {
            source: "walk".into(),
            root: path.parent().unwrap().display().to_string(),
            volume_id: "desktop-test".into(),
            filesystem: "NTFS".into(),
            root_id: 1,
            created_unix_ms: 0,
            checkpoint: None,
            pending_refresh: vec![],
            skipped_entries: 0,
            reparse_points_not_followed: 0,
        });
        for (id, name, size, flags) in [
            (2, "alpha.txt", Some(0), 0),
            (3, "beta.txt", None, 0),
            (4, "Nested", None, DIRECTORY),
        ] {
            index
                .push(Record {
                    id,
                    parent: 1,
                    usn: 0,
                    size,
                    flags,
                    name: name.encode_utf16().collect(),
                })
                .unwrap();
        }
        index.finish().unwrap();
        rustsearch_storage::save(path, &index).unwrap();
    }
    fn replies(engine: &Engine) -> mpsc::Receiver<Event> {
        let events = engine.events.clone();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            while let Ok(event) = events.recv_blocking() {
                if tx.send(event).is_err() {
                    break;
                }
            }
        });
        rx
    }
    fn reply(rx: &mpsc::Receiver<Event>, generation: u64) -> Event {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let event = rx
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .expect("bounded engine reply");
            match &event {
                Event::Results { generation: g, .. } | Event::Error { generation: g, .. }
                    if *g == generation =>
                {
                    return event;
                }
                _ => {}
            }
        }
    }
    fn search() -> SearchSpec {
        SearchSpec {
            text: "a".into(),
            limit: 100,
            ..Default::default()
        }
    }

    #[test]
    fn federated_queries_keep_colliding_ids_and_global_largest_order() {
        let dir = tempfile::tempdir().unwrap();
        let first = dir.path().join("first.rsidx");
        let second = dir.path().join("second.rsidx");
        fixture(&first);
        fixture(&second);
        let mut index = rustsearch_storage::load(&second).unwrap();
        index.set_size(0, 42);
        rustsearch_storage::save(&second, &index).unwrap();
        let path = dir.path().join("computer.rscatalog");
        Catalog {
            version: 1,
            snapshots: vec![first, second],
            warnings: vec!["Offline test drive".into()],
        }
        .save(&path)
        .unwrap();
        let engine = Engine::new();
        let rx = replies(&engine);
        engine.submit(
            Request::Open {
                path: path.clone(),
                generation: 1,
                search: SearchSpec {
                    limit: 1,
                    ..search()
                },
            },
            1,
        );
        let Event::Results {
            source,
            total,
            rows,
            ..
        } = reply(&rx, 1)
        else {
            panic!("catalog load failed")
        };
        assert_eq!(source.records, 6);
        assert_eq!(source.volumes.len(), 2);
        assert_eq!(source.warnings.len(), 1);
        assert_eq!(total, 4);
        assert_eq!(rows.len(), 1);
        assert_eq!((rows[0].source, rows[0].id), (0, 2));
        engine.submit(
            Request::Search {
                generation: 2,
                search: SearchSpec {
                    text: String::new(),
                    largest: true,
                    limit: 2,
                    ..Default::default()
                },
            },
            2,
        );
        let Event::Results { total, rows, .. } = reply(&rx, 2) else {
            panic!("largest failed")
        };
        assert_eq!(total, 2);
        assert_eq!(
            rows.iter()
                .map(|r| (r.source, r.id, r.size))
                .collect::<Vec<_>>(),
            vec![(1, 2, Some(42)), (0, 2, Some(0))]
        );
        engine.submit(
            Request::Search {
                generation: 3,
                search: SearchSpec {
                    source: Some(1),
                    ..search()
                },
            },
            3,
        );
        let Event::Results { total, rows, .. } = reply(&rx, 3) else {
            panic!("location filter failed")
        };
        assert_eq!(total, 2);
        assert!(rows.iter().all(|r| r.source == 1));
        engine.submit(
            Request::Open {
                path,
                generation: 4,
                search: search(),
            },
            4,
        );
        let Event::Results {
            source,
            total,
            rows,
            ..
        } = reply(&rx, 4)
        else {
            panic!("catalog reload failed")
        };
        assert_eq!(source.cache_state, "hit");
        assert_eq!(total, 4);
        assert_eq!(
            rows.iter().map(|r| (r.source, r.id)).collect::<Vec<_>>(),
            vec![(0, 2), (0, 3), (1, 2), (1, 3)]
        );
    }

    #[test]
    fn computer_scan_persists_two_roots_and_reports_an_unavailable_root() {
        let dir = tempfile::tempdir().unwrap();
        let first = dir.path().join("first");
        let second = dir.path().join("second");
        std::fs::create_dir(&first).unwrap();
        std::fs::create_dir(&second).unwrap();
        std::fs::write(first.join("alpha.txt"), b"one").unwrap();
        std::fs::write(second.join("alpha.txt"), b"two-two").unwrap();
        let output = dir.path().join("indexes");
        let engine = Engine::new();
        let rx = replies(&engine);
        engine.submit(
            Request::ScanComputer {
                roots: vec![first, dir.path().join("missing"), second],
                directory: output.clone(),
                mode: ScanMode::Accessible,
                sizes: true,
                generation: 1,
                search: search(),
            },
            1,
        );
        let Event::Results {
            source,
            total,
            rows,
            ..
        } = reply(&rx, 1)
        else {
            panic!("computer scan failed")
        };
        assert_eq!(total, 2);
        assert_eq!(source.volumes.len(), 2);
        assert_eq!(source.warnings.len(), 1);
        assert_eq!(
            rows.iter()
                .map(|r| (r.source, r.id, r.size))
                .collect::<Vec<_>>(),
            vec![(0, 2, Some(3)), (1, 2, Some(7))]
        );
        let catalog = Catalog::load(&output.join("computer.rscatalog")).unwrap();
        assert_eq!(catalog.snapshots.len(), 2);
        assert!(catalog.warnings[0].contains("missing"));
        engine.submit(
            Request::ScanComputer {
                roots: vec![],
                directory: output,
                mode: ScanMode::Accessible,
                sizes: false,
                generation: 2,
                search: search(),
            },
            2,
        );
        assert!(matches!(reply(&rx, 2), Event::Error { .. }));
    }

    #[test]
    fn worker_recovers_from_bad_source_and_delivers_the_latest_search() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.rsidx");
        fixture(&path);
        let engine = Engine::new();
        let rx = replies(&engine);
        engine.submit(
            Request::Open {
                path: dir.path().join("missing.rsidx"),
                generation: 1,
                search: search(),
            },
            1,
        );
        assert!(matches!(reply(&rx, 1), Event::Error { .. }));
        engine.submit(
            Request::Open {
                path,
                generation: 2,
                search: search(),
            },
            2,
        );
        let Event::Results { total, rows, .. } = reply(&rx, 2) else {
            panic!("valid source did not load")
        };
        assert_eq!(total, 2);
        assert_eq!(rows[0].size, Some(0));
        assert_eq!(rows[1].size, None);
        for generation in 3..=20 {
            engine.submit(
                Request::Search {
                    generation,
                    search: SearchSpec {
                        text: if generation == 20 {
                            "no-match"
                        } else {
                            "alpha"
                        }
                        .into(),
                        ..search()
                    },
                },
                generation,
            );
        }
        let Event::Results { total, rows, .. } = reply(&rx, 20) else {
            panic!("latest query failed")
        };
        assert_eq!(total, 0);
        assert!(rows.is_empty());
    }

    #[test]
    fn worker_reloads_two_committed_delta_views_without_changing_base_cache() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.rsidx");
        fixture(&path);
        let engine = Engine::new();
        let rx = replies(&engine);
        engine.submit(
            Request::Open {
                path: path.clone(),
                generation: 1,
                search: search(),
            },
            1,
        );
        assert!(matches!(reply(&rx, 1), Event::Results { total: 2, .. }));
        let cached = std::fs::read(rustsearch_query::cache_path(&path)).unwrap();
        for sequence in 1..=2 {
            let index = rustsearch_storage::load(&path).unwrap();
            let records = if sequence == 1 {
                BTreeMap::from([
                    (
                        2,
                        Some(Record {
                            id: 2,
                            parent: 1,
                            usn: 1,
                            size: Some(42),
                            flags: 0,
                            name: "changed.txt".encode_utf16().collect(),
                        }),
                    ),
                    (3, None),
                    (
                        500,
                        Some(Record {
                            id: 500,
                            parent: 1,
                            usn: 1,
                            size: Some(0),
                            flags: 0,
                            name: "alpha-new.txt".encode_utf16().collect(),
                        }),
                    ),
                ])
            } else {
                BTreeMap::from([
                    (2, None),
                    (
                        500,
                        Some(Record {
                            id: 500,
                            parent: 1,
                            usn: 2,
                            size: None,
                            flags: DIRECTORY,
                            name: "Alpha folder".encode_utf16().collect(),
                        }),
                    ),
                ])
            };
            let guard = rustsearch_storage::WriteGuard::acquire(&path).unwrap();
            rustsearch_storage::append_delta(
                &guard,
                &index,
                &Delta {
                    previous_sequence: index.log_sequence,
                    previous_checkpoint: None,
                    metadata: index.metadata.clone(),
                    records,
                },
            )
            .unwrap();
            drop(guard);
            let generation = sequence + 1;
            engine.submit(
                Request::Open {
                    path: path.clone(),
                    generation,
                    search: search(),
                },
                generation,
            );
            let Event::Results {
                source,
                total,
                rows,
                ..
            } = reply(&rx, generation)
            else {
                panic!("delta view failed")
            };
            assert_eq!(source.sequence, sequence);
            assert_eq!(source.cache_state, "hit");
            if sequence == 1 {
                assert_eq!(total, 2);
                assert_eq!(rows.iter().map(|r| r.id).collect::<Vec<_>>(), vec![2, 500]);
            } else {
                assert_eq!(total, 1);
                assert!(rows[0].directory);
                assert_eq!(rows[0].name, "Alpha folder");
            }
            assert_eq!(
                std::fs::read(rustsearch_query::cache_path(&path)).unwrap(),
                cached
            );
        }
    }
}
