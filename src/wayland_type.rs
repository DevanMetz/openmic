//! Native Wayland dictation. Only backend setup may fall through: retrying a
//! phrase after an injection failure would duplicate already inserted text.
use anyhow::{Context, Result, bail};
use ashpd::desktop::{
    PersistMode, Session,
    remote_desktop::{DeviceType, KeyState, RemoteDesktop},
};
use std::io::{Seek, Write};
use std::os::fd::AsFd;
use std::sync::{
    OnceLock,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};
use wayland_client::{
    Connection, Dispatch, QueueHandle, delegate_noop,
    globals::{GlobalListContents, registry_queue_init},
    protocol::{wl_registry, wl_seat},
};
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::{
    zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1,
    zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1,
};

const KEY_INTERVAL: Duration = Duration::from_millis(12);

type PortalKeyboard = (
    RemoteDesktop<'static>,
    Session<'static, RemoteDesktop<'static>>,
);
// Keep the grant alive even on older portals without persistent restore tokens.
static PORTAL: futures_util::lock::Mutex<Option<PortalKeyboard>> =
    futures_util::lock::Mutex::new(None);
static CONTEXT: OnceLock<eframe::egui::Context> = OnceLock::new();

pub(crate) fn observe_context(ctx: &eframe::egui::Context) {
    CONTEXT.get_or_init(|| ctx.clone());
}

pub(crate) fn is_wayland() -> bool {
    ["WAYLAND_DISPLAY", "WAYLAND_SOCKET"]
        .iter()
        .any(|name| std::env::var_os(name).is_some_and(|v| !v.is_empty()))
        || std::env::var("XDG_SESSION_TYPE").is_ok_and(|v| v.eq_ignore_ascii_case("wayland"))
}

fn safe_to_type(cancel: &AtomicBool) -> Result<()> {
    if cancel.load(Ordering::Relaxed) {
        bail!("Transcription cancelled");
    }
    let ctx = CONTEXT.get().context("OpenMic focus is unknown")?;
    if openmic_focused(ctx) {
        bail!("Focus another app to type dictation");
    }
    Ok(())
}

fn openmic_focused(ctx: &eframe::egui::Context) -> bool {
    ctx.input(|i| i.focused || i.raw.viewports.values().any(|v| v.focused == Some(true)))
}

fn keysyms(text: &str) -> Result<Vec<u32>> {
    text.chars()
        .map(|ch| {
            let ch = if matches!(ch, '\r' | '\n') { ' ' } else { ch };
            if ch.is_control() {
                bail!("can't type control character {ch:?}");
            }
            let sym = xkeysym::Keysym::from_char(ch);
            if sym == xkeysym::Keysym::NoSymbol {
                bail!("can't type {ch:?}");
            }
            Ok(sym.raw())
        })
        .collect()
}

pub(crate) fn type_text(text: &str, cancel: &AtomicBool) -> Result<()> {
    let symbols = keysyms(text)?;
    if symbols.is_empty() {
        return Ok(());
    }
    safe_to_type(cancel)?;
    pollster::block_on(async {
        let mut cached = PORTAL.lock().await;
        if cached.is_none() {
            match portal_session().await {
                Ok(session) => *cached = Some(session),
                Err(err) => {
                    crate::logfile::warn(format_args!(
                        "Wayland keyboard portal unavailable: {err}"
                    ));
                    return virtual_keyboard(&symbols, cancel);
                }
            }
        }
        let (portal, session) = cached.as_ref().expect("portal session was established");
        let result = async {
            // Approval can change focus. Recheck after setup and before every key.
            for sym in symbols {
                safe_to_type(cancel)?;
                let pressed = portal
                    .notify_keyboard_keysym(session, sym as i32, KeyState::Pressed)
                    .await;
                // Even a failed D-Bus reply can follow a delivered press.
                let released = portal
                    .notify_keyboard_keysym(session, sym as i32, KeyState::Released)
                    .await;
                if released.is_err() {
                    let _ = portal
                        .notify_keyboard_keysym(session, sym as i32, KeyState::Released)
                        .await;
                }
                pressed?;
                released?;
                std::thread::sleep(KEY_INTERVAL);
            }
            Ok(())
        }
        .await;
        if result.is_err() {
            let _ = session.close().await;
            *cached = None;
        }
        result
    })
}
async fn portal_session() -> Result<PortalKeyboard> {
    let portal = RemoteDesktop::new().await?;
    let session = portal.create_session().await?;
    let result = async {
        let path = crate::config::settings_path().with_file_name("dictation-remote-desktop-token");
        let token = std::fs::read_to_string(&path).ok();
        portal
            .select_devices(
                &session,
                DeviceType::Keyboard.into(),
                token.as_deref(),
                PersistMode::ExplicitlyRevoked,
            )
            .await?
            .response()?;
        let devices = portal.start(&session, None).await?.response()?;
        if !devices.devices().contains(DeviceType::Keyboard) {
            bail!("keyboard access was not granted");
        }
        if let Some(token) = devices.restore_token()
            && let Err(err) = save_token(&path, token)
        {
            crate::logfile::warn(format_args!(
                "could not save the Wayland keyboard grant: {err}"
            ));
        }
        Ok(())
    }
    .await;
    if let Err(err) = result {
        let _ = session.close().await;
        return Err(err);
    }
    Ok((portal, session))
}

fn save_token(path: &std::path::Path, token: &str) -> Result<()> {
    // Atomic replacement also gives the token private permissions on Unix.
    let parent = path.parent().context("token directory")?;
    std::fs::create_dir_all(parent)?;
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    file.write_all(token.as_bytes())?;
    file.persist(path)?;
    Ok(())
}

struct State;
impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for State {
    fn event(
        _: &mut Self,
        _: &wl_registry::WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}
delegate_noop!(State: ignore wl_seat::WlSeat);
delegate_noop!(State: ignore ZwpVirtualKeyboardManagerV1);
delegate_noop!(State: ignore ZwpVirtualKeyboardV1);

fn virtual_keyboard(symbols: &[u32], cancel: &AtomicBool) -> Result<()> {
    let conn = Connection::connect_to_env()?;
    let (globals, mut queue) = registry_queue_init::<State>(&conn)?;
    let qh = queue.handle();
    let manager: ZwpVirtualKeyboardManagerV1 = globals
        .bind(&qh, 1..=1, ())
        .context("Wayland typing unavailable; copy the transcript instead")?;
    let seat: wl_seat::WlSeat = globals.bind(&qh, 1..=1, ())?;
    let keyboard = manager.create_virtual_keyboard(&seat, &qh, ());
    // One immutable keymap per phrase avoids racing keymap replacement with
    // application key processing. Evdev codes 1..=247 map to XKB codes 9..=255.
    let unique: std::collections::BTreeSet<_> = symbols.iter().copied().collect();
    if unique.len() > 247 {
        bail!("too many distinct characters for the virtual keyboard");
    }
    let codes: std::collections::BTreeMap<_, _> = unique
        .into_iter()
        .enumerate()
        .map(|(i, sym)| (sym, i as u32 + 1))
        .collect();
    let mut names = String::new();
    let mut bindings = String::new();
    for (sym, code) in &codes {
        names.push_str(&format!("<K{code}> = {};", code + 8));
        bindings.push_str(&format!("key <K{code}> {{ [ 0x{sym:x} ] }};"));
    }
    let map = format!(
        "xkb_keymap {{ xkb_keycodes {{ minimum = 8; maximum = 255; {names} }}; xkb_types {{ type \"ONE_LEVEL\" {{ modifiers = None; map[None] = Level1; level_name[Level1] = \"Any\"; }}; }}; xkb_compatibility {{ }}; xkb_symbols {{ {bindings} }}; }};\0"
    );
    let mut file = tempfile::tempfile()?;
    file.write_all(map.as_bytes())?;
    file.rewind()?;
    keyboard.keymap(1, file.as_fd(), u32::try_from(map.len())?);
    keyboard.modifiers(0, 0, 0, 0);
    queue.roundtrip(&mut State)?;
    let started = Instant::now();
    let result = (|| {
        for sym in symbols {
            safe_to_type(cancel)?;
            let time = started.elapsed().as_millis() as u32;
            keyboard.key(time, codes[sym], 1);
            keyboard.key(time, codes[sym], 0);
            conn.flush()?;
            std::thread::sleep(KEY_INTERVAL);
        }
        Ok(())
    })();
    keyboard.destroy();
    let _ = conn.flush();
    result
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn restore_token_replacement_stays_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir_in(".").unwrap();
        let path = dir.path().join("config/remote-desktop-token");
        save_token(&path, "first").unwrap();
        save_token(&path, "replacement").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "replacement");
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o077,
            0
        );
    }

    #[test]
    fn focus_guard_includes_secondary_windows() {
        use eframe::egui::{Context, RawInput, ViewportId, ViewportInfo};
        let ctx = Context::default();
        ctx.begin_pass(RawInput::default());
        assert!(openmic_focused(&ctx));
        let _ = ctx.end_pass();
        let mut input = RawInput {
            focused: false,
            ..Default::default()
        };
        input.viewports.get_mut(&ViewportId::ROOT).unwrap().focused = Some(false);
        ctx.begin_pass(input.clone());
        assert!(!openmic_focused(&ctx));
        let _ = ctx.end_pass();
        input.viewports.insert(
            ViewportId::from_hash_of("strip"),
            ViewportInfo {
                focused: Some(true),
                ..Default::default()
            },
        );
        ctx.begin_pass(input);
        assert!(openmic_focused(&ctx));
        let _ = ctx.end_pass();
    }

    #[test]
    fn cancelled_and_empty_text_need_no_desktop() {
        type_text("", &AtomicBool::new(false)).unwrap();
        assert!(
            type_text("hello", &AtomicBool::new(true))
                .unwrap_err()
                .to_string()
                .contains("cancelled")
        );
    }

    #[test]
    fn unicode_keysyms_round_trip() {
        let text = "Hello, café Ελληνικά 中文 🎵";
        let back: String = keysyms(text)
            .unwrap()
            .into_iter()
            .map(|s| xkeysym::Keysym::new(s).key_char().unwrap())
            .collect();
        assert_eq!(back, text);
        assert_eq!(keysyms("🎵").unwrap(), [0x0101f3b5]);
    }
    #[test]
    fn normalize_and_validate_before_typing() {
        assert_eq!(keysyms("a\r\nb").unwrap(), keysyms("a  b").unwrap());
        assert!(keysyms("\t").is_err());
        assert!(keysyms("\u{fdd0}").is_err());
        assert!(keysyms("").unwrap().is_empty());
    }
}
