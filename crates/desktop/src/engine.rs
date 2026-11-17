use crate::catalog::Catalog;
use anyhow::{Context, Result, ensure};
use rustsearch_core::{DIRECTORY, Index, SIZE_KNOWN};
use rustsearch_query::{Kind, Mode, Query, SearchIndex};
use serde::{Deserialize, Serialize};
use std::{
    path::PathBuf,
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Instant,
};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
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
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ScanMode {
    #[default]
    Accessible,
    Native,
}
#[derive(Debug, Serialize, Deserialize)]
pub enum Request {
    Cancel,
    Current {
        generation: u64,
    },
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
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VolumeSummary {
    pub root: String,
    pub records: usize,
    pub sequence: u64,
    pub skipped: u64,
    pub reparse: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
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
    pub live: bool,
    #[serde(default)]
    pub recovering: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
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
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Event {
    Runtime(crate::worker::Runtime),
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
    loading: std::sync::atomic::AtomicBool,
    control: Option<Arc<crate::live::Control>>,
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
    fn poll(&self) -> (bool, Option<Request>) {
        self.poll_timeout(std::time::Duration::from_millis(250))
    }
    fn poll_timeout(&self, timeout: std::time::Duration) -> (bool, Option<Request>) {
        let pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        let mut pending = if pending.request.is_none() && !pending.stopped {
            self.wake
                .wait_timeout(pending, timeout)
                .unwrap_or_else(|e| e.into_inner())
                .0
        } else {
            pending
        };
        (pending.stopped, pending.request.take())
    }
    fn current(&self, generation: u64) -> bool {
        self.generation.load(Ordering::Acquire) == generation
    }
    fn scanning(&self, generation: u64) -> bool {
        while self.current(generation)
            && self
                .control
                .as_ref()
                .is_some_and(|c| c.paused.load(Ordering::Acquire))
        {
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        self.current(generation)
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
        Self::local(None)
    }
    pub fn new_live(
        root: PathBuf,
        _roots: Vec<PathBuf>,
        control: Arc<crate::live::Control>,
    ) -> Self {
        Self::local(Some((root, control)))
    }
    pub fn remote(root: PathBuf) -> Self {
        let mailbox = Arc::new(Mailbox::default());
        let worker = mailbox.clone();
        let (tx, rx) = async_channel::bounded(8);
        std::thread::spawn(move || {
            let result = (|| -> Result<()> {
                let mut stream = crate::worker::connect_or_start(&root)?;
                let mut reader = stream.try_clone()?;
                let replies = tx.clone();
                let status_root = root.clone();
                std::thread::spawn(move || {
                    while let Ok(event) = crate::worker::receive::<Event>(&mut reader) {
                        if replies.send_blocking(event).is_err() {
                            break;
                        }
                    }
                    let mut status = crate::worker::runtime(&status_root);
                    status.running = false;
                    status.phase = "Stopped".into();
                    let _ = replies.send_blocking(Event::Runtime(status));
                });
                while let Some(request) = worker.take() {
                    let rpc = match request {
                        Request::Current { generation } => crate::worker::Rpc::Current(generation),
                        other => crate::worker::Rpc::Request(other),
                    };
                    crate::worker::send(&mut stream, &rpc)?;
                }
                let _ = stream.shutdown(std::net::Shutdown::Both);
                Ok(())
            })();
            if let Err(error) = result {
                let _ = tx.send_blocking(Event::Runtime(crate::worker::Runtime {
                    running: false,
                    error: Some(format!("{error:#}")),
                    phase: "Worker unavailable".into(),
                    ..Default::default()
                }));
            }
        });
        Self {
            mailbox,
            events: rx,
        }
    }
    fn local(live: Option<(PathBuf, Arc<crate::live::Control>)>) -> Self {
        let mailbox = Arc::new(Mailbox {
            control: live.as_ref().map(|(_, c)| c.clone()),
            ..Default::default()
        });
        let worker = mailbox.clone();
        let (tx, rx) = async_channel::bounded(1);
        let drain = rx.clone();
        std::thread::Builder::new()
            .name("rustsearch-index".into())
            .spawn(move || {
                let reply = (&tx, &drain);
                let mut next = worker.take();
                while let Some(request) = next {
                    if matches!(&request,Request::Open{..}|Request::ScanFolder{..}|Request::ScanComputer{..}) {worker.loading.store(true,Ordering::Release);}
                    let session_live = live
                        .as_ref()
                        .filter(|(root, _)| match &request {
                            Request::Open { path, .. } => path.starts_with(root),
                            Request::ScanFolder { output, .. } => output.starts_with(root),
                            Request::ScanComputer { directory, .. } => directory.starts_with(root),
                            _ => false,
                        })
                        .cloned();
                    let start = Instant::now();
                    let priority = live
                        .as_ref()
                        .map(|_| crate::live::BackgroundPriority::begin());
                    let watch_roots = match &request {
                        Request::ScanComputer { roots, .. } => roots.clone(),
                        Request::ScanFolder { root, .. } => vec![root.clone()],
                        _ => vec![],
                    };
                    let mut watch = session_live.as_ref().and_then(|(exclude, _)| {
                        crate::live::LiveWatch::new(
                            &watch_roots
                                .iter()
                                .map(|p| p.canonicalize().unwrap_or_else(|_| p.clone()))
                                .collect::<Vec<_>>(),
                            exclude.clone(),
                        )
                        .ok()
                    });
                    let resumed=matches!(&request,Request::Open{..}) && session_live.is_some();
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
                        Request::Search {generation,..} | Request::Current {generation} => {
                            if worker.current(generation) {send_latest(Event::Error{generation,message:"No index is loaded. Choose drives or resume indexing in Settings.".into()},reply);}
                            next=worker.take();continue;
                        }
                        Request::Cancel=>{next=worker.take();continue;}
                    };
                    drop(priority);
                    match loaded {
                        Ok(mut loaded) => {
                            if !worker.current(generation) {
                                next = worker.take();
                                continue;
                            }
                            if let Some((exclude, _)) = &live
                                && watch_roots.is_empty()
                            {
                                watch = crate::live::LiveWatch::new(
                                    &loaded
                                        .indexes
                                        .iter()
                                        .map(|i| PathBuf::from(&i.metadata.root))
                                        .collect::<Vec<_>>(),
                                    exclude.clone(),
                                )
                                .ok();
                            }
                            let load_ms = start.elapsed().as_secs_f64() * 1000.;
                            let mut retained: Option<Vec<rustsearch_query::SearchTables>> = None;
                            let mut active_generation = generation;
                            let mut active_search = search;
                            let mut last_refresh = Instant::now();
                            let mut last_watch_stats=None;
                            let mut recoveries:Vec<_>=loaded.indexes.iter().map(|index| {
                                let mut work=crate::live::Recovery::default();
                                if resumed && index.metadata.source!="ntfs-usn" {work.schedule(PathBuf::from(&index.metadata.root),true);}
                                work
                            }).collect();
                            let mut counts:Vec<_>=loaded.indexes.iter().map(Index::counts).collect();
                            'session: loop {
                                let start = Instant::now();
                                let mut prepared = Vec::new();
                                let mut cache_states = vec![];
                                if let Some(tables) = retained.take() {
                                    for (table, index) in tables.into_iter().zip(&loaded.indexes) {
                                        prepared.push(
                                            table.attach(index).expect("unchanged search base"),
                                        );
                                    }
                                    cache_states.push("live");
                                } else {
                                    for (index, path) in loaded.indexes.iter().zip(&loaded.paths) {
                                        if !worker.current(active_generation) {
                                            next = worker.take();
                                            break 'session;
                                        }
                                        progress(
                                            active_generation,
                                            index.entries.len() as u64,
                                            format!("Preparing search · {}", index.metadata.root),
                                            reply,
                                        );
                                        let (table, cache) =
                                            SearchIndex::prepare(index, path, true);
                                        prepared.push(table);
                                        cache_states.push(cache.state);
                                    }
                                }
                                let mut info = SourceInfo {
                                    path: loaded.path.clone(),
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
                                    warnings: loaded.warnings.clone(),
                                    live: session_live.is_some(),
                                    recovering: recoveries.iter().any(|work|!work.is_empty()),
                                };
                                if let Some(watch) = &watch {
                                    info.warnings.extend(watch.errors.clone());
                                }
                                for ((index, table), &(records, folders, sized, _)) in loaded.indexes.iter().zip(&prepared).zip(&counts) {
                                    info.records += records;
                                    info.folders += folders;
                                    info.files += records - folders;
                                    info.sized_files += sized;
                                    info.pending += index.metadata.pending_refresh.len();
                                    info.sequence = info.sequence.max(index.log_sequence);
                                    info.memory_bytes +=
                                        index.allocated_bytes() + table.allocated_bytes();
                                    info.volumes.push(VolumeSummary {
                                        root: index.metadata.root.clone(),
                                        records,
                                        sequence: index.log_sequence,
                                        skipped: index.metadata.skipped_entries,
                                        reparse: index.metadata.reparse_points_not_followed,
                                    });
                                }
                                let mut info = Arc::new(info);
                                worker.loading.store(false,Ordering::Release);
                                respond(
                                    &loaded.indexes,
                                    &prepared,
                                    info.clone(),
                                    active_generation,
                                    active_search.clone(),
                                    &worker,
                                    reply,
                                );
                                let pending_deltas = loop {
                                    let background_active=recoveries.iter().any(|work|!work.is_empty()) && session_live.as_ref().is_some_and(|(_,control)|!control.paused.load(Ordering::Acquire));
                                    let (stopped, request) = if session_live.is_some() {
                                        if background_active {worker.poll_timeout(std::time::Duration::ZERO)}else{worker.poll()}
                                    } else {
                                        match worker.take() {
                                            Some(r) => (false, Some(r)),
                                            None => (true, None),
                                        }
                                    };
                                    if stopped {
                                        next = None;
                                        break 'session;
                                    }
                                    match request {
                                        Some(Request::Search { generation, search }) => {
                                            worker.generation.store(generation,Ordering::Release);
                                            active_generation = generation;
                                            active_search = search;
                                            respond(
                                                &loaded.indexes,
                                                &prepared,
                                                info.clone(),
                                                active_generation,
                                                active_search.clone(),
                                                &worker,
                                                reply,
                                            );
                                        }
                                        Some(other) => {
                                            next = Some(other);
                                            break 'session;
                                        }
                                        None => {}
                                    }
                                    let Some((exclude, control)) = &session_live else {
                                        continue;
                                    };
                                    if control.paused.load(Ordering::Acquire) {
                                        continue;
                                    }
                                    // Live work gets a turn even while queries arrive faster than poll().
                                    if !background_active && last_refresh.elapsed() < std::time::Duration::from_millis(250) {continue;}
                                    let events = watch
                                        .as_ref()
                                        .map_or_else(crate::live::Changes::default, |w| w.take());
                                    if last_watch_stats.as_ref()!=Some(&events.stats) {
                                        let _=rustsearch_storage::write_derived_atomic(&exclude.join("watch-recovery.json"),|file| {
                                            serde_json::to_writer_pretty(file,&events.stats)?;Ok(())
                                        });
                                        last_watch_stats=Some(events.stats.clone());
                                    }
                                    let native_due=last_refresh.elapsed()>=std::time::Duration::from_secs(2)
                                        && loaded.indexes.iter().any(|i|i.metadata.source=="ntfs-usn");
                                    if events.is_empty() && !native_due && !background_active {continue;}
                                    let changes=(|| -> Result<Vec<Option<rustsearch_core::Delta>>> {
                                        let mut changes=vec![];
                                        for ((index, bound),recovery) in loaded.indexes.iter().zip(&prepared).zip(&mut recoveries) {
                                            let mut delta=if index.metadata.source=="ntfs-usn" {
                                                rustsearch_windows::refresh(index,control.sizes.load(Ordering::Acquire))?.0
                                            } else {
                                                recovery.step(index,bound,&events.events,&events.folders,control.sizes.load(Ordering::Acquire),exclude)?
                                            };
                                            if index.metadata.source=="ntfs-usn" {crate::live::exclude_native_storage(index,&mut delta,exclude);}
                                            let empty=delta.records.is_empty()
                                                && (index.metadata.source=="ntfs-usn" || delta.metadata.checkpoint==index.metadata.checkpoint)
                                                && delta.metadata.pending_refresh==index.metadata.pending_refresh;
                                            changes.push(if empty {None}else{Some(delta)});
                                        }
                                        Ok(changes)
                                    })();
                                    last_refresh=Instant::now();
                                    // Ordinary log/attribute notifications often do not change indexed
                                    // fields. Keep borrowing the same tables and statistics in that case.
                                    if changes.as_ref().is_ok_and(|items|items.iter().all(Option::is_none)) {
                                        let recovering=recoveries.iter().any(|work|!work.is_empty());
                                        if info.recovering!=recovering {
                                            let mut next_info=(*info).clone();next_info.recovering=recovering;info=Arc::new(next_info);
                                            respond(&loaded.indexes,&prepared,info.clone(),active_generation,active_search.clone(),&worker,reply);
                                        }
                                        continue;
                                    }
                                    break changes;
                                };
                                let tables = prepared
                                    .into_iter()
                                    .map(SearchIndex::detach)
                                    .collect::<Vec<_>>();
                                let (exclude, control) =
                                    session_live.as_ref().expect("live updates enabled");
                                let updated =
                                    (|| -> Result<Vec<rustsearch_query::SearchTables>> {
                                        let mut retained_tables = vec![];
                                        let deltas=pending_deltas?;
                                        for ((((index,path),table),delta),counts) in loaded.indexes.iter_mut().zip(&loaded.paths).zip(tables).zip(deltas).zip(&mut counts) {
                                            if let Some(delta)=delta {
                                                let guard=rustsearch_storage::WriteGuard::acquire(path)?;
                                                rustsearch_storage::append_delta(&guard,index,&delta)?;
                                                let updated_counts=delta_counts(index,&delta,*counts);
                                                index.apply_delta(delta)?;
                                                *counts=updated_counts;
                                            }
                                            retained_tables.push(table);
                                        }
                                        Ok(retained_tables)
                                    })();
                                last_refresh = Instant::now();
                                let tables = match updated {
                                    Ok(tables) => tables,
                                    Err(error) => {
                                        let _=rustsearch_storage::write_derived_atomic(&exclude.join("last-reconciliation.json"),|file| {
                                            serde_json::to_writer_pretty(file,&serde_json::json!({"at_ms":crate::worker::now_ms(),"reason":format!("{error:#}")}))?;Ok(())
                                        });
                                        worker.loading.store(true,Ordering::Release);
                                        progress(
                                            active_generation,
                                            0,
                                            format!("Reconciling after {error:#}"),
                                            reply,
                                        );
                                        next = Some(Request::ScanComputer {
                                            roots: loaded
                                                .indexes
                                                .iter()
                                                .map(|i| PathBuf::from(&i.metadata.root))
                                                .collect(),
                                            directory: exclude.join(format!(
                                                "managed-{}",
                                                crate::worker::now_ms()
                                            )),
                                            mode: if loaded.indexes.iter().all(|i|i.metadata.source=="ntfs-usn"){ScanMode::Native}else{ScanMode::Accessible},
                                            sizes: control.sizes.load(Ordering::Acquire),
                                            generation: active_generation,
                                            search: active_search.clone(),
                                        });
                                        break 'session;
                                    }
                                };
                                retained = Some(tables);
                            }
                        }
                        Err(error) => {
                            worker.loading.store(false,Ordering::Release);
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
        if !matches!(&request, Request::Search { .. })
            || !self.mailbox.loading.load(Ordering::Acquire)
        {
            self.mailbox.generation.store(generation, Ordering::Release);
        }
        self.mailbox
            .pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .request = Some(request);
        self.mailbox.wake.notify_one();
    }
}
fn delta_counts(
    index: &Index,
    delta: &rustsearch_core::Delta,
    mut counts: (usize, usize, usize, usize),
) -> (usize, usize, usize, usize) {
    for (&id, record) in &delta.records {
        if let Some(old) = index.get(id) {
            counts.0 -= 1;
            counts.1 -= usize::from(old.flags & DIRECTORY != 0);
            counts.2 -= usize::from(old.flags & (DIRECTORY | SIZE_KNOWN) == SIZE_KNOWN);
            counts.3 -= usize::from(old.flags & rustsearch_core::REPARSE != 0);
        }
        if let Some(record) = record {
            counts.0 += 1;
            counts.1 += usize::from(record.flags & DIRECTORY != 0);
            counts.2 += usize::from(record.flags & DIRECTORY == 0 && record.size.is_some());
            counts.3 += usize::from(record.flags & rustsearch_core::REPARSE != 0);
        }
    }
    counts
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
        if !mailbox.scanning(generation) {
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
                        if !mailbox.scanning(generation) {
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
    let mut path_bytes = 0usize;
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
        let bytes = name.len() * 2 + location.len() + path.as_ref().map_or(0, |p| p.len() * 2);
        if path_bytes + bytes > 4 * 1024 * 1024 {
            break;
        }
        path_bytes += bytes;
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
    fn incremental_counts_match_full_counts_after_mixed_commits() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("source.rsidx");
        fixture(&path);
        let mut index = rustsearch_storage::load(&path).unwrap();
        let make = |id, parent, flags, size| {
            Some(Record {
                id,
                parent,
                flags,
                size,
                usn: 0,
                name: format!("item-{id}").encode_utf16().collect(),
            })
        };
        let mut records: std::collections::BTreeMap<_, _> =
            index.live_entries().map(|e| (e.id, None)).collect();
        records.extend([
            (1000, make(1000, 1, DIRECTORY, None)),
            (1001, make(1001, 1000, 0, Some(0))),
            (1002, make(1002, 1000, 0, None)),
            (
                1003,
                make(1003, 1, DIRECTORY | rustsearch_core::REPARSE, None),
            ),
        ]);
        for records in [
            records,
            std::collections::BTreeMap::from([
                (1001, None),
                (1002, make(1002, 1, 0, Some(17))),
                (1003, make(1003, 1, 0, Some(3))),
                (1004, make(1004, 1, DIRECTORY, None)),
                (1005, make(1005, 1004, 0, Some(0))),
            ]),
        ] {
            let delta = rustsearch_core::Delta {
                previous_sequence: index.log_sequence,
                previous_checkpoint: index.metadata.checkpoint.clone(),
                metadata: index.metadata.clone(),
                records,
            };
            let counts = delta_counts(&index, &delta, index.counts());
            index.apply_delta(delta).unwrap();
            assert_eq!(counts, index.counts());
        }
    }

    #[test]
    fn frequent_searches_do_not_starve_live_notifications() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source");
        let data = dir.path().join("data");
        std::fs::create_dir(&source).unwrap();
        let control = Arc::new(crate::live::Control::default());
        control.sizes.store(true, Ordering::Release);
        let engine = Engine::new_live(data.clone(), vec![source.clone()], control.clone());
        let rx = replies(&engine);
        let query = SearchSpec {
            text: "late.txt".into(),
            ..search()
        };
        engine.submit(
            Request::ScanFolder {
                root: source.clone(),
                output: data.join("source.rsidx"),
                generation: 1,
                search: query.clone(),
            },
            1,
        );
        assert!(matches!(reply(&rx, 1), Event::Results { total: 0, .. }));
        control.paused.store(true, Ordering::Release);
        engine.submit(
            Request::Search {
                generation: 2,
                search: query.clone(),
            },
            2,
        );
        assert!(matches!(reply(&rx, 2), Event::Results { total: 0, .. }));
        std::fs::write(source.join("late.txt"), b"late").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        control.paused.store(false, Ordering::Release);
        let mut observed = false;
        for generation in 3..103 {
            engine.submit(
                Request::Search {
                    generation,
                    search: query.clone(),
                },
                generation,
            );
            if matches!(reply(&rx, generation), Event::Results { total: 1, .. }) {
                observed = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(
            observed,
            "queries faster than the poll timeout must not block live updates"
        );
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
