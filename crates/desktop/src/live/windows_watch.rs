//! One cancellable native reader per root. Buffers are rearmed before event delivery.
use anyhow::{Context, Result, ensure};
use notify::{
    Event, EventKind,
    event::{CreateKind, Flag, ModifyKind, RemoveKind, RenameMode},
};
use std::{
    ffi::OsString,
    os::windows::ffi::{OsStrExt, OsStringExt},
    path::{Path, PathBuf},
    sync::Arc,
};
use windows_sys::Win32::{
    Foundation::*,
    Storage::FileSystem::*,
    System::{
        IO::*,
        Threading::*,
        WindowsProgramming::{DRIVE_FIXED, DRIVE_RAMDISK, DRIVE_REMOVABLE, DRIVE_UNKNOWN},
    },
};

const NETWORK_BUFFER_BYTES: usize = 64 * 1024;
const LOCAL_BUFFER_BYTES: usize = 1024 * 1024;
fn buffer_bytes(root: &Path) -> usize {
    let path: Vec<_> = root.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut volume = vec![0u16; 32768];
    // SAFETY: terminated input and writable output with the declared capacity.
    let kind = unsafe {
        if GetVolumePathNameW(path.as_ptr(), volume.as_mut_ptr(), volume.len() as u32) == 0 {
            DRIVE_UNKNOWN
        } else {
            GetDriveTypeW(volume.as_ptr())
        }
    };
    match kind {
        DRIVE_FIXED | DRIVE_REMOVABLE | DRIVE_RAMDISK => LOCAL_BUFFER_BYTES,
        _ => NETWORK_BUFFER_BYTES,
    }
}
struct Handle(HANDLE);
// SAFETY: owned kernel handles can be transferred between threads; event operations are
// thread-safe. Directory I/O handles are used exclusively by their reader thread.
unsafe impl Send for Handle {}
unsafe impl Sync for Handle {}
impl Drop for Handle {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}
type Receiver = Arc<dyn Fn(Vec<Event>) + Send + Sync>;
pub struct NativeWatch {
    stop: Arc<Handle>,
    thread: Option<std::thread::JoinHandle<()>>,
}
fn open(root: &Path) -> Result<Handle> {
    let wide: Vec<_> = root.as_os_str().encode_wide().chain(Some(0)).collect();
    // SAFETY: terminated path and no retained Rust pointers; the returned handle is owned.
    let handle = unsafe {
        CreateFileW(
            wide.as_ptr(),
            FILE_LIST_DIRECTORY,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            core::ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OVERLAPPED,
            core::ptr::null_mut(),
        )
    };
    ensure!(
        handle != INVALID_HANDLE_VALUE,
        "open notification root: {}",
        std::io::Error::last_os_error()
    );
    Ok(Handle(handle))
}
fn event_handle() -> Result<Handle> {
    let handle = unsafe { CreateEventW(core::ptr::null(), 1, 0, core::ptr::null()) };
    ensure!(
        !handle.is_null(),
        "create notification event: {}",
        std::io::Error::last_os_error()
    );
    Ok(Handle(handle))
}
impl NativeWatch {
    pub fn new(root: PathBuf, receive: Receiver) -> Result<Self> {
        let directory = open(&root)?;
        let stop = Arc::new(event_handle()?);
        let complete = event_handle()?;
        let stopping = stop.clone();
        let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel(1);
        let thread = std::thread::Builder::new()
            .name("rustsearch-notify".into())
            .spawn(move || run(root, directory, stopping, complete, receive, ready_tx))
            .context("start native notification reader")?;
        let ready = ready_rx.recv_timeout(std::time::Duration::from_secs(5));
        if !matches!(&ready, Ok(Ok(()))) {
            unsafe {
                SetEvent(stop.0);
            }
            let _ = thread.join();
            anyhow::bail!("start directory notifications: {ready:?}");
        }
        Ok(Self {
            stop,
            thread: Some(thread),
        })
    }
}
impl Drop for NativeWatch {
    fn drop(&mut self) {
        unsafe {
            SetEvent(self.stop.0);
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
fn lost(root: &Path, receive: &Receiver) {
    receive(vec![
        Event::new(EventKind::Other)
            .set_flag(Flag::Rescan)
            .add_path(root.to_owned()),
    ]);
}
fn decode(root: &Path, bytes: &[u8]) -> Result<Vec<Event>> {
    ensure!(!bytes.is_empty(), "empty notification completion");
    let mut offset = 0;
    let mut events = vec![];
    loop {
        ensure!(bytes.len() - offset >= 12, "truncated notification header");
        let word = |at| u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
        let next = word(offset);
        let action = word(offset + 4) as u32;
        let length = word(offset + 8);
        ensure!(
            length % 2 == 0 && length <= bytes.len() - offset - 12,
            "invalid notification name"
        );
        let name: Vec<_> = bytes[offset + 12..offset + 12 + length]
            .chunks_exact(2)
            .map(|b| u16::from_le_bytes([b[0], b[1]]))
            .collect();
        let kind = match action {
            FILE_ACTION_ADDED => Some(EventKind::Create(CreateKind::Any)),
            FILE_ACTION_REMOVED => Some(EventKind::Remove(RemoveKind::Any)),
            FILE_ACTION_MODIFIED => Some(EventKind::Modify(ModifyKind::Any)),
            FILE_ACTION_RENAMED_OLD_NAME => {
                Some(EventKind::Modify(ModifyKind::Name(RenameMode::From)))
            }
            FILE_ACTION_RENAMED_NEW_NAME => {
                Some(EventKind::Modify(ModifyKind::Name(RenameMode::To)))
            }
            _ => None,
        };
        if let Some(kind) = kind {
            events.push(Event::new(kind).add_path(root.join(OsString::from_wide(&name))));
        }
        if next == 0 {
            break;
        }
        ensure!(
            next >= 12 + length && next % 4 == 0 && next <= bytes.len() - offset - 12,
            "invalid notification offset"
        );
        offset += next;
    }
    Ok(events)
}
fn issue(
    directory: &Handle,
    complete: &Handle,
    buffer: &mut [u8],
    overlap: &mut OVERLAPPED,
) -> bool {
    // SAFETY: buffer and OVERLAPPED remain live until GetOverlappedResult acknowledges
    // completion/cancellation. Only one request is outstanding on this directory handle.
    unsafe {
        ResetEvent(complete.0);
        // Only implemented index fields: timestamp/content writes of unchanged size
        // otherwise swamp full-volume watchers without changing any searchable value.
        let flags = FILE_NOTIFY_CHANGE_FILE_NAME
            | FILE_NOTIFY_CHANGE_DIR_NAME
            | FILE_NOTIFY_CHANGE_ATTRIBUTES
            | FILE_NOTIFY_CHANGE_SIZE
            | FILE_NOTIFY_CHANGE_SECURITY;
        ReadDirectoryChangesW(
            directory.0,
            buffer.as_mut_ptr().cast(),
            buffer.len() as u32,
            1,
            flags,
            core::ptr::null_mut(),
            overlap,
            None,
        ) != 0
            || GetLastError() == ERROR_IO_PENDING
    }
}
struct Reader {
    directory: Handle,
    complete: Handle,
    receiving: Vec<u8>,
    completed: Vec<u8>,
    overlap: Box<OVERLAPPED>,
    pending: bool,
}
impl Reader {
    fn arm(&mut self) {
        // The previous request has completed before this stable allocation is reused.
        *self.overlap = unsafe { core::mem::zeroed() };
        self.overlap.hEvent = self.complete.0;
        self.pending = issue(
            &self.directory,
            &self.complete,
            &mut self.receiving,
            &mut self.overlap,
        );
    }
}
impl Drop for Reader {
    fn drop(&mut self) {
        if self.pending {
            // Covers normal shutdown AND unwinding from a consumer callback. Kernel I/O
            // must acknowledge cancellation before any referenced allocation is released.
            unsafe {
                CancelIoEx(self.directory.0, &*self.overlap);
                let mut ignored = 0;
                GetOverlappedResult(self.directory.0, &*self.overlap, &mut ignored, 1);
                if core::ptr::read_volatile(&self.overlap.Internal) == STATUS_PENDING as usize {
                    // A broken driver must not turn failed cancellation into use-after-free.
                    core::mem::forget(std::mem::take(&mut self.receiving));
                    core::mem::forget(std::mem::take(&mut self.completed));
                    let allocation =
                        std::mem::replace(&mut self.overlap, Box::new(core::mem::zeroed()));
                    let _ = Box::leak(allocation);
                }
            }
        }
    }
}
fn run(
    root: PathBuf,
    directory: Handle,
    stop: Arc<Handle>,
    complete: Handle,
    receive: Receiver,
    ready: std::sync::mpsc::SyncSender<std::result::Result<(), String>>,
) {
    let buffer_bytes = buffer_bytes(&root);
    let mut reader = Reader {
        directory,
        complete,
        receiving: vec![0; buffer_bytes],
        completed: vec![0; buffer_bytes],
        overlap: Box::new(unsafe { core::mem::zeroed() }),
        pending: false,
    };
    reader.arm();
    let _ = ready.send(if reader.pending {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error().to_string())
    });
    let mut announced_failure = false;
    loop {
        if !reader.pending {
            if !announced_failure {
                lost(&root, &receive);
                announced_failure = true;
            }
            if unsafe { WaitForSingleObject(stop.0, 1000) } == WAIT_OBJECT_0 {
                break;
            }
            if let Ok(directory) = open(&root) {
                reader.directory = directory;
                reader.arm();
            }
            continue;
        }
        let handles = [stop.0, reader.complete.0];
        if unsafe { WaitForMultipleObjects(2, handles.as_ptr(), 0, INFINITE) } != WAIT_OBJECT_0 + 1
        {
            break;
        }
        let mut bytes = 0;
        let success =
            unsafe { GetOverlappedResult(reader.directory.0, &*reader.overlap, &mut bytes, 0) }
                != 0;
        let error = if success {
            0
        } else {
            unsafe { GetLastError() }
        };
        if !success && error == ERROR_IO_INCOMPLETE {
            continue;
        }
        reader.pending = false;
        core::mem::swap(&mut reader.receiving, &mut reader.completed);
        if success || error == ERROR_NOTIFY_ENUM_DIR || error == ERROR_OPERATION_ABORTED {
            reader.arm();
        }
        if !success || bytes == 0 || bytes as usize > reader.completed.len() {
            lost(&root, &receive);
            announced_failure = true;
            continue;
        }
        announced_failure = false;
        match decode(&root, &reader.completed[..bytes as usize]) {
            Ok(events) => receive(events),
            Err(_) => lost(&root, &receive),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn local_reader_is_armed_on_return_and_tracks_flushed_size_and_rename() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("before.txt");
        std::fs::write(&file, b"x").unwrap();
        assert_eq!(buffer_bytes(dir.path()), LOCAL_BUFFER_BYTES);
        let (tx, rx) = std::sync::mpsc::channel();
        let _watch = NativeWatch::new(
            dir.path().to_owned(),
            Arc::new(move |events| {
                for event in events {
                    let _ = tx.send(event);
                }
            }),
        )
        .unwrap();
        let handle = std::fs::OpenOptions::new().write(true).open(&file).unwrap();
        handle.set_len(4097).unwrap();
        handle.sync_all().unwrap();
        drop(handle);
        let end = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            let event = rx
                .recv_timeout(end.saturating_duration_since(std::time::Instant::now()))
                .expect("size notification missing");
            if event.paths == [file.clone()] && matches!(event.kind, EventKind::Modify(_)) {
                break;
            }
        }
        let renamed = dir.path().join("after.txt");
        std::fs::rename(&file, &renamed).unwrap();
        let mut old = false;
        let mut new = false;
        while !old || !new {
            let event = rx
                .recv_timeout(end.saturating_duration_since(std::time::Instant::now()))
                .expect("rename notification missing");
            old |= event.paths == [file.clone()]
                && matches!(
                    event.kind,
                    EventKind::Modify(ModifyKind::Name(RenameMode::From))
                );
            new |= event.paths == [renamed.clone()]
                && matches!(
                    event.kind,
                    EventKind::Modify(ModifyKind::Name(RenameMode::To))
                );
        }
    }

    #[test]
    fn idle_readers_cancel_without_waiting_for_another_filesystem_event() {
        let dir = tempfile::tempdir().unwrap();
        let start = std::time::Instant::now();
        for _ in 0..16 {
            let watcher = NativeWatch::new(dir.path().to_owned(), Arc::new(|_| {})).unwrap();
            drop(watcher);
        }
        assert!(start.elapsed() < std::time::Duration::from_secs(10));
    }
    #[test]
    fn bounded_decoder_preserves_utf16_and_rejects_truncated_or_looping_records() {
        let name = [b'x' as u16, 0xd800, b'.' as u16, b't' as u16];
        let mut bytes = vec![];
        bytes.extend(0u32.to_le_bytes());
        bytes.extend(FILE_ACTION_ADDED.to_le_bytes());
        bytes.extend(((name.len() * 2) as u32).to_le_bytes());
        for c in name {
            bytes.extend(c.to_le_bytes());
        }
        let root = Path::new(r"C:\fixture");
        let events = decode(root, &bytes).unwrap();
        let actual: Vec<_> = events[0].paths[0]
            .file_name()
            .unwrap()
            .encode_wide()
            .collect();
        assert_eq!(actual, name);
        assert!(decode(root, &[]).is_err());
        assert!(decode(root, &bytes[..bytes.len() - 1]).is_err());
        bytes[..4].copy_from_slice(&4u32.to_le_bytes());
        assert!(decode(root, &bytes).is_err());
    }
}
