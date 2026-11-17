use crate::{
    engine::{Engine, Event, Request, ScanMode, SearchSpec},
    live::Control,
};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::{
    fs::File,
    io::{Read, Write},
    net::{Shutdown, TcpListener, TcpStream},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const MAX_FRAME: usize = 32 * 1024 * 1024;
static DATA_DIRECTORY: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
pub fn set_data_directory(root: PathBuf) {
    let _ = DATA_DIRECTORY.set(root);
}
pub fn data_directory() -> PathBuf {
    DATA_DIRECTORY.get().cloned().unwrap_or_else(|| {
        std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir)
            .join("RustSearch")
    })
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Profile {
    pub background: bool,
    pub paused: bool,
    pub auto_index: bool,
    pub start_at_login: bool,
    pub sizes: bool,
    pub mode: ScanMode,
    pub roots: Vec<PathBuf>,
    pub catalog: Option<PathBuf>,
}
impl Default for Profile {
    fn default() -> Self {
        Self {
            background: true,
            paused: false,
            auto_index: true,
            start_at_login: false,
            sizes: false,
            mode: ScanMode::Accessible,
            roots: vec![],
            catalog: None,
        }
    }
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Runtime {
    pub running: bool,
    pub profile: Profile,
    pub phase: String,
    pub error: Option<String>,
    pub updated_ms: u64,
}
#[derive(Serialize, Deserialize)]
pub enum Rpc {
    Request(Request),
    Current(u64),
    Configure(Profile),
    Status,
    Stop,
}
#[derive(Serialize, Deserialize)]
struct Endpoint {
    port: u16,
    token: String,
    pid: u32,
}
#[derive(Serialize, Deserialize)]
struct Auth {
    token: String,
}
pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
pub fn profile(root: &Path) -> Profile {
    std::fs::read(root.join("background.json"))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}
pub fn runtime(root: &Path) -> Runtime {
    std::fs::read(root.join("status.json"))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}
fn save<T: Serialize>(root: &Path, name: &str, value: &T) -> Result<()> {
    std::fs::create_dir_all(root)?;
    rustsearch_storage::write_derived_atomic(&root.join(name), |f| {
        serde_json::to_writer_pretty(f, value)?;
        Ok(())
    })
}
pub fn send<T: Serialize>(stream: &mut TcpStream, value: &T) -> Result<()> {
    let bytes = serde_json::to_vec(value)?;
    ensure!(bytes.len() <= MAX_FRAME, "worker message too large");
    stream.write_all(&(bytes.len() as u32).to_le_bytes())?;
    stream.write_all(&bytes)?;
    Ok(())
}
pub fn receive<T: DeserializeOwned>(stream: &mut TcpStream) -> Result<T> {
    receive_limited(stream, MAX_FRAME)
}
fn receive_limited<T: DeserializeOwned>(stream: &mut TcpStream, limit: usize) -> Result<T> {
    let mut length = [0; 4];
    stream.read_exact(&mut length)?;
    let length = u32::from_le_bytes(length) as usize;
    ensure!(length <= limit, "worker message too large");
    let mut bytes = vec![0; length];
    stream.read_exact(&mut bytes)?;
    Ok(serde_json::from_slice(&bytes)?)
}
fn connect(root: &Path) -> Result<TcpStream> {
    let file = File::open(root.join("endpoint.json"))?;
    ensure!(file.metadata()?.len() < 4096, "invalid worker endpoint");
    let endpoint: Endpoint = serde_json::from_reader(file)?;
    let mut stream = TcpStream::connect_timeout(
        &std::net::SocketAddr::from(([127, 0, 0, 1], endpoint.port)),
        Duration::from_secs(1),
    )?;
    stream.set_nodelay(true)?;
    stream.set_read_timeout(Some(Duration::from_secs(3)))?;
    stream.set_write_timeout(Some(Duration::from_secs(3)))?;
    send(
        &mut stream,
        &Auth {
            token: endpoint.token,
        },
    )?;
    let accepted: bool = receive(&mut stream)?;
    ensure!(accepted, "worker authentication failed");
    stream.set_read_timeout(None)?;
    Ok(stream)
}
pub fn connect_or_start(root: &Path) -> Result<TcpStream> {
    if let Ok(stream) = connect(root) {
        return Ok(stream);
    }
    use std::os::windows::process::CommandExt;
    std::process::Command::new(std::env::current_exe()?)
        .arg("--worker")
        .arg("--data-dir")
        .arg(root)
        .creation_flags(0x08000000)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .context("start background indexer")?;
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Ok(stream) = connect(root) {
            return Ok(stream);
        }
        ensure!(
            Instant::now() < deadline,
            "background indexer did not start"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}
pub fn command(root: &Path, rpc: Rpc) -> Result<()> {
    let mut stream = connect_or_start(root)?;
    send(&mut stream, &rpc)
}
pub fn command_state(root: &Path, rpc: Rpc, start: bool) -> Result<Runtime> {
    let mut stream = if start {
        connect_or_start(root)?
    } else {
        connect(root)?
    };
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    send(&mut stream, &rpc)?;
    loop {
        if let Event::Runtime(state) = receive(&mut stream)? {
            return Ok(state);
        }
    }
}
pub fn windows_argument(value: &str) -> String {
    let mut quoted = String::from("\"");
    let mut slashes = 0;
    for c in value.chars() {
        if c == '\\' {
            slashes += 1;
            continue;
        }
        if c == '"' {
            quoted.extend(std::iter::repeat_n('\\', slashes * 2 + 1));
        } else {
            quoted.extend(std::iter::repeat_n('\\', slashes));
        }
        slashes = 0;
        quoted.push(c);
    }
    quoted.extend(std::iter::repeat_n('\\', slashes * 2));
    quoted.push('"');
    quoted
}
fn startup(enabled: bool, root: &Path) -> Result<()> {
    startup_at(
        enabled,
        root,
        "Software\\Microsoft\\Windows\\CurrentVersion\\Run",
        "RustSearch",
    )
}
fn startup_at(enabled: bool, root: &Path, registry_path: &str, value_name: &str) -> Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::System::Registry::*;
    let wide = |s: &std::ffi::OsStr| s.encode_wide().chain(Some(0)).collect::<Vec<_>>();
    let subkey = wide(std::ffi::OsStr::new(registry_path));
    let name = wide(std::ffi::OsStr::new(value_name));
    let mut key = core::ptr::null_mut();
    // SAFETY: current-user key, terminated strings, no elevated or machine-wide settings.
    unsafe {
        let result = RegCreateKeyExW(
            HKEY_CURRENT_USER,
            subkey.as_ptr(),
            0,
            core::ptr::null(),
            0,
            KEY_SET_VALUE,
            core::ptr::null(),
            &mut key,
            core::ptr::null_mut(),
        );
        ensure!(result == 0, "open startup settings: Windows error {result}");
        let result = if enabled {
            let value = format!(
                "{} --worker --data-dir {}",
                windows_argument(&std::env::current_exe()?.to_string_lossy()),
                windows_argument(&root.to_string_lossy())
            );
            let data = wide(std::ffi::OsStr::new(&value));
            RegSetValueExW(
                key,
                name.as_ptr(),
                0,
                REG_SZ,
                data.as_ptr().cast(),
                (data.len() * 2) as u32,
            )
        } else {
            let result = RegDeleteValueW(key, name.as_ptr());
            if result == 2 { 0 } else { result }
        };
        RegCloseKey(key);
        ensure!(result == 0, "save startup settings: Windows error {result}");
    }
    Ok(())
}
fn token() -> Result<String> {
    use windows_sys::Win32::Security::Cryptography::*;
    let mut bytes = [0u8; 32];
    // SAFETY: writable byte array, system random provider, synchronous call.
    ensure!(
        unsafe {
            BCryptGenRandom(
                core::ptr::null_mut(),
                bytes.as_mut_ptr(),
                bytes.len() as u32,
                BCRYPT_USE_SYSTEM_PREFERRED_RNG,
            )
        } >= 0,
        "generate worker credential"
    );
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}
fn valid_token(candidate: &str, secret: &str) -> bool {
    if candidate.len() != 64 || secret.len() != 64 {
        return false;
    }
    candidate
        .bytes()
        .zip(secret.bytes())
        .fold(0u8, |diff, (a, b)| diff | (a ^ b))
        == 0
}
/// Remove only recognized, older app-owned generations; retain current and previous publication.
fn prune_generations(root: &Path, current: &Path, previous: &Path) {
    use std::os::windows::fs::MetadataExt;
    let Ok(canonical_root) = root.canonicalize() else {
        return;
    };
    let keep = [
        current.parent().and_then(|p| p.canonicalize().ok()),
        previous.parent().and_then(|p| p.canonicalize().ok()),
    ];
    let Ok(directories) = std::fs::read_dir(root) else {
        return;
    };
    for entry in directories.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if !name
            .strip_prefix("managed-")
            .is_some_and(|s| !s.is_empty() && s.bytes().all(|c| c.is_ascii_digit()))
        {
            continue;
        }
        let Ok(metadata) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if !metadata.is_dir() || metadata.file_attributes() & 0x400 != 0 {
            continue;
        }
        let Ok(resolved) = path.canonicalize() else {
            continue;
        };
        if resolved.parent() != Some(canonical_root.as_path())
            || keep.iter().flatten().any(|p| p == &resolved)
        {
            continue;
        }
        let Ok(files) = std::fs::read_dir(&resolved) else {
            continue;
        };
        let files: Vec<_> = files.flatten().collect();
        let safe = files.iter().all(|f| {
            let name = f.file_name();
            let Some(name) = name.to_str() else {
                return false;
            };
            let recognized = name == "computer.rscatalog"
                || name.strip_prefix("drive-").is_some_and(|s| {
                    let digits = s.bytes().take_while(|b| b.is_ascii_digit()).count();
                    if digits == 0 {
                        return false;
                    }
                    matches!(
                        &s[digits..],
                        ".rsidx"
                            | ".rsidx.search"
                            | ".rsidx.delta"
                            | ".rsidx.lock"
                            | ".rsidx.search.lock"
                    )
                });
            recognized
                && f.file_type().is_ok_and(|t| t.is_file())
                && f.metadata().is_ok_and(|m| m.file_attributes() & 0x400 == 0)
        });
        if !safe {
            continue;
        }
        for file in files {
            let _ = std::fs::remove_file(file.path());
        }
        let _ = std::fs::remove_dir(resolved);
    }
}
struct ConnectionSlot(Arc<AtomicUsize>);
impl Drop for ConnectionSlot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}
fn generation(request: &mut Request, generation: u64) {
    match request {
        Request::Open { generation: g, .. }
        | Request::ScanFolder { generation: g, .. }
        | Request::ScanComputer { generation: g, .. }
        | Request::Search { generation: g, .. }
        | Request::Current { generation: g } => *g = generation,
        Request::Cancel => {}
    }
}
pub fn retag(event: &mut Event, generation: u64) {
    match event {
        Event::Results { generation: g, .. }
        | Event::Progress { generation: g, .. }
        | Event::Error { generation: g, .. } => *g = generation,
        Event::Runtime(_) => {}
    }
}
fn request_generation(request: &Request) -> u64 {
    match request {
        Request::Open { generation, .. }
        | Request::ScanFolder { generation, .. }
        | Request::ScanComputer { generation, .. }
        | Request::Search { generation, .. }
        | Request::Current { generation } => *generation,
        Request::Cancel => 0,
    }
}
struct Client {
    tx: async_channel::Sender<Event>,
    drain: async_channel::Receiver<Event>,
    generation: AtomicU64,
}
impl Client {
    fn deliver(&self, mut event: Event) {
        retag(&mut event, self.generation.load(Ordering::Acquire));
        if let Err(async_channel::TrySendError::Full(event)) = self.tx.try_send(event) {
            let _ = self.drain.try_recv();
            let _ = self.tx.try_send(event);
        }
    }
}
pub fn run(root: PathBuf) -> Result<()> {
    std::fs::create_dir_all(&root)?;
    let _guard = match rustsearch_storage::WriteGuard::acquire(&root.join("worker")) {
        Ok(g) => g,
        Err(_) => return Ok(()),
    };
    let mut config = profile(&root);
    if config.roots.is_empty() && config.auto_index {
        config.roots = rustsearch_windows::local_drives()?
            .into_iter()
            .filter(|d| !d.removable && d.error.is_none())
            .map(|d| PathBuf::from(d.root))
            .collect();
        save(&root, "background.json", &config)?;
    }
    let control = Arc::new(Control::default());
    control.paused.store(config.paused, Ordering::Release);
    control.sizes.store(config.sizes, Ordering::Release);
    let engine = Arc::new(Engine::new_live(
        root.clone(),
        config.roots.clone(),
        control.clone(),
    ));
    let runtime = Arc::new(Mutex::new(Runtime {
        running: true,
        profile: config.clone(),
        phase: "Starting".into(),
        error: None,
        updated_ms: now_ms(),
    }));
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))?;
    listener.set_nonblocking(true)?;
    let secret = token()?;
    save(
        &root,
        "endpoint.json",
        &Endpoint {
            port: listener.local_addr()?.port(),
            token: secret.clone(),
            pid: std::process::id(),
        },
    )?;
    let clients: Arc<Mutex<Vec<Arc<Client>>>> = Arc::new(Mutex::new(vec![]));
    let latest: Arc<Mutex<Option<Event>>> = Arc::new(Mutex::new(None));
    let active = Arc::new(AtomicUsize::new(0));
    let stop = Arc::new(AtomicBool::new(false));
    let sequence = Arc::new(AtomicU64::new(1));
    let rx = engine.events.clone();
    let broadcast = clients.clone();
    let last = latest.clone();
    let state = runtime.clone();
    let state_root = root.clone();
    let current_sequence = sequence.clone();
    let reuse_saved = config.auto_index
        && !config.paused
        && !config.roots.is_empty()
        && config.catalog.as_ref().is_some_and(|path| path.is_file());
    let startup_reuse = Arc::new(AtomicBool::new(reuse_saved));
    let reused = startup_reuse.clone();
    let fallback_engine = Arc::downgrade(&engine);
    std::thread::spawn(move || {
        let mut revision = None;
        while let Ok(event) = rx.recv_blocking() {
            let event_generation = match &event {
                Event::Results { generation, .. }
                | Event::Progress { generation, .. }
                | Event::Error { generation, .. } => *generation,
                Event::Runtime(_) => continue,
            };
            if event_generation != current_sequence.load(Ordering::Acquire) {
                continue;
            }
            if matches!(&event, Event::Error { .. }) && reused.swap(false, Ordering::AcqRel) {
                let config = state
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .profile
                    .clone();
                let generation = current_sequence.fetch_add(1, Ordering::AcqRel) + 1;
                if !config.auto_index || config.paused {
                    continue;
                }
                let Some(fallback_engine) = fallback_engine.upgrade() else {
                    break;
                };
                fallback_engine.submit(
                    Request::ScanComputer {
                        roots: config.roots,
                        directory: state_root.join(format!("managed-{}", now_ms())),
                        mode: config.mode,
                        sizes: config.sizes,
                        generation,
                        search: SearchSpec {
                            limit: 500,
                            ..Default::default()
                        },
                    },
                    generation,
                );
                continue;
            }
            if matches!(&event, Event::Results { .. }) {
                reused.store(false, Ordering::Release);
            }
            let mut changed = true;
            let mut obsolete = None;
            {
                let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
                match &event {
                    Event::Progress { stage, .. } => state.phase = stage.clone(),
                    Event::Results { source, .. } => {
                        let next = (
                            source.path.clone(),
                            source.sequence,
                            source.records,
                            source.pending,
                            source.recovering,
                        );
                        changed = revision.as_ref() != Some(&next);
                        revision = Some(next);
                        state.phase = if source.recovering {
                            "Checking saved files"
                        } else if source.live {
                            "Monitoring changes"
                        } else {
                            "Read-only snapshot"
                        }
                        .into();
                        state.error = None;
                        if source.live && state.profile.catalog.as_ref() != Some(&source.path) {
                            obsolete = state.profile.catalog.replace(source.path.clone());
                            let _ = save(&state_root, "background.json", &state.profile);
                        }
                    }
                    Event::Error { message, .. } => {
                        state.phase = "Needs attention".into();
                        state.error = Some(message.clone());
                    }
                    Event::Runtime(_) => {}
                }
                if changed {
                    state.updated_ms = now_ms();
                    let _ = save(&state_root, "status.json", &*state);
                }
            }
            if let Some(previous) = obsolete
                && let Event::Results { source, .. } = &event
            {
                prune_generations(&state_root, &source.path, &previous);
            }
            *last.lock().unwrap_or_else(|e| e.into_inner()) = Some(event.clone());
            broadcast
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .retain(|c| !c.tx.is_closed());
            let status = state.lock().unwrap_or_else(|e| e.into_inner()).clone();
            for client in broadcast.lock().unwrap_or_else(|e| e.into_inner()).iter() {
                if changed {
                    client.deliver(Event::Runtime(status.clone()));
                }
                client.deliver(event.clone());
            }
        }
    });
    if config.auto_index && !config.paused && !config.roots.is_empty() {
        let search = SearchSpec {
            limit: 500,
            ..Default::default()
        };
        // Reuse the immutable source and reconcile watcher downtime in bounded batches.
        let request = if reuse_saved {
            Request::Open {
                path: config.catalog.clone().unwrap(),
                generation: 1,
                search,
            }
        } else {
            Request::ScanComputer {
                roots: config.roots,
                directory: root.join(format!("managed-{}", now_ms())),
                mode: config.mode,
                sizes: config.sizes,
                generation: 1,
                search,
            }
        };
        engine.submit(request, 1);
    }
    let connections = Arc::new(AtomicUsize::new(0));
    let started = Instant::now();
    while !stop.load(Ordering::Acquire) {
        match listener.accept() {
            Ok((mut stream, _)) => {
                if connections.fetch_add(1, Ordering::AcqRel) >= 8 {
                    connections.fetch_sub(1, Ordering::AcqRel);
                    let _ = stream.shutdown(Shutdown::Both);
                    continue;
                }
                let slot = ConnectionSlot(connections.clone());
                let secret = secret.clone();
                let engine = engine.clone();
                let clients = clients.clone();
                let latest = latest.clone();
                let runtime = runtime.clone();
                let root = root.clone();
                let control = control.clone();
                let stop = stop.clone();
                let active = active.clone();
                let sequence = sequence.clone();
                let startup_reuse = startup_reuse.clone();
                std::thread::spawn(move || {
                    let _slot = slot;
                    let serve = (|| -> Result<()> {
                        // Windows accepted sockets inherit the listener's nonblocking mode.
                        // Each connection handler uses bounded blocking frame reads instead.
                        stream.set_nonblocking(false)?;
                        stream.set_nodelay(true)?;
                        stream.set_read_timeout(Some(Duration::from_secs(3)))?;
                        stream.set_write_timeout(Some(Duration::from_secs(5)))?;
                        let auth: Auth = receive_limited(&mut stream, 2048)?;
                        ensure!(
                            valid_token(&auth.token, &secret),
                            "invalid worker credential"
                        );
                        send(&mut stream, &true)?;
                        stream.set_read_timeout(None)?;
                        let (tx, rx) = async_channel::bounded(4);
                        let client = Arc::new(Client {
                            tx,
                            drain: rx.clone(),
                            generation: AtomicU64::new(0),
                        });
                        let mut outgoing = stream.try_clone()?;
                        std::thread::spawn(move || {
                            while let Ok(event) = rx.recv_blocking() {
                                if send(&mut outgoing, &event).is_err() {
                                    break;
                                }
                            }
                            let _ = outgoing.shutdown(Shutdown::Both);
                        });
                        clients
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .push(client.clone());
                        active.fetch_add(1, Ordering::AcqRel);
                        loop {
                            let rpc: Rpc = match receive(&mut stream) {
                                Ok(r) => r,
                                Err(_) => break,
                            };
                            match rpc {
                                Rpc::Current(local) => {
                                    client.generation.store(local, Ordering::Release);
                                    client.deliver(Event::Runtime(
                                        runtime.lock().unwrap_or_else(|e| e.into_inner()).clone(),
                                    ));
                                    let event =
                                        latest.lock().unwrap_or_else(|e| e.into_inner()).clone();
                                    if matches!(event, Some(Event::Results { .. })) {
                                        let global = sequence.fetch_add(1, Ordering::AcqRel) + 1;
                                        engine.submit(
                                            Request::Search {
                                                generation: global,
                                                search: SearchSpec {
                                                    limit: 500,
                                                    ..Default::default()
                                                },
                                            },
                                            global,
                                        );
                                    } else if let Some(event) = event {
                                        client.deliver(event);
                                    } else {
                                        let state = runtime
                                            .lock()
                                            .unwrap_or_else(|e| e.into_inner())
                                            .clone();
                                        if state.profile.auto_index
                                            && !state.profile.paused
                                            && !state.profile.roots.is_empty()
                                        {
                                            client.deliver(Event::Progress {
                                                generation: local,
                                                records: 0,
                                                stage: "Starting automatic indexing".into(),
                                            });
                                        } else {
                                            client.deliver(Event::Error{generation:local,message:"Indexing is paused or automatic indexing is disabled. Open Settings to resume, or select drives in This computer.".into()});
                                        }
                                    }
                                }
                                Rpc::Request(mut request) => {
                                    if !matches!(&request, Request::Search { .. }) {
                                        startup_reuse.store(false, Ordering::Release);
                                    }
                                    client
                                        .generation
                                        .store(request_generation(&request), Ordering::Release);
                                    if let Request::ScanComputer {
                                        roots, sizes, mode, ..
                                    } = &request
                                    {
                                        control.sizes.store(*sizes, Ordering::Release);
                                        let mut state =
                                            runtime.lock().unwrap_or_else(|e| e.into_inner());
                                        state.profile.roots = roots.clone();
                                        state.profile.sizes = *sizes;
                                        state.profile.mode = *mode;
                                        let _ = save(&root, "background.json", &state.profile);
                                    }
                                    if matches!(&request, Request::Cancel) {
                                        control.paused.store(true, Ordering::Release);
                                        let mut state =
                                            runtime.lock().unwrap_or_else(|e| e.into_inner());
                                        state.profile.paused = true;
                                        state.phase = "Paused".into();
                                        let _ = save(&root, "background.json", &state.profile);
                                        *latest.lock().unwrap_or_else(|e| e.into_inner()) = None;
                                        for c in
                                            clients.lock().unwrap_or_else(|e| e.into_inner()).iter()
                                        {
                                            c.deliver(Event::Runtime(state.clone()));
                                        }
                                    }
                                    let global = sequence.fetch_add(1, Ordering::AcqRel) + 1;
                                    generation(&mut request, global);
                                    engine.submit(request, global);
                                }
                                Rpc::Configure(mut profile) => {
                                    if !profile.auto_index || profile.paused {
                                        startup_reuse.store(false, Ordering::Release);
                                    }
                                    let mut state =
                                        runtime.lock().unwrap_or_else(|e| e.into_inner());
                                    if !profile.background && state.profile.background {
                                        profile.start_at_login = false;
                                    }
                                    if profile.start_at_login {
                                        profile.background = true;
                                    }
                                    if profile.start_at_login != state.profile.start_at_login
                                        && let Err(e) = startup(profile.start_at_login, &root)
                                    {
                                        state.error = Some(format!("Startup: {e:#}"));
                                        profile.start_at_login = state.profile.start_at_login;
                                    }
                                    profile.catalog = state.profile.catalog.clone();
                                    profile.roots = state.profile.roots.clone();
                                    let was_paused = state.profile.paused;
                                    control.paused.store(profile.paused, Ordering::Release);
                                    control.sizes.store(profile.sizes, Ordering::Release);
                                    state.profile = profile;
                                    state.updated_ms = now_ms();
                                    let _ = save(&root, "background.json", &state.profile);
                                    let _ = save(&root, "status.json", &*state);
                                    for c in
                                        clients.lock().unwrap_or_else(|e| e.into_inner()).iter()
                                    {
                                        c.deliver(Event::Runtime(state.clone()));
                                    }
                                    if was_paused
                                        && !state.profile.paused
                                        && latest
                                            .lock()
                                            .unwrap_or_else(|e| e.into_inner())
                                            .is_none()
                                        && !state.profile.roots.is_empty()
                                    {
                                        let global = sequence.fetch_add(1, Ordering::AcqRel) + 1;
                                        engine.submit(
                                            Request::ScanComputer {
                                                roots: state.profile.roots.clone(),
                                                directory: root
                                                    .join(format!("managed-{}", now_ms())),
                                                mode: state.profile.mode,
                                                sizes: state.profile.sizes,
                                                generation: global,
                                                search: SearchSpec {
                                                    limit: 500,
                                                    ..Default::default()
                                                },
                                            },
                                            global,
                                        );
                                    }
                                }
                                Rpc::Status => {
                                    client.deliver(Event::Runtime(
                                        runtime.lock().unwrap_or_else(|e| e.into_inner()).clone(),
                                    ));
                                }
                                Rpc::Stop => {
                                    let mut status =
                                        runtime.lock().unwrap_or_else(|e| e.into_inner()).clone();
                                    status.running = false;
                                    status.phase = "Stopped".into();
                                    client.deliver(Event::Runtime(status));
                                    stop.store(true, Ordering::Release);
                                    break;
                                }
                            }
                        }
                        client.tx.close();
                        active.fetch_sub(1, Ordering::AcqRel);
                        Ok(())
                    })();
                    let _ = serve;
                });
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(50))
            }
            Err(e) => return Err(e.into()),
        }
        if started.elapsed() > Duration::from_secs(5)
            && active.load(Ordering::Acquire) == 0
            && !runtime
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .profile
                .background
        {
            break;
        }
    }
    {
        let mut state = runtime.lock().unwrap_or_else(|e| e.into_inner());
        state.running = false;
        state.phase = "Stopped".into();
        save(&root, "status.json", &*state)?;
    }
    let _ = std::fs::remove_file(root.join("endpoint.json"));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn result(stream: &mut TcpStream, total: usize) -> crate::engine::SourceInfo {
        let deadline = Instant::now() + Duration::from_secs(10);
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        loop {
            assert!(Instant::now() < deadline, "bounded worker reply");
            match receive::<Event>(stream).unwrap() {
                Event::Results {
                    source, total: n, ..
                } if n == total => return (*source).clone(),
                Event::Error { message, .. } => panic!("worker error: {message}"),
                _ => {}
            }
        }
    }
    #[test]
    fn accessible_restart_reuses_saved_catalog_and_reconciles_offline_changes() {
        for corrupt in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let data = dir.path().join("data");
            let source = dir.path().join("source");
            std::fs::create_dir_all(data.join("saved")).unwrap();
            std::fs::create_dir(&source).unwrap();
            std::fs::write(source.join("before.txt"), b"before").unwrap();
            let (mut index, _) = rustsearch_windows::walk(&source, false).unwrap();
            let snapshot = data.join("saved/drive-0.rsidx");
            index.snapshot_identity = Some(rustsearch_storage::save(&snapshot, &index).unwrap());
            drop(rustsearch_query::SearchIndex::prepare(&index, &snapshot, true).0);
            let catalog = data.join("saved/computer.rscatalog");
            crate::catalog::Catalog {
                version: 1,
                snapshots: vec![snapshot.clone()],
                warnings: vec![],
            }
            .save(&catalog)
            .unwrap();
            if corrupt {
                let mut bytes = std::fs::read(&snapshot).unwrap();
                *bytes.last_mut().unwrap() ^= 1;
                std::fs::write(&snapshot, bytes).unwrap();
            }
            let base = std::fs::read(&snapshot).unwrap();
            let cache = std::fs::read(rustsearch_query::cache_path(&snapshot)).unwrap();
            std::fs::write(source.join("offline.txt"), b"offline").unwrap();
            save(
                &data,
                "background.json",
                &Profile {
                    roots: vec![source],
                    catalog: Some(catalog.clone()),
                    ..Default::default()
                },
            )
            .unwrap();
            let root = data.clone();
            let (done_tx, done_rx) = std::sync::mpsc::channel();
            let server = std::thread::spawn(move || {
                let _ = done_tx.send(run(root));
            });
            let end = Instant::now() + Duration::from_secs(10);
            let mut client = loop {
                if let Ok(client) = connect(&data) {
                    break client;
                }
                assert!(Instant::now() < end, "worker startup timed out");
                std::thread::sleep(Duration::from_millis(10));
            };
            send(&mut client, &Rpc::Current(1)).unwrap();
            let found = result(&mut client, 2);
            client.shutdown(Shutdown::Both).unwrap();
            command_state(&data, Rpc::Stop, false).unwrap();
            done_rx
                .recv_timeout(Duration::from_secs(10))
                .unwrap()
                .unwrap();
            server.join().unwrap();
            if corrupt {
                assert_ne!(
                    found.path, catalog,
                    "corrupt saved source must fall back once"
                );
            } else {
                assert_eq!(
                    found.path, catalog,
                    "restart must catch up the saved source instead of replacing its catalog"
                );
            }
            assert_eq!(std::fs::read(&snapshot).unwrap(), base);
            assert_eq!(
                std::fs::read(rustsearch_query::cache_path(&snapshot)).unwrap(),
                cache
            );
        }
    }

    #[test]
    fn background_worker_authenticates_updates_while_gui_is_closed_and_pauses() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("data");
        let source = dir.path().join("source");
        std::fs::create_dir(&source).unwrap();
        std::fs::write(source.join("alpha.txt"), b"one").unwrap();
        let config = Profile {
            roots: vec![source.clone()],
            sizes: true,
            ..Default::default()
        };
        save(&data, "background.json", &config).unwrap();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let root = data.clone();
        let server = std::thread::spawn(move || {
            let result = run(root);
            let _ = done_tx.send(result);
        });
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut client = loop {
            if let Ok(stream) = connect(&data) {
                break stream;
            }
            assert!(Instant::now() < deadline, "server did not start");
            std::thread::sleep(Duration::from_millis(10));
        };
        send(&mut client, &Rpc::Current(1)).unwrap();
        let info = result(&mut client, 1);
        let catalog = crate::catalog::Catalog::load(&info.path).unwrap();
        let base = std::fs::read(&catalog.snapshots[0]).unwrap();
        let cache = std::fs::read(rustsearch_query::cache_path(&catalog.snapshots[0])).unwrap();
        client.shutdown(Shutdown::Both).unwrap();
        drop(client);
        std::fs::write(source.join("closed-window.txt"), b"background").unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let index = rustsearch_storage::load(&catalog.snapshots[0]).unwrap();
            if index.counts().0 == 2 {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "background changes were not committed"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        let mut client = connect(&data).unwrap();
        send(&mut client, &Rpc::Current(2)).unwrap();
        result(&mut client, 2);
        let mut paused = config.clone();
        paused.paused = true;
        let state = command_state(&data, Rpc::Configure(paused), false).unwrap();
        assert!(state.profile.paused);
        std::fs::write(source.join("resume.txt"), b"after-pause").unwrap();
        send(
            &mut client,
            &Rpc::Request(Request::Search {
                generation: 3,
                search: SearchSpec {
                    text: "resume.txt".into(),
                    limit: 10,
                    ..Default::default()
                },
            }),
        )
        .unwrap();
        result(&mut client, 0);
        let state = command_state(&data, Rpc::Configure(config), false).unwrap();
        assert!(!state.profile.paused);
        result(&mut client, 1);
        let mut pressure = profile(&data);
        pressure.paused = true;
        assert!(
            command_state(&data, Rpc::Configure(pressure.clone()), false)
                .unwrap()
                .profile
                .paused
        );
        for number in 0..4200 {
            std::fs::write(source.join(format!("burst-{number}.txt")), b"").unwrap();
        }
        pressure.paused = false;
        command_state(&data, Rpc::Configure(pressure), false).unwrap();
        send(
            &mut client,
            &Rpc::Request(Request::Search {
                generation: 4,
                search: SearchSpec {
                    text: "burst-".into(),
                    limit: 10,
                    ..Default::default()
                },
            }),
        )
        .unwrap();
        let recovered = result(&mut client, 4200);
        assert_eq!(
            recovered.path, info.path,
            "queue pressure must not replace the published source"
        );
        assert_eq!(recovered.records, 4203);
        let endpoint: Endpoint =
            serde_json::from_slice(&std::fs::read(data.join("endpoint.json")).unwrap()).unwrap();
        let mut bad = TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, endpoint.port)).unwrap();
        bad.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        send(
            &mut bad,
            &Auth {
                token: "wrong".into(),
            },
        )
        .unwrap();
        assert!(receive::<bool>(&mut bad).is_err());
        assert_eq!(std::fs::read(&catalog.snapshots[0]).unwrap(), base);
        assert_eq!(
            std::fs::read(rustsearch_query::cache_path(&catalog.snapshots[0])).unwrap(),
            cache
        );
        client.shutdown(Shutdown::Both).unwrap();
        drop(client);
        assert!(!command_state(&data, Rpc::Stop, false).unwrap().running);
        done_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("bounded worker shutdown")
            .unwrap();
        server.join().unwrap();
    }
    #[test]
    fn worker_frames_reject_oversize_before_allocating_and_quote_windows_paths() {
        assert_eq!(
            windows_argument("C:\\Program Files\\RustSearch\\"),
            "\"C:\\Program Files\\RustSearch\\\\\""
        );
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let peer = std::thread::spawn(move || {
            let mut stream = TcpStream::connect(address).unwrap();
            stream
                .write_all(&((MAX_FRAME + 1) as u32).to_le_bytes())
                .unwrap();
        });
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        assert!(
            receive::<Auth>(&mut stream)
                .err()
                .unwrap()
                .to_string()
                .contains("too large")
        );
        peer.join().unwrap();
    }
    #[test]
    fn startup_value_roundtrips_in_an_isolated_current_user_key() {
        use windows_sys::Win32::System::Registry::*;
        let root = tempfile::tempdir().unwrap();
        let key = format!("Software\\RustSearch\\Acceptance\\{}", std::process::id());
        let wide = |s: &str| s.encode_utf16().chain(Some(0)).collect::<Vec<_>>();
        let path = wide(&key);
        let name = wide("StartupProof");
        startup_at(true, root.path(), &key, "StartupProof").unwrap();
        let mut data = [0u16; 4096];
        let mut size = (data.len() * 2) as u32;
        // SAFETY: isolated per-process test key; buffers match declared byte lengths.
        let status = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                path.as_ptr(),
                name.as_ptr(),
                RRF_RT_REG_SZ,
                core::ptr::null_mut(),
                data.as_mut_ptr().cast(),
                &mut size,
            )
        };
        assert_eq!(status, 0);
        let text = String::from_utf16_lossy(&data[..size as usize / 2 - 1]);
        assert!(text.contains("--worker --data-dir"));
        assert!(text.contains(&root.path().to_string_lossy().into_owned()));
        startup_at(false, root.path(), &key, "StartupProof").unwrap();
        let status = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                path.as_ptr(),
                name.as_ptr(),
                RRF_RT_REG_SZ,
                core::ptr::null_mut(),
                data.as_mut_ptr().cast(),
                &mut size,
            )
        };
        assert_eq!(status, 2);
        unsafe {
            RegDeleteKeyW(HKEY_CURRENT_USER, path.as_ptr());
        }
    }
    #[test]
    fn generation_cleanup_retains_two_publications_and_skips_foreign_files() {
        let root = tempfile::tempdir().unwrap();
        for name in ["managed-1", "managed-2", "managed-3", "managed-4"] {
            let dir = root.path().join(name);
            std::fs::create_dir(&dir).unwrap();
            std::fs::write(dir.join("computer.rscatalog"), b"fixture").unwrap();
            std::fs::write(dir.join("drive-0.rsidx"), b"snapshot").unwrap();
        }
        std::fs::write(root.path().join("managed-4/user-document.txt"), b"preserve").unwrap();
        prune_generations(
            root.path(),
            &root.path().join("managed-3/computer.rscatalog"),
            &root.path().join("managed-2/computer.rscatalog"),
        );
        assert!(!root.path().join("managed-1").exists());
        assert!(root.path().join("managed-2").exists());
        assert!(root.path().join("managed-3").exists());
        assert_eq!(
            std::fs::read(root.path().join("managed-4/user-document.txt")).unwrap(),
            b"preserve"
        );
    }
}
