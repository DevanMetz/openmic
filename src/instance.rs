//! One OpenMic at a time. A second launch (from the Start menu, say, while
//! the first sits hidden in the notification area) asks the running copy to
//! show its window, then exits instead of fighting it for the devices.

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
pub struct Instance;

#[cfg(not(windows))]
pub fn acquire() -> Result<Instance, AlreadyRunning> {
    Ok(Instance)
}

#[cfg(not(windows))]
impl Instance {
    pub fn on_show(self, _show: impl Fn() + Send + 'static) {}
}
