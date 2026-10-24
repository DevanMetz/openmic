//! One OpenMic at a time. A second launch (from the Start menu, say, while
//! the first sits hidden in the notification area) asks the running copy to
//! show its window, then exits instead of fighting it for the devices.

#[derive(Debug)]
pub struct AlreadyRunning;

#[cfg(windows)]
pub use windows_instance::{acquire, Instance};

#[cfg(windows)]
mod windows_instance {
    use windows::core::w;
    use windows::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS, HANDLE, WAIT_OBJECT_0};
    use windows::Win32::System::Threading::{CreateEventW, SetEvent, WaitForSingleObject, INFINITE};

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
}

/// Kept open for the process lifetime; closing it would release the lock.
#[cfg(not(windows))]
static LOCK: std::sync::OnceLock<std::fs::File> = std::sync::OnceLock::new();

#[cfg(not(windows))]
pub fn acquire() -> Result<Instance, AlreadyRunning> {
    let Some(path) = directories::ProjectDirs::from("app", "OpenMic", "OpenMic")
        .map(|dirs| dirs.config_dir().join("openmic.lock"))
    else {
        return Ok(Instance { lock: None });
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    acquire_at(&path)
}

#[cfg(not(windows))]
impl Instance {
    pub fn on_show(self, _show: impl Fn() + Send + 'static) {
        if let Some(file) = self.lock {
            let _ = LOCK.set(file);
        }
    }
}

#[cfg(all(test, not(windows)))]
mod tests {
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
}

#[cfg(not(windows))]
fn acquire_at(path: &std::path::Path) -> Result<Instance, AlreadyRunning> {
    let Ok(file) = std::fs::OpenOptions::new().create(true).truncate(false).read(true).write(true).open(path) else {
        return Ok(Instance { lock: None });
    };
    match file.try_lock() {
        Ok(()) => Ok(Instance { lock: Some(file) }),
        Err(std::fs::TryLockError::WouldBlock) => Err(AlreadyRunning),
        Err(std::fs::TryLockError::Error(_)) => Ok(Instance { lock: None }),
    }
}
