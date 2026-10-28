//! Make the virtual cable's recording side ("CABLE Output" on Windows,
//! "OpenMic Microphone" on Linux) the default microphone while OpenMic runs,
//! so apps left on "Default" (Discord's own default) hear the cleaned voice
//! with no manual setup.
//!
//! Windows has no public API for changing default devices; this uses the
//! long-stable undocumented IPolicyConfig interface, as EarTrumpet and
//! SoundSwitch do. COM work runs on a short-lived MTA thread so it never
//! touches the GUI thread's apartment. Linux uses pactl's default source.

use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};

/// The default recording endpoints before OpenMic changed them, persisted so
/// they can be restored even after a crash.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedDefaults {
    pub console: String,
    pub communications: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Role {
    Console,
    Communications,
}

/// Endpoint id of the virtual cable's recording device, if present.
pub fn cable_capture_id() -> Result<Option<String>> {
    on_com_thread(imp::cable_capture_id)
}

pub fn current() -> Result<SavedDefaults> {
    on_com_thread(imp::current)
}

/// Make `id` the default recording device for console and communications.
pub fn set(id: &str) -> Result<()> {
    let id = id.to_owned();
    on_com_thread(move || {
        imp::set_role(&id, Role::Console)?;
        imp::set_role(&id, Role::Communications)
    })
}

/// Put `saved` back for each role that still points at `ours`, leaving any
/// default the user changed in the meantime alone.
pub fn restore(saved: &SavedDefaults, ours: &str) -> Result<()> {
    let (saved, ours) = (saved.clone(), ours.to_owned());
    on_com_thread(move || {
        let now = imp::current()?;
        restore_roles(&saved, &ours, &now, imp::set_role)
    })
}

fn restore_roles(
    saved: &SavedDefaults,
    ours: &str,
    now: &SavedDefaults,
    mut set_role: impl FnMut(&str, Role) -> Result<()>,
) -> Result<()> {
    let mut failure = None;
    for (before, current, role) in [
        (&saved.console, &now.console, Role::Console),
        (
            &saved.communications,
            &now.communications,
            Role::Communications,
        ),
    ] {
        // Do not rewrite a role the user changed. A missing old endpoint for
        // one role must not prevent the other role from being restored.
        if current == ours
            && before != current
            && let Err(e) = set_role(before, role)
        {
            failure.get_or_insert(e);
        }
    }
    failure.map_or(Ok(()), Err)
}

fn on_com_thread<T: Send + 'static>(f: impl FnOnce() -> Result<T> + Send + 'static) -> Result<T> {
    std::thread::Builder::new()
        .name("openmic-default-mic".into())
        .spawn(move || {
            let _apartment = imp::com_init()?;
            f()
        })
        .context("start default microphone helper")?
        .join()
        .map_err(|_| anyhow!("default microphone thread panicked"))?
}

#[cfg(windows)]
#[allow(non_snake_case)] // COM method names
mod imp {
    use anyhow::{Context, Result, anyhow};
    use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
    use windows::Win32::Media::Audio::{
        DEVICE_STATE_ACTIVE, ERole, IMMDeviceEnumerator, MMDeviceEnumerator, eCapture,
        eCommunications, eConsole,
    };
    use windows::Win32::System::Com::{
        CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree,
        CoUninitialize, STGM_READ,
    };
    use windows::core::{GUID, HRESULT, IUnknown, IUnknown_Vtbl, PCWSTR, interface};

    use super::{Role, SavedDefaults};

    /// Only the vtable order matters; OpenMic calls SetDefaultEndpoint alone.
    #[interface("f8679f50-850a-41cf-9c72-430f290290c8")]
    unsafe trait IPolicyConfig: IUnknown {
        fn GetMixFormat(&self) -> HRESULT;
        fn GetDeviceFormat(&self) -> HRESULT;
        fn ResetDeviceFormat(&self) -> HRESULT;
        fn SetDeviceFormat(&self) -> HRESULT;
        fn GetProcessingPeriod(&self) -> HRESULT;
        fn SetProcessingPeriod(&self) -> HRESULT;
        fn GetShareMode(&self) -> HRESULT;
        fn SetShareMode(&self) -> HRESULT;
        fn GetPropertyValue(&self) -> HRESULT;
        fn SetPropertyValue(&self) -> HRESULT;
        fn SetDefaultEndpoint(&self, id: PCWSTR, role: ERole) -> HRESULT;
        fn SetEndpointVisibility(&self) -> HRESULT;
    }

    const CLSID_POLICY_CONFIG: GUID = GUID::from_u128(0x870af99c_171d_4f9e_af0d_e63df40c2bc9);

    /// Each successful initialization is released on the same thread.
    pub struct ComApartment(std::marker::PhantomData<*mut ()>);

    impl Drop for ComApartment {
        fn drop(&mut self) {
            // SAFETY: constructed only after successful initialization;
            // the non-Send guard outlives all COM objects used by the helper.
            unsafe { CoUninitialize() };
        }
    }

    pub fn com_init() -> Result<ComApartment> {
        unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }
            .ok()
            .context("initialise COM")?;
        Ok(ComApartment(std::marker::PhantomData))
    }

    fn enumerator() -> Result<IMMDeviceEnumerator> {
        Ok(unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }?)
    }

    fn take_id(id: windows::core::PWSTR) -> Result<String> {
        let s = unsafe { id.to_string() };
        unsafe { CoTaskMemFree(Some(id.0 as _)) };
        Ok(s?)
    }

    pub fn cable_capture_id() -> Result<Option<String>> {
        let devices = unsafe { enumerator()?.EnumAudioEndpoints(eCapture, DEVICE_STATE_ACTIVE) }?;
        for i in 0..unsafe { devices.GetCount() }? {
            let device = unsafe { devices.Item(i) }?;
            let props = unsafe { device.OpenPropertyStore(STGM_READ) }?;
            let name = unsafe { props.GetValue(&PKEY_Device_FriendlyName) }?.to_string();
            if name.contains("CABLE Output") {
                return Ok(Some(take_id(unsafe { device.GetId() }?)?));
            }
        }
        Ok(None)
    }

    pub fn current() -> Result<SavedDefaults> {
        let e = enumerator()?;
        let id = |role| -> Result<String> {
            take_id(unsafe { e.GetDefaultAudioEndpoint(eCapture, role)?.GetId() }?)
        };
        Ok(SavedDefaults {
            console: id(eConsole)?,
            communications: id(eCommunications)?,
        })
    }

    pub fn set_role(id: &str, role: Role) -> Result<()> {
        let policy: IPolicyConfig =
            unsafe { CoCreateInstance(&CLSID_POLICY_CONFIG, None, CLSCTX_ALL) }
                .context("open Windows audio policy")?;
        let role = match role {
            Role::Console => eConsole,
            Role::Communications => eCommunications,
        };
        let wide: Vec<u16> = id.encode_utf16().chain(Some(0)).collect();
        unsafe { policy.SetDefaultEndpoint(PCWSTR(wide.as_ptr()), role) }
            .ok()
            .map_err(|e| anyhow!("set default microphone: {e}"))
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    use anyhow::{Context, Result, bail};

    use super::{Role, SavedDefaults};

    pub struct ComApartment;

    pub fn com_init() -> Result<ComApartment> {
        Ok(ComApartment)
    }

    pub fn cable_capture_id() -> Result<Option<String>> {
        let sources = pactl(&["list", "short", "sources"])?;
        Ok(source_names(&sources)
            .into_iter()
            .find(|name| *name == crate::virtual_mic::SOURCE_NAME)
            .map(str::to_owned))
    }

    pub fn current() -> Result<SavedDefaults> {
        let source = pactl(&["get-default-source"])?.trim().to_owned();
        if source.is_empty() {
            bail!("pactl returned no default source");
        }
        Ok(SavedDefaults {
            console: source.clone(),
            communications: source,
        })
    }

    pub fn set_role(id: &str, _role: Role) -> Result<()> {
        pactl(&["set-default-source", id]).map(|_| ())
    }

    /// The names in `pactl list short sources`: "<index> <name> ...".
    fn source_names(text: &str) -> Vec<&str> {
        text.lines()
            .filter_map(|line| line.split_whitespace().nth(1))
            .collect()
    }

    /// pactl can hang when a server disappears mid-command; fail instead.
    fn pactl(args: &[&str]) -> Result<String> {
        let what = args.first().copied().unwrap_or("pactl");
        let mut child = Command::new("pactl")
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| format!("run pactl {what} (is pulseaudio-utils installed?)"))?;
        let deadline = Instant::now() + Duration::from_secs(3);
        while child.try_wait()?.is_none() {
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait_with_output();
                bail!("pactl {what} timed out");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let output = child.wait_with_output().context("read pactl output")?;
        if !output.status.success() {
            bail!(
                "pactl {what} failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn source_names_skip_the_index_and_extra_columns() {
            let text = "0\talsa_output.pci.monitor\tmodule-alsa-card.c\ts16le 2ch 48000Hz\tSUSPENDED\n\
                        1\topenmic_mic\tmodule-remap-source.c\tfloat32le 1ch 48000Hz\tIDLE\n";
            assert_eq!(
                source_names(text),
                ["alsa_output.pci.monitor", "openmic_mic"]
            );
            assert!(source_names("").is_empty());
        }
    }
}

#[cfg(all(not(windows), not(target_os = "linux")))]
mod imp {
    use super::{Role, SavedDefaults};
    use anyhow::{Result, anyhow};

    pub struct ComApartment;

    pub fn com_init() -> Result<ComApartment> {
        Ok(ComApartment)
    }
    pub fn cable_capture_id() -> Result<Option<String>> {
        Ok(None)
    }
    pub fn current() -> Result<SavedDefaults> {
        Err(anyhow!(
            "default devices are not supported on this platform"
        ))
    }
    pub fn set_role(_: &str, _: Role) -> Result<()> {
        Err(anyhow!(
            "default devices are not supported on this platform"
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn defaults(console: &str, communications: &str) -> SavedDefaults {
        SavedDefaults {
            console: console.into(),
            communications: communications.into(),
        }
    }

    #[test]
    fn restore_leaves_user_changed_roles_untouched() {
        let mut restored = Vec::new();
        restore_roles(
            &defaults("old mic", "old headset"),
            "cable",
            &defaults("new mic", "cable"),
            |id, role| {
                restored.push((id.to_owned(), role));
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(restored, [("old headset".into(), Role::Communications)]);
    }

    #[test]
    fn a_missing_old_endpoint_does_not_block_the_other_role() {
        let saved = defaults("unplugged mic", "headset");
        let mut now = defaults("cable", "cable");
        let mut restored = Vec::new();
        let result = restore_roles(&saved, "cable", &now, |id, role| {
            restored.push((id.to_owned(), role));
            if role == Role::Console {
                Err(anyhow!("endpoint unavailable"))
            } else {
                Ok(())
            }
        });
        assert!(
            result.is_err(),
            "keep the recovery snapshot for a later retry"
        );
        assert_eq!(
            restored.len(),
            2,
            "communications is attempted even if console fails"
        );

        now.communications = "headset".into();
        restored.clear();
        restore_roles(&saved, "cable", &now, |id, role| {
            restored.push((id.to_owned(), role));
            Ok(())
        })
        .unwrap();
        assert_eq!(restored, [("unplugged mic".into(), Role::Console)]);
    }

    #[test]
    fn defaults_that_already_pointed_at_cable_need_no_restore() {
        restore_roles(
            &defaults("cable", "cable"),
            "cable",
            &defaults("cable", "cable"),
            |_, _| panic!("unchanged defaults should not be rewritten"),
        )
        .unwrap();
    }
}
