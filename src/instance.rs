//! One OpenMic at a time. A second launch asks the running copy to show its
//! window, then exits instead of fighting it for the devices.

#[derive(Debug)]
pub struct AlreadyRunning;

#[cfg(windows)]
pub use windows_instance::{Instance, acquire};

#[cfg(windows)]
mod windows_instance {
    use windows::Win32::Foundation::{ERROR_ALREADY_EXISTS, GetLastError, HANDLE, WAIT_OBJECT_0};
    use windows::Win32::System::Threading::{
        CreateEventW, INFINITE, SetEvent, WaitForSingleObject,
    };
    use windows::core::w;

    use super::AlreadyRunning;

    /// The running copy's "show your window" signal.
    pub struct Instance(Option<HANDLE>);

    /// Become the running copy, or wake the existing one and step aside.
    pub fn acquire() -> Result<Instance, AlreadyRunning> {
        // SAFETY: plain Win32 calls; the handle lives for the whole process.
        unsafe {
            let Ok(event) = CreateEventW(None, false, false, w!(r"Local\OpenMic.Show")) else {
                return Ok(Instance(None)); // can't coordinate; run anyway
            };
            if GetLastError() == ERROR_ALREADY_EXISTS {
                let _ = SetEvent(event);
                return Err(AlreadyRunning);
            }
            Ok(Instance(Some(event)))
        }
    }

    impl Instance {
        /// Call `show` each time another launch asks for the window.
        pub fn on_show(self, show: impl Fn() + Send + 'static) {
            let Some(event) = self.0 else { return };
            let raw = event.0 as usize; // HANDLE isn't Send; the value is.
            let _ = std::thread::Builder::new()
                .name("openmic-instance".into())
                .spawn(move || {
                    let event = HANDLE(raw as *mut _);
                    // SAFETY: the event handle is never closed.
                    while unsafe { WaitForSingleObject(event, INFINITE) } == WAIT_OBJECT_0 {
                        show();
                    }
                });
        }
    }
}

#[cfg(not(windows))]
pub struct Instance {
    lock: Option<std::fs::File>,
    /// Bound before [`acquire`] returns, so a second launch can ask for the
    /// window while this process is still starting. The datagram stays queued
    /// until [`Instance::on_show`] reads it.
    show: Option<std::os::unix::net::UnixDatagram>,
}

/// Kept open for the process lifetime; closing one would release its lock.
#[cfg(not(windows))]
static LOCKS: std::sync::Mutex<Vec<std::fs::File>> = std::sync::Mutex::new(Vec::new());

#[cfg(not(windows))]
pub fn acquire() -> Result<Instance, AlreadyRunning> {
    let Some(path) = directories::ProjectDirs::from("app", "OpenMic", "OpenMic")
        .map(|dirs| dirs.config_dir().join("openmic.lock"))
    else {
        return Ok(Instance {
            lock: None,
            show: None,
        });
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    acquire_at(&path)
}

#[cfg(not(windows))]
impl Instance {
    pub fn on_show(self, show: impl Fn() + Send + 'static) {
        if let Some(file) = self.lock {
            LOCKS.lock().unwrap_or_else(|e| e.into_inner()).push(file);
        }
        let Some(socket) = self.show else { return };
        let _ = std::thread::Builder::new()
            .name("openmic-instance".into())
            .spawn(move || {
                let mut buf = [0u8; 8];
                // Any datagram is a request; the payload is not a protocol.
                while socket.recv(&mut buf).is_ok() {
                    show();
                }
            });
    }
}

#[cfg(all(test, not(windows)))]
mod tests {
    use std::os::unix::fs::PermissionsExt;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    use super::*;

    #[test]
    fn the_linux_lock_is_exclusive_and_released_when_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("openmic.lock");
        let first = acquire_at(&path).unwrap();
        assert!(matches!(acquire_at(&path), Err(AlreadyRunning)));
        drop(first);
        assert!(acquire_at(&path).is_ok());
    }

    #[test]
    fn a_second_launch_asks_the_running_copy_to_show_its_window() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("openmic.lock");
        let running = acquire_at(&path).unwrap();
        let mode = std::fs::metadata(show_socket_path(&path))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "other users cannot raise the window");

        // Queued before the listener starts, as when a second launch arrives
        // while the first is still opening its window.
        assert!(matches!(acquire_at(&path), Err(AlreadyRunning)));
        let shown = Arc::new(AtomicUsize::new(0));
        let count = shown.clone();
        running.on_show(move || {
            count.fetch_add(1, Ordering::SeqCst);
        });
        wait_for_shows(&shown, 1);

        assert!(matches!(acquire_at(&path), Err(AlreadyRunning)));
        wait_for_shows(&shown, 2);
    }

    #[test]
    fn a_replaced_instance_receives_the_next_show_request() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("openmic.lock");
        let crashed = acquire_at(&path).unwrap();
        drop(crashed); // lock released, socket file left behind
        assert!(
            std::fs::metadata(show_socket_path(&path)).is_ok(),
            "a crash leaves the socket file"
        );

        let running = acquire_at(&path).unwrap();
        let shown = Arc::new(AtomicUsize::new(0));
        let count = shown.clone();
        running.on_show(move || {
            count.fetch_add(1, Ordering::SeqCst);
        });
        assert!(matches!(acquire_at(&path), Err(AlreadyRunning)));
        wait_for_shows(&shown, 1);
    }

    fn wait_for_shows(shown: &AtomicUsize, n: usize) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while shown.load(Ordering::SeqCst) < n {
            assert!(
                Instant::now() < deadline,
                "running copy was not asked to show its window"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

#[cfg(not(windows))]
fn acquire_at(path: &std::path::Path) -> Result<Instance, AlreadyRunning> {
    let Ok(file) = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)
    else {
        return Ok(Instance {
            lock: None,
            show: None,
        });
    };
    let show_path = show_socket_path(path);
    match file.try_lock() {
        Ok(()) => Ok(Instance {
            lock: Some(file),
            show: bind_show_socket(&show_path),
        }),
        Err(std::fs::TryLockError::WouldBlock) => {
            request_show(&show_path);
            Err(AlreadyRunning)
        }
        Err(std::fs::TryLockError::Error(_)) => Ok(Instance {
            lock: None,
            show: None,
        }),
    }
}

#[cfg(not(windows))]
fn show_socket_path(lock_path: &std::path::Path) -> std::path::PathBuf {
    lock_path.with_file_name("openmic.show")
}

/// Replace a socket file left by a process that exited. A live process holds
/// the lock until it exits, and only the lock holder calls this, so a socket
/// that is still receiving is never unlinked.
#[cfg(not(windows))]
fn bind_show_socket(path: &std::path::Path) -> Option<std::os::unix::net::UnixDatagram> {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::remove_file(path);
    let socket = std::os::unix::net::UnixDatagram::bind(path).ok()?;
    // Other accounts on this machine do not get to raise the window.
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    Some(socket)
}

/// Tell the running copy to show its window. A missing socket (an older
/// running copy, or one still replacing a crashed one) is retried briefly,
/// then ignored so this launch still exits.
#[cfg(not(windows))]
fn request_show(path: &std::path::Path) {
    let Ok(socket) = std::os::unix::net::UnixDatagram::unbound() else {
        return;
    };
    // A full queue already holds a show request. Don't stall this launch on it.
    let _ = socket.set_nonblocking(true);
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(100);
    loop {
        match socket.send_to(b"show", path) {
            Ok(_) => return,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => return,
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound
                        | std::io::ErrorKind::ConnectionRefused
                        | std::io::ErrorKind::AddrNotAvailable
                ) =>
            {
                if std::time::Instant::now() >= deadline {
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            Err(_) => return,
        }
    }
}
