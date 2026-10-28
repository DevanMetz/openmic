//! Type dictation into the focused X11 window.
//!
//! Each character is bound to an unused keycode and sent with the XTEST
//! extension, then that keycode is cleared. The keysym is the character
//! itself, so the active layout and Caps Lock do not change what is typed.
//! Wayland clients never receive XTEST events; `wayland_type` handles those
//! sessions with native keyboard backends.

use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use x11rb::connection::Connection;
use x11rb::errors::{ConnectionError, ReplyError};
use x11rb::protocol::xproto::{ConnectionExt as _, KEY_PRESS_EVENT, KEY_RELEASE_EVENT, KeyButMask};
use x11rb::protocol::xtest::ConnectionExt as _;
use x11rb::rust_connection::RustConnection;
use xkeysym::Keysym;

/// How long to wait for the dictation shortcut's modifiers to come up.
/// Matches the Windows typer: a held Ctrl or Alt would turn the text into shortcuts.
const MODIFIER_WAIT: Duration = Duration::from_secs(3);

/// Type `text` into whichever X11 window has keyboard focus.
pub(crate) fn type_text(text: &str, cancel: &AtomicBool) -> Result<()> {
    let text = normalize_for_typing(text);
    if text.is_empty() {
        return Ok(());
    }
    cancelled(cancel)?;
    if let Some(reason) = session_block_from_env() {
        bail!(reason);
    }
    let keysyms = keysyms_for(&text)?;
    let (conn, _) = match RustConnection::connect(None) {
        Ok(conn) => conn,
        Err(err) => {
            crate::logfile::warn(format_args!("dictation typing could not connect: {err}"));
            bail!("couldn't connect to the X11 display");
        }
    };
    wait_for_shortcut_modifiers(&conn, cancel)?;
    cancelled(cancel)?;

    let (min, count) = keycode_range(&conn)?;
    let mapping = conn
        .get_keyboard_mapping(min, count)?
        .reply()
        .context("read the keyboard map")?;
    let scratch = scratch_keycode(min, mapping.keysyms_per_keycode, &mapping.keysyms)
        .context("this keyboard map has no free key for typed text")?;
    let per = mapping.keysyms_per_keycode;
    let restore = Restore {
        conn: &conn,
        keycode: scratch,
        per,
    };
    for keysym in keysyms {
        // Once characters are going out, finish the phrase. Cancelling after
        // a partial insert would hide the transcript while the other app
        // already has some of the words.
        bind_and_press(&conn, scratch, per, keysym)?;
        discard_events(&conn);
    }
    drop(restore);
    Ok(())
}

/// The Dictation shortcut's modifiers. Caps Lock and Num Lock are left alone.
fn shortcut_modifiers_held(mask: KeyButMask) -> bool {
    let held = u16::from(mask);
    let shortcut = u16::from(KeyButMask::SHIFT)
        | u16::from(KeyButMask::CONTROL)
        | u16::from(KeyButMask::MOD1)
        | u16::from(KeyButMask::MOD4);
    held & shortcut != 0
}

fn session_block_from_env() -> Option<&'static str> {
    session_block(
        std::env::var_os("DISPLAY").is_some(),
        std::env::var_os("WAYLAND_DISPLAY").is_some(),
        std::env::var("XDG_SESSION_TYPE").ok().as_deref(),
    )
}

/// `None` when an X11 session can receive injected keystrokes.
///
/// `WAYLAND_DISPLAY` or a Wayland session type means the focused app is
/// probably a Wayland client. XWayland's `DISPLAY` still accepts XTEST, but
/// those events never reach native Wayland windows.
fn session_block(
    display: bool,
    wayland_display: bool,
    session: Option<&str>,
) -> Option<&'static str> {
    if wayland_display || session.is_some_and(|kind| kind.eq_ignore_ascii_case("wayland")) {
        Some("typing into other apps isn't available on Wayland")
    } else if !display {
        Some("typing into other apps needs an X11 display")
    } else {
        None
    }
}

/// Windows replaces line breaks with spaces before sending Unicode keystrokes.
fn normalize_for_typing(text: &str) -> String {
    text.replace(['\r', '\n'], " ")
}

fn keysyms_for(text: &str) -> Result<Vec<u32>> {
    text.chars()
        .map(|ch| {
            let sym = Keysym::from_char(ch);
            if sym == Keysym::NoSymbol {
                bail!("can't type {ch:?}");
            }
            Ok(sym.raw())
        })
        .collect()
}

fn keycode_range(conn: &RustConnection) -> Result<(u8, u8)> {
    let setup = conn.setup();
    let min = setup.min_keycode;
    let count = u8::try_from(u16::from(setup.max_keycode) - u16::from(min) + 1)
        .context("read the X11 keycode range")?;
    Ok((min, count))
}

/// The first keycode whose keysyms are all empty. Typing never rebinds a real key.
fn scratch_keycode(min: u8, per: u8, keysyms: &[u32]) -> Option<u8> {
    let per = usize::from(per);
    if per == 0 {
        return None;
    }
    keysyms
        .chunks_exact(per)
        .enumerate()
        .find_map(|(index, row)| {
            if row.iter().any(|sym| *sym != 0) {
                return None;
            }
            u8::try_from(index)
                .ok()
                .and_then(|index| min.checked_add(index))
        })
}

fn bind_and_press(conn: &RustConnection, keycode: u8, per: u8, keysym: u32) -> Result<()> {
    let mut row = vec![0u32; usize::from(per)];
    row[0] = keysym;
    checked(
        conn.change_keyboard_mapping(1, keycode, per, &row),
        "bind a key",
    )?;
    fake_key(conn, KEY_PRESS_EVENT, keycode)?;
    fake_key(conn, KEY_RELEASE_EVENT, keycode)?;
    Ok(())
}

fn fake_key(conn: &RustConnection, kind: u8, keycode: u8) -> Result<()> {
    checked(
        conn.xtest_fake_input(kind, keycode, x11rb::CURRENT_TIME, x11rb::NONE, 0, 0, 0),
        "send a key",
    )
}

fn checked(
    cookie: Result<x11rb::cookie::VoidCookie<'_, RustConnection>, ConnectionError>,
    what: &'static str,
) -> Result<()> {
    let result = match cookie {
        Ok(cookie) => cookie.check(),
        Err(err) => Err(ReplyError::from(err)),
    };
    match result {
        Ok(()) => Ok(()),
        Err(ReplyError::ConnectionError(ConnectionError::UnsupportedExtension)) => {
            bail!("this X11 server cannot inject keystrokes");
        }
        Err(err) => {
            crate::logfile::warn(format_args!("dictation typing ({what}): {err}"));
            bail!("the X11 server rejected typed text");
        }
    }
}

fn wait_for_shortcut_modifiers(conn: &RustConnection, cancel: &AtomicBool) -> Result<()> {
    let root = conn.setup().roots.first().context("X11 screen")?.root;
    let started = Instant::now();
    while shortcut_modifiers_held(
        conn.query_pointer(root)?
            .reply()
            .context("read modifier keys")?
            .mask,
    ) {
        cancelled(cancel)?;
        if started.elapsed() > MODIFIER_WAIT {
            bail!("Release Ctrl, Alt, Shift and Super so the text can be typed");
        }
        thread::sleep(Duration::from_millis(20));
    }
    Ok(())
}

fn cancelled(cancel: &AtomicBool) -> Result<()> {
    if cancel.load(Ordering::Relaxed) {
        bail!("Transcription cancelled");
    }
    Ok(())
}

fn discard_events(conn: &RustConnection) {
    while matches!(conn.poll_for_event(), Ok(Some(_))) {}
}

/// Puts the scratch keycode back so a failed or finished phrase cannot leave
/// a character bound to a key.
struct Restore<'a> {
    conn: &'a RustConnection,
    keycode: u8,
    per: u8,
}

impl Drop for Restore<'_> {
    fn drop(&mut self) {
        if self.per == 0 {
            return;
        }
        let zeros = vec![0u32; usize::from(self.per)];
        let _ = checked(
            self.conn
                .change_keyboard_mapping(1, self.keycode, self.per, &zeros),
            "restore the keyboard map",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use x11rb::protocol::xproto;

    #[test]
    fn line_breaks_become_spaces_like_windows() {
        assert_eq!(normalize_for_typing("a\r\nb\nc"), "a  b c");
        assert_eq!(normalize_for_typing("already one line"), "already one line");
    }

    #[test]
    fn keysyms_round_trip_plain_text_accents_and_symbols() {
        let text = "Hello, café 🎵";
        let syms = keysyms_for(&normalize_for_typing(text)).unwrap();
        let back: String = syms
            .into_iter()
            .map(|sym| Keysym::new(sym).key_char().unwrap())
            .collect();
        assert_eq!(back, text);
    }

    #[test]
    fn a_noncharacter_is_refused_before_any_key_is_sent() {
        let err = keysyms_for("ok\u{FDD0}").unwrap_err();
        assert!(err.to_string().contains("can't type"));
    }

    #[test]
    fn scratch_keycode_is_the_first_empty_row() {
        let keysyms = vec![
            0x61, 0, // 8
            0, 0, // 9
            0x20, 0, // 10
        ];
        assert_eq!(scratch_keycode(8, 2, &keysyms), Some(9));
        assert_eq!(scratch_keycode(8, 2, &[1, 0, 2, 0]), None);
        assert_eq!(scratch_keycode(8, 0, &[]), None);
        assert_eq!(scratch_keycode(255, 1, &[0]), Some(255));
        assert_eq!(
            scratch_keycode(255, 1, &[1, 0]),
            None,
            "the empty row would be keycode 256"
        );
    }

    #[test]
    fn shortcut_modifiers_are_shift_ctrl_alt_and_super_only() {
        assert!(!shortcut_modifiers_held(KeyButMask::default()));
        assert!(!shortcut_modifiers_held(KeyButMask::LOCK));
        assert!(!shortcut_modifiers_held(KeyButMask::MOD2));
        assert!(shortcut_modifiers_held(KeyButMask::SHIFT));
        assert!(shortcut_modifiers_held(KeyButMask::CONTROL));
        assert!(shortcut_modifiers_held(KeyButMask::MOD1));
        assert!(shortcut_modifiers_held(KeyButMask::MOD4));
        assert!(shortcut_modifiers_held(
            KeyButMask::CONTROL | KeyButMask::BUTTON1
        ));
    }

    #[test]
    fn wayland_and_headless_sessions_do_not_type() {
        assert_eq!(session_block(true, false, None), None);
        assert_eq!(session_block(true, false, Some("x11")), None);
        assert_eq!(session_block(true, false, Some("tty")), None);
        assert_eq!(
            session_block(false, false, Some("x11")),
            Some("typing into other apps needs an X11 display")
        );
        assert_eq!(
            session_block(true, true, Some("x11")),
            Some("typing into other apps isn't available on Wayland"),
            "XWayland's DISPLAY does not reach native Wayland windows"
        );
        assert_eq!(
            session_block(true, false, Some("Wayland")),
            Some("typing into other apps isn't available on Wayland")
        );
    }

    #[test]
    fn empty_text_and_cancellation_do_not_need_a_display() {
        type_text("", &AtomicBool::new(false)).unwrap();
        let err = type_text("hello", &AtomicBool::new(true)).unwrap_err();
        assert!(err.to_string().contains("Transcription cancelled"));
    }

    /// One live server for every XTEST test. Parallel tests would steal the
    /// keymap and the modifier state from each other.
    static X11_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    struct TestWindow {
        conn: RustConnection,
        window: u32,
    }

    impl TestWindow {
        fn open() -> Result<Self> {
            let (conn, screen_num) = RustConnection::connect(None).context("connect")?;
            let window = conn.generate_id()?;
            let screen = &conn.setup().roots[screen_num];
            let root = screen.root;
            let white = screen.white_pixel;
            conn.create_window(
                x11rb::COPY_DEPTH_FROM_PARENT,
                window,
                root,
                0,
                0,
                200,
                100,
                0,
                xproto::WindowClass::INPUT_OUTPUT,
                0,
                &xproto::CreateWindowAux::new()
                    .background_pixel(white)
                    .event_mask(xproto::EventMask::KEY_PRESS | xproto::EventMask::KEY_RELEASE),
            )?
            .check()?;
            conn.map_window(window)?.check()?;
            conn.set_input_focus(xproto::InputFocus::PARENT, window, x11rb::CURRENT_TIME)?
                .check()?;
            let deadline = Instant::now() + Duration::from_secs(2);
            loop {
                if conn.get_input_focus()?.reply()?.focus == window {
                    break;
                }
                if Instant::now() >= deadline {
                    bail!("the test window did not take keyboard focus");
                }
                thread::sleep(Duration::from_millis(10));
            }
            Ok(Self { conn, window })
        }

        fn mapping(&self) -> (u8, Vec<u32>) {
            let (min, count) = keycode_range(&self.conn).unwrap();
            let map = self
                .conn
                .get_keyboard_mapping(min, count)
                .unwrap()
                .reply()
                .unwrap();
            (map.keysyms_per_keycode, map.keysyms)
        }

        fn control_keycode(&self) -> Option<u8> {
            let reply = self.conn.get_modifier_mapping().ok()?.reply().ok()?;
            let per = usize::from(reply.keycodes_per_modifier());
            const CONTROL: usize = 2;
            reply.keycodes[CONTROL * per..(CONTROL + 1) * per]
                .iter()
                .copied()
                .find(|code| *code != 0)
        }

        fn drain_key_presses(&self, expect: usize) -> Vec<u8> {
            let mut presses = Vec::new();
            let deadline = Instant::now() + Duration::from_secs(3);
            let mut quiet_since = None;
            while Instant::now() < deadline {
                match self.conn.poll_for_event().expect("read X events") {
                    Some(x11rb::protocol::Event::KeyPress(ev)) if ev.event == self.window => {
                        presses.push(ev.detail);
                        quiet_since = None;
                    }
                    Some(_) => quiet_since = None,
                    None => {
                        let since = *quiet_since.get_or_insert_with(Instant::now);
                        if presses.len() >= expect && since.elapsed() >= Duration::from_millis(50) {
                            break;
                        }
                        thread::sleep(Duration::from_millis(5));
                    }
                }
            }
            presses
        }

        /// The character currently bound to `keycode`, as the server reports
        /// it, then the matching key press. The binding must still be in place.
        fn character_from_next_press(&self, keycode: u8) -> Option<char> {
            let deadline = Instant::now() + Duration::from_secs(2);
            let mut bound = None;
            while Instant::now() < deadline {
                match self.conn.poll_for_event().ok()? {
                    Some(x11rb::protocol::Event::MappingNotify(_)) => bound = self.column0(keycode),
                    Some(x11rb::protocol::Event::KeyPress(ev))
                        if ev.detail == keycode && ev.event == self.window =>
                    {
                        return bound.and_then(|sym| Keysym::new(sym).key_char());
                    }
                    Some(_) => {}
                    None => thread::sleep(Duration::from_millis(5)),
                }
            }
            None
        }

        fn column0(&self, keycode: u8) -> Option<u32> {
            let (min, count) = keycode_range(&self.conn).ok()?;
            let map = self
                .conn
                .get_keyboard_mapping(min, count)
                .ok()?
                .reply()
                .ok()?;
            let per = usize::from(map.keysyms_per_keycode);
            let index = usize::from(keycode.checked_sub(min)?) * per;
            map.keysyms.get(index).copied().filter(|sym| *sym != 0)
        }
    }

    struct HeldKey<'a> {
        conn: &'a RustConnection,
        keycode: u8,
    }

    impl Drop for HeldKey<'_> {
        fn drop(&mut self) {
            let _ = fake_key(self.conn, KEY_RELEASE_EVENT, self.keycode);
        }
    }

    #[test]
    #[ignore = "types into a focused window on a disposable X11 DISPLAY"]
    fn focused_window_receives_each_character_and_the_keymap_is_restored() {
        let _lock = X11_LOCK.lock().unwrap_or_else(|err| err.into_inner());
        let window = TestWindow::open().expect("connect to DISPLAY");
        let before = window.mapping();
        let text = "Hello, café 🎵";
        type_text(text, &AtomicBool::new(false)).unwrap();
        let presses = window.drain_key_presses(text.chars().count());
        assert_eq!(
            presses.len(),
            text.chars().count(),
            "key presses: {presses:?}"
        );
        assert!(
            presses.iter().all(|code| *code == presses[0]),
            "one scratch keycode, got {presses:?}"
        );
        assert_eq!(
            window.mapping(),
            before,
            "the scratch key must be empty again"
        );

        let scratch = presses[0];
        let sample = 'é';
        bind_and_press(
            &window.conn,
            scratch,
            before.0,
            Keysym::from_char(sample).raw(),
        )
        .unwrap();
        assert_eq!(window.character_from_next_press(scratch), Some(sample));
        drop(Restore {
            conn: &window.conn,
            keycode: scratch,
            per: before.0,
        });
        assert_eq!(window.mapping(), before);
    }

    #[test]
    #[ignore = "holds a modifier on a disposable X11 DISPLAY"]
    fn typing_waits_for_ctrl_to_come_up_and_cancel_stops_the_wait() {
        let _lock = X11_LOCK.lock().unwrap_or_else(|err| err.into_inner());
        let window = TestWindow::open().expect("connect to DISPLAY");
        let ctrl = window.control_keycode().expect("a Control keycode");
        let before = window.mapping();

        fake_key(&window.conn, KEY_PRESS_EVENT, ctrl).unwrap();
        let held = HeldKey {
            conn: &window.conn,
            keycode: ctrl,
        };
        let started = Instant::now();
        let cancel = std::sync::Arc::new(AtomicBool::new(false));
        let flag = std::sync::Arc::clone(&cancel);
        let worker = thread::spawn(move || type_text("a", &flag));
        thread::sleep(Duration::from_millis(150));
        assert!(!worker.is_finished(), "Ctrl must hold typing back");
        cancel.store(true, Ordering::Relaxed);
        let err = worker.join().unwrap().unwrap_err();
        assert!(err.to_string().contains("Transcription cancelled"), "{err}");
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "cancel must not wait out the modifier timeout"
        );
        drop(held);
        assert_eq!(
            window.mapping(),
            before,
            "a cancelled wait must not rebind keys"
        );

        fake_key(&window.conn, KEY_PRESS_EVENT, ctrl).unwrap();
        let held = HeldKey {
            conn: &window.conn,
            keycode: ctrl,
        };
        let worker = thread::spawn(|| type_text("b", &AtomicBool::new(false)));
        thread::sleep(Duration::from_millis(100));
        assert!(
            !worker.is_finished(),
            "typing starts only after Ctrl is released"
        );
        drop(held);
        worker.join().unwrap().unwrap();
        assert_eq!(window.mapping(), before);
    }
}
