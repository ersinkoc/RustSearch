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
    Search {
        generation: u64,
        search: SearchSpec,
    },
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
}

#[derive(Clone, Debug)]
pub struct Row {
    pub id: u64,
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

/// A single index-owning worker, one replaceable pending request, and one pending reply.
/// UI state receives only bounded result rows and source statistics, never an index copy.
pub struct Engine {
    mailbox: Arc<Mailbox>,
    pub events: async_channel::Receiver<Event>,
}
impl Default for Engine {
    fn default() -> Self {
        Self::new()
    }
}
impl Engine {
    pub fn new() -> Self {
        let mailbox = Arc::new(Mailbox::default());
        let (tx, rx) = async_channel::bounded(1);
        let worker_mailbox = mailbox.clone();
        let drain = rx.clone();
        std::thread::Builder::new()
            .name("rustsearch-index".into())
            .spawn(move || {
                let mut next = worker_mailbox.take();
                while let Some(request) = next {
                    let load_start = Instant::now();
                    let (path, generation, search, loaded) = match request {
                        Request::Open {
                            path,
                            generation,
                            search,
                        } => {
                            let loaded = rustsearch_storage::load(&path);
                            (path, generation, search, loaded)
                        }
                        Request::ScanFolder {
                            root,
                            output,
                            generation,
                            search,
                        } => {
                            let loaded = (|| -> Result<Index> {
                                std::fs::create_dir_all(
                                    output.parent().context("missing index directory")?,
                                )?;
                                let mut pulse = Instant::now();
                                let (mut index, _) =
                                    rustsearch_windows::walk_controlled(&root, true, |records| {
                                        if !worker_mailbox.current(generation) {
                                            return false;
                                        }
                                        if pulse.elapsed().as_millis() >= 200 {
                                            send_latest(
                                                Event::Progress {
                                                    generation,
                                                    records,
                                                },
                                                &tx,
                                                &drain,
                                            );
                                            pulse = Instant::now();
                                        }
                                        true
                                    })?;
                                ensure!(
                                    worker_mailbox.current(generation),
                                    "Folder indexing cancelled"
                                );
                                index.consolidate()?;
                                ensure!(
                                    worker_mailbox.current(generation),
                                    "Folder indexing cancelled"
                                );
                                index.snapshot_identity =
                                    Some(rustsearch_storage::save(&output, &index)?);
                                Ok(index)
                            })();
                            (output, generation, search, loaded)
                        }
                        Request::Search { .. } | Request::Cancel => {
                            next = worker_mailbox.take();
                            continue;
                        }
                    };
                    match loaded {
                        Ok(index) => {
                            if !worker_mailbox.current(generation) {
                                next = worker_mailbox.take();
                                continue;
                            }
                            let load_ms = load_start.elapsed().as_secs_f64() * 1000.;
                            let start = Instant::now();
                            let (prepared, cache) = SearchIndex::prepare(&index, &path, true);
                            let prepare_ms = start.elapsed().as_secs_f64() * 1000.;
                            let (records, folders, sized_files, _) = index.counts();
                            let info = Arc::new(SourceInfo {
                                path,
                                root: index.metadata.root.clone(),
                                backend: index.metadata.source.clone(),
                                records,
                                files: records - folders,
                                folders,
                                sized_files,
                                pending: index.metadata.pending_refresh.len(),
                                sequence: index.log_sequence,
                                memory_bytes: index.allocated_bytes() + prepared.allocated_bytes(),
                                load_ms,
                                prepare_ms,
                                cache_state: cache.state.into(),
                            });
                            respond(
                                &index,
                                &prepared,
                                info.clone(),
                                generation,
                                search,
                                &worker_mailbox,
                                (&tx, &drain),
                            );
                            loop {
                                match worker_mailbox.take() {
                                    Some(Request::Search { generation, search }) => {
                                        respond(
                                            &index,
                                            &prepared,
                                            info.clone(),
                                            generation,
                                            search,
                                            &worker_mailbox,
                                            (&tx, &drain),
                                        );
                                    }
                                    other => {
                                        next = other;
                                        break;
                                    }
                                }
                            }
                            // prepared and index drop here before another source is loaded.
                        }
                        Err(error) => {
                            if worker_mailbox.current(generation) {
                                send_latest(
                                    Event::Error {
                                        generation,
                                        message: format!("{error:#}"),
                                    },
                                    &tx,
                                    &drain,
                                );
                            }
                            next = worker_mailbox.take();
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
fn send_latest(
    event: Event,
    tx: &async_channel::Sender<Event>,
    drain: &async_channel::Receiver<Event>,
) {
    if let Err(async_channel::TrySendError::Full(event)) = tx.try_send(event) {
        let _ = drain.try_recv();
        let _ = tx.try_send(event);
    }
}
fn respond(
    index: &Index,
    prepared: &SearchIndex<'_>,
    source: Arc<SourceInfo>,
    generation: u64,
    search: SearchSpec,
    mailbox: &Mailbox,
    reply: (
        &async_channel::Sender<Event>,
        &async_channel::Receiver<Event>,
    ),
) {
    if !mailbox.current(generation) {
        return;
    }
    let start = Instant::now();
    let result = prepared.execute(&Query {
        text: search.text,
        kind: search.kind,
        mode: search.mode,
        extension: search.extension,
        min_size: search.min_size,
        largest: search.largest,
        limit: search.limit.min(10_000),
    });
    let query_ms = start.elapsed().as_secs_f64() * 1000.;
    let mut rows = Vec::with_capacity(result.rows.len());
    for id in result.rows {
        if !mailbox.current(generation) {
            return;
        }
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
                total: result.total,
                rows,
                query_ms,
            },
            reply.0,
            reply.1,
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
