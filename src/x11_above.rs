//! Keep the level strip above other X11 windows without taking keyboard focus.
//!
//! winit asks for `_NET_WM_STATE_ABOVE` when the strip is created. That flag
//! is not sticky: a window manager restacks when another window is focused,
//! the same way Windows raises the taskbar over a topmost window. This module
//! repeats the request when the focused window changes, and every couple of
//! seconds, and does not call `SetInputFocus`.
//!
//! Wayland's xdg-shell has no stacking request, and winit ignores the window
//! level there. These functions do nothing unless winit itself is on X11
//! (no `WAYLAND_DISPLAY` or `WAYLAND_SOCKET`, and `DISPLAY` is set).

use std::cell::RefCell;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use x11rb::connection::Connection;
use x11rb::errors::ReplyError;
use x11rb::protocol::ErrorKind;
use x11rb::protocol::xproto::{
    Atom, AtomEnum, ClientMessageEvent, ConfigureWindowAux, ConnectionExt, EventMask, StackMode,
    Window,
};
use x11rb::rust_connection::RustConnection;

/// `_NET_WM_STATE_ADD`.
const STATE_ADD: u32 = 1;
/// The request came from an ordinary client, not a pager.
const SOURCE_APPLICATION: u32 = 1;
/// How long to wait before talking to a display that just failed.
const RETRY_AFTER: Duration = Duration::from_secs(2);

/// The focused X11 window, or nothing on Wayland and when the display is down.
pub(crate) fn foreground_window() -> Option<isize> {
    with_server(|server| server.active_window()).flatten()
}

/// Re-assert "above" for each of `titles` owned by this process.
///
/// The hover hint is included so a restack of the strip does not cover it.
/// A title with no window is skipped; the strip viewport may not exist yet.
pub(crate) fn raise_windows(titles: &[&str]) {
    let pid = std::process::id();
    let _ = with_server(|server| {
        for title in titles {
            server.raise_titled(title, pid)?;
        }
        Ok(())
    });
}

/// winit 0.30 picks Wayland when `WAYLAND_DISPLAY` or `WAYLAND_SOCKET` is
/// non-empty, and otherwise X11 when `DISPLAY` is non-empty.
fn uses_x11(wayland: bool, display: bool) -> bool {
    !wayland && display
}

fn env_nonempty(name: &str) -> bool {
    std::env::var_os(name).is_some_and(|value| !value.is_empty())
}

fn session_uses_x11() -> bool {
    static USES_X11: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *USES_X11.get_or_init(|| {
        let wayland = env_nonempty("WAYLAND_DISPLAY") || env_nonempty("WAYLAND_SOCKET");
        uses_x11(wayland, env_nonempty("DISPLAY"))
    })
}

struct Link {
    server: Option<Server>,
    retry_at: Option<Instant>,
    warned: bool,
}

thread_local! {
    static LINK: RefCell<Link> = const { RefCell::new(Link { server: None, retry_at: None, warned: false }) };
}

fn with_server<T>(f: impl FnOnce(&mut Server) -> Result<T>) -> Option<T> {
    if !session_uses_x11() {
        return None;
    }
    LINK.with(|link| {
        let mut link = link.borrow_mut();
        if link.retry_at.is_some_and(|at| Instant::now() < at) {
            return None;
        }
        if link.server.is_none() {
            match Server::connect(None) {
                Ok(server) => link.server = Some(server),
                Err(error) => {
                    link.fail(format_args!("level strip could not reach X11: {error:#}"));
                    return None;
                }
            }
        }
        let server = link.server.as_mut()?;
        match f(server) {
            Ok(value) => {
                link.warned = false;
                link.retry_at = None;
                Some(value)
            }
            Err(error) => {
                link.fail(format_args!("level strip stay-on-top failed: {error:#}"));
                None
            }
        }
    })
}

impl Link {
    fn fail(&mut self, message: impl std::fmt::Display) {
        self.server = None;
        self.retry_at = Some(Instant::now() + RETRY_AFTER);
        if !self.warned {
            self.warned = true;
            crate::logfile::warn(message);
        }
    }
}

struct Atoms {
    net_wm_name: Atom,
    utf8: Atom,
    net_wm_pid: Atom,
    net_client_list: Atom,
    net_active: Atom,
    net_wm_state: Atom,
    net_wm_state_above: Atom,
    net_supporting_wm: Atom,
}

struct Server {
    conn: RustConnection,
    /// Screen roots, the connection's default screen first.
    roots: Vec<Window>,
    atoms: Atoms,
    /// Title to the window we raised last, so the next raise is one lookup.
    cache: Vec<(String, Window)>,
}

impl Server {
    fn connect(display: Option<&str>) -> Result<Self> {
        let (conn, screen) = RustConnection::connect(display).context("connect to X11")?;
        let mut roots: Vec<Window> = conn.setup().roots.iter().map(|root| root.root).collect();
        if let Some(primary) = roots.get(screen).copied() {
            roots.retain(|root| *root != primary);
            roots.insert(0, primary);
        }
        let atom = |name: &[u8]| -> Result<Atom> {
            Ok(conn
                .intern_atom(false, name)?
                .reply()
                .context("intern an X11 atom")?
                .atom)
        };
        let atoms = Atoms {
            net_wm_name: atom(b"_NET_WM_NAME")?,
            utf8: atom(b"UTF8_STRING")?,
            net_wm_pid: atom(b"_NET_WM_PID")?,
            net_client_list: atom(b"_NET_CLIENT_LIST")?,
            net_active: atom(b"_NET_ACTIVE_WINDOW")?,
            net_wm_state: atom(b"_NET_WM_STATE")?,
            net_wm_state_above: atom(b"_NET_WM_STATE_ABOVE")?,
            net_supporting_wm: atom(b"_NET_SUPPORTING_WM_CHECK")?,
        };
        Ok(Self {
            conn,
            roots,
            atoms,
            cache: Vec::new(),
        })
    }

    /// `_NET_ACTIVE_WINDOW` on the default screen, then any other screen.
    fn active_window(&self) -> Result<Option<isize>> {
        for &root in &self.roots {
            let Some(values) = self.words(root, self.atoms.net_active, AtomEnum::WINDOW)? else {
                continue;
            };
            if let Some(window) = values.into_iter().find(|window| *window != 0) {
                return Ok(Some(window as isize));
            }
        }
        Ok(None)
    }

    fn raise_titled(&mut self, title: &str, pid: u32) -> Result<()> {
        let Some(window) = self.find(title, pid)? else {
            return Ok(());
        };
        self.request_above(window)
    }

    fn find(&mut self, title: &str, pid: u32) -> Result<Option<Window>> {
        if let Some(window) = self.cached(title, pid)? {
            return Ok(Some(window));
        }
        let found = self.search(title, pid)?;
        if let Some(window) = found {
            self.cache.retain(|(cached, _)| cached != title);
            self.cache.push((title.to_owned(), window));
        }
        Ok(found)
    }

    fn cached(&mut self, title: &str, pid: u32) -> Result<Option<Window>> {
        let Some(index) = self.cache.iter().position(|(cached, _)| cached == title) else {
            return Ok(None);
        };
        let window = self.cache[index].1;
        let still = matches!(
            self.identity(window)?,
            Some((name, Some(window_pid))) if name == title && window_pid == pid
        );
        if still {
            Ok(Some(window))
        } else {
            self.cache.remove(index);
            Ok(None)
        }
    }

    /// Windows this process owns with `title`.
    ///
    /// A window manager's client list is enough: the strip shows up there
    /// once it is mapped. With no list, look at the root's children and one
    /// level down, which is as deep as a frame around the client window.
    fn search(&self, title: &str, pid: u32) -> Result<Option<Window>> {
        let mut ids = Vec::new();
        let mut listed = false;
        for &root in &self.roots {
            let clients = self.client_list(root)?;
            listed |= !clients.is_empty();
            ids.extend(clients);
            ids.extend(self.children(root)?);
        }
        dedup(&mut ids);
        if let Some(window) = self.owned(&ids, title, pid)? {
            return Ok(Some(window));
        }
        if listed {
            return Ok(None);
        }
        let top = ids.clone();
        for window in top {
            ids.extend(self.children(window)?);
        }
        dedup(&mut ids);
        self.owned(&ids, title, pid)
    }

    fn owned(&self, windows: &[Window], title: &str, pid: u32) -> Result<Option<Window>> {
        for chunk in windows.chunks(80) {
            if let Some(window) = self.owned_chunk(chunk, title, pid)? {
                return Ok(Some(window));
            }
        }
        Ok(None)
    }

    fn owned_chunk(&self, windows: &[Window], title: &str, pid: u32) -> Result<Option<Window>> {
        let mut pending = Vec::with_capacity(windows.len());
        for &window in windows {
            let name = self
                .conn
                .get_property(
                    false,
                    window,
                    self.atoms.net_wm_name,
                    self.atoms.utf8,
                    0,
                    256,
                )
                .context("read a window title")?;
            let legacy = self
                .conn
                .get_property(false, window, AtomEnum::WM_NAME, AtomEnum::ANY, 0, 256)
                .context("read a window title")?;
            let window_pid = self
                .conn
                .get_property(
                    false,
                    window,
                    self.atoms.net_wm_pid,
                    AtomEnum::CARDINAL,
                    0,
                    1,
                )
                .context("read a window pid")?;
            pending.push((window, name, legacy, window_pid));
        }
        for (window, name, legacy, window_pid) in pending {
            let name = fetch(name.reply())?;
            let legacy = fetch(legacy.reply())?;
            let window_pid = fetch(window_pid.reply())?;
            let Some(name) = name else { continue };
            let Some(window_pid) = window_pid else {
                continue;
            };
            let bytes = if name
                .value8()
                .is_some_and(|mut bytes| bytes.any(|byte| byte != 0))
            {
                bytes_of(&name)
            } else {
                let Some(legacy) = legacy else { continue };
                bytes_of(&legacy)
            };
            let Some(got) = title_text(&bytes) else {
                continue;
            };
            let Some(got_pid) = window_pid.value32().and_then(|mut values| values.next()) else {
                continue;
            };
            if got == title && got_pid == pid {
                return Ok(Some(window));
            }
        }
        Ok(None)
    }

    fn identity(&self, window: Window) -> Result<Option<(String, Option<u32>)>> {
        let name = match self.bytes(window, self.atoms.net_wm_name, self.atoms.utf8)? {
            None => return Ok(None),
            Some(bytes) if bytes.iter().any(|byte| *byte != 0) => bytes,
            Some(_) => match self.bytes(window, AtomEnum::WM_NAME, AtomEnum::ANY)? {
                None => return Ok(None),
                Some(bytes) => bytes,
            },
        };
        let pid = match self.words(window, self.atoms.net_wm_pid, AtomEnum::CARDINAL)? {
            None => return Ok(None),
            Some(values) => values.first().copied(),
        };
        Ok(title_text(&name).map(|name| (name, pid)))
    }

    /// Ask the window manager to put `window` above other windows.
    ///
    /// The client message is the EWMH request and does not focus the window.
    /// With no window manager listening, restack directly. That request
    /// changes stacking only: not position, size, or focus.
    fn request_above(&self, window: Window) -> Result<()> {
        let Some(root) = self.root_of(window)? else {
            return Ok(());
        };
        let event = ClientMessageEvent::new(
            32,
            window,
            self.atoms.net_wm_state,
            above_message(self.atoms.net_wm_state_above),
        );
        checked(
            self.conn
                .send_event(
                    false,
                    root,
                    EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY,
                    event,
                )?
                .check(),
        )
        .context("ask for the strip to stay above other windows")?;
        if !self.wm_present()? {
            checked(
                self.conn
                    .configure_window(
                        window,
                        &ConfigureWindowAux::new().stack_mode(StackMode::ABOVE),
                    )?
                    .check(),
            )
            .context("raise the strip")?;
        }
        self.conn.flush().context("send the raise")?;
        Ok(())
    }

    fn wm_present(&self) -> Result<bool> {
        for &root in &self.roots {
            let Some(values) = self.words(root, self.atoms.net_supporting_wm, AtomEnum::WINDOW)?
            else {
                continue;
            };
            if values.first().copied().unwrap_or(0) != 0 {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn root_of(&self, window: Window) -> Result<Option<Window>> {
        match self.conn.query_tree(window)?.reply() {
            Ok(tree) => Ok(Some(tree.root)),
            Err(error) if bad_window(&error) => Ok(None),
            Err(error) => Err(anyhow!(error).context("find the strip's screen")),
        }
    }

    fn client_list(&self, root: Window) -> Result<Vec<Window>> {
        Ok(self
            .words(root, self.atoms.net_client_list, AtomEnum::WINDOW)?
            .unwrap_or_default())
    }

    fn children(&self, window: Window) -> Result<Vec<Window>> {
        match self.conn.query_tree(window)?.reply() {
            Ok(tree) => Ok(tree.children),
            Err(error) if bad_window(&error) => Ok(Vec::new()),
            Err(error) => Err(anyhow!(error).context("list windows")),
        }
    }

    /// `None` when `window` no longer exists. An empty property is `Some`.
    fn words(
        &self,
        window: Window,
        property: impl Into<Atom>,
        kind: impl Into<Atom>,
    ) -> Result<Option<Vec<u32>>> {
        match fetch(
            self.conn
                .get_property(false, window, property, kind, 0, 4096)?
                .reply(),
        )? {
            None => Ok(None),
            Some(reply) => Ok(Some(
                reply
                    .value32()
                    .map(|values| values.collect())
                    .unwrap_or_default(),
            )),
        }
    }

    fn bytes(
        &self,
        window: Window,
        property: impl Into<Atom>,
        kind: impl Into<Atom>,
    ) -> Result<Option<Vec<u8>>> {
        match fetch(
            self.conn
                .get_property(false, window, property, kind, 0, 256)?
                .reply(),
        )? {
            None => Ok(None),
            Some(reply) => Ok(Some(bytes_of(&reply))),
        }
    }
}

fn above_message(above: u32) -> [u32; 5] {
    [STATE_ADD, above, 0, SOURCE_APPLICATION, 0]
}

fn bytes_of(reply: &x11rb::protocol::xproto::GetPropertyReply) -> Vec<u8> {
    reply
        .value8()
        .map(|bytes| bytes.collect())
        .unwrap_or_default()
}

/// `None` when the window is gone. Other protocol errors still fail.
fn fetch(
    result: Result<x11rb::protocol::xproto::GetPropertyReply, ReplyError>,
) -> Result<Option<x11rb::protocol::xproto::GetPropertyReply>> {
    match result {
        Ok(reply) => Ok(Some(reply)),
        Err(error) if bad_window(&error) => Ok(None),
        Err(error) => Err(anyhow!(error).context("read an X11 property")),
    }
}

fn checked(result: Result<(), ReplyError>) -> Result<()> {
    match result {
        Ok(()) => Ok(()),
        Err(error) if bad_window(&error) => Ok(()),
        Err(error) => Err(anyhow!(error)),
    }
}

fn bad_window(error: &ReplyError) -> bool {
    matches!(error, ReplyError::X11Error(error) if error.error_kind == ErrorKind::Window)
}

fn title_text(bytes: &[u8]) -> Option<String> {
    let end = bytes
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(bytes.len());
    let bytes = &bytes[..end];
    if bytes.is_empty() {
        return None;
    }
    std::str::from_utf8(bytes).ok().map(str::to_owned)
}

fn dedup(ids: &mut Vec<Window>) {
    ids.sort_unstable();
    ids.dedup();
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::io::Read;
    use std::path::Path;
    use std::process::{Command, Stdio};
    use std::thread;
    use std::time::Duration;

    use x11rb::protocol::Event;
    use x11rb::protocol::xproto::{
        ChangeWindowAttributesAux, ConfigureWindowAux, ConnectionExt, CreateWindowAux, EventMask,
        InputFocus, PropMode, StackMode, WindowClass,
    };
    use x11rb::wrapper::ConnectionExt as _;
    use x11rb::{COPY_DEPTH_FROM_PARENT, CURRENT_TIME};

    #[test]
    fn x11_is_used_only_when_winit_would_use_it() {
        assert!(uses_x11(false, true));
        assert!(
            !uses_x11(true, true),
            "Wayland wins when both displays are set"
        );
        assert!(!uses_x11(true, false));
        assert!(!uses_x11(false, false));
    }

    #[test]
    fn the_above_request_adds_the_state_and_does_not_toggle_it() {
        assert_eq!(above_message(42), [1, 42, 0, 1, 0]);
    }

    #[test]
    fn titles_ignore_a_trailing_null_and_reject_other_text() {
        assert_eq!(
            title_text(b"OpenMic level\0").as_deref(),
            Some("OpenMic level")
        );
        assert_eq!(title_text(b"OpenMic hint").as_deref(), Some("OpenMic hint"));
        assert_eq!(title_text(b"\0hidden"), None);
        assert_eq!(title_text(b""), None);
        assert_eq!(title_text(&[0xff, 0xfe]), None);
    }

    struct Xvfb {
        child: std::process::Child,
        display: String,
    }

    impl Xvfb {
        fn start() -> Self {
            for number in 200..500 {
                let socket = format!("/tmp/.X11-unix/X{number}");
                let lock = format!("/tmp/.X{number}-lock");
                let claim = format!("/tmp/openmic-xvfb-{number}.lock");
                if Path::new(&socket).exists()
                    || Path::new(&lock).exists()
                    || Path::new(&claim).exists()
                {
                    continue;
                }
                // create_new so two tests cannot claim the same display.
                if std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&claim)
                    .is_err()
                {
                    continue;
                }
                let display = format!(":{number}");
                let mut child = match Command::new("Xvfb")
                    .args([
                        &display,
                        "-screen",
                        "0",
                        "640x480x24",
                        "-nolisten",
                        "tcp",
                        "-ac",
                        "-noreset",
                    ])
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::piped())
                    .spawn()
                {
                    Ok(child) => child,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        let _ = std::fs::remove_file(&claim);
                        panic!(
                            "Xvfb is required to test that the level strip stays above other windows"
                        );
                    }
                    Err(error) => {
                        let _ = std::fs::remove_file(&claim);
                        panic!("start Xvfb: {error}");
                    }
                };
                let deadline = Instant::now() + Duration::from_secs(3);
                loop {
                    // A dead child did not create this socket, even if one appears.
                    if child.try_wait().expect("Xvfb status").is_some() {
                        let mut err = String::new();
                        if let Some(stderr) = child.stderr.as_mut() {
                            let _ = stderr.read_to_string(&mut err);
                        }
                        let _ = child.wait();
                        let _ = std::fs::remove_file(&claim);
                        if err.contains("already active") || err.contains("already running") {
                            break;
                        }
                        panic!("Xvfb {display} exited: {err}");
                    }
                    if Path::new(&socket).exists() {
                        return Self { child, display };
                    }
                    if Instant::now() >= deadline {
                        let _ = child.kill();
                        let _ = child.wait();
                        let _ = std::fs::remove_file(&claim);
                        break;
                    }
                    thread::sleep(Duration::from_millis(20));
                }
            }
            panic!("no free X display for the level-strip test");
        }
    }

    impl Drop for Xvfb {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
            if let Some(number) = self.display.strip_prefix(':') {
                let _ = std::fs::remove_file(format!("/tmp/openmic-xvfb-{number}.lock"));
            }
        }
    }

    struct Fixture {
        conn: RustConnection,
        root: Window,
        utf8: Atom,
        net_wm_name: Atom,
        net_wm_pid: Atom,
        net_wm_state: Atom,
        net_wm_state_above: Atom,
        net_active: Atom,
        net_supporting_wm: Atom,
    }

    impl Fixture {
        fn open(display: &str) -> Self {
            let (conn, screen) =
                RustConnection::connect(Some(display)).expect("connect to the test display");
            let root = conn.setup().roots[screen].root;
            let atom = |name: &[u8]| conn.intern_atom(false, name).unwrap().reply().unwrap().atom;
            Self {
                net_wm_name: atom(b"_NET_WM_NAME"),
                utf8: atom(b"UTF8_STRING"),
                net_wm_pid: atom(b"_NET_WM_PID"),
                net_wm_state: atom(b"_NET_WM_STATE"),
                net_wm_state_above: atom(b"_NET_WM_STATE_ABOVE"),
                net_active: atom(b"_NET_ACTIVE_WINDOW"),
                net_supporting_wm: atom(b"_NET_SUPPORTING_WM_CHECK"),
                conn,
                root,
            }
        }

        fn window(&self, title: &str, pid: u32) -> Window {
            let screen = &self.conn.setup().roots[0];
            let window = self.conn.generate_id().unwrap();
            self.conn
                .create_window(
                    COPY_DEPTH_FROM_PARENT,
                    window,
                    self.root,
                    0,
                    0,
                    80,
                    34,
                    0,
                    WindowClass::INPUT_OUTPUT,
                    0,
                    &CreateWindowAux::new().background_pixel(screen.white_pixel),
                )
                .unwrap();
            self.conn
                .change_property8(
                    PropMode::REPLACE,
                    window,
                    self.net_wm_name,
                    self.utf8,
                    title.as_bytes(),
                )
                .unwrap();
            self.conn
                .change_property8(
                    PropMode::REPLACE,
                    window,
                    AtomEnum::WM_NAME,
                    AtomEnum::STRING,
                    title.as_bytes(),
                )
                .unwrap();
            self.conn
                .change_property32(
                    PropMode::REPLACE,
                    window,
                    self.net_wm_pid,
                    AtomEnum::CARDINAL,
                    &[pid],
                )
                .unwrap();
            self.conn.map_window(window).unwrap();
            window
        }

        fn stacking(&self) -> Vec<Window> {
            self.conn
                .query_tree(self.root)
                .unwrap()
                .reply()
                .unwrap()
                .children
        }

        fn focus(&self) -> Window {
            self.conn.get_input_focus().unwrap().reply().unwrap().focus
        }

        fn listen(&self) {
            self.conn
                .change_window_attributes(
                    self.root,
                    &ChangeWindowAttributesAux::new().event_mask(EventMask::SUBSTRUCTURE_NOTIFY),
                )
                .unwrap();
            self.conn.sync().unwrap();
            while self.conn.poll_for_event().unwrap().is_some() {}
        }

        fn next_client_message(&self) -> ClientMessageEvent {
            let deadline = Instant::now() + Duration::from_secs(2);
            while Instant::now() < deadline {
                match self.conn.poll_for_event().unwrap() {
                    Some(Event::ClientMessage(event)) => return event,
                    Some(_) => continue,
                    None => thread::sleep(Duration::from_millis(10)),
                }
            }
            panic!("no _NET_WM_STATE message arrived");
        }
    }

    fn index_of(stack: &[Window], window: Window) -> usize {
        stack
            .iter()
            .position(|id| *id == window)
            .unwrap_or_else(|| panic!("{window} missing from {stack:?}"))
    }

    /// Build the strip under another window, focus that window, and raise.
    fn place_strip(display: &str, wm: bool) -> (Fixture, Window, Window, Window) {
        let fix = Fixture::open(display);
        let ours = fix.window("OpenMic level", 42);
        let foreign = fix.window("OpenMic level", 99);
        let marker = fix.window("other", 7);
        fix.conn
            .configure_window(
                ours,
                &ConfigureWindowAux::new().stack_mode(StackMode::BELOW),
            )
            .unwrap()
            .check()
            .unwrap();
        fix.conn
            .change_property32(
                PropMode::REPLACE,
                fix.root,
                fix.net_active,
                AtomEnum::WINDOW,
                &[marker],
            )
            .unwrap();
        fix.conn
            .set_input_focus(InputFocus::POINTER_ROOT, marker, CURRENT_TIME)
            .unwrap();
        if wm {
            let claimed = fix.window("wm", 1);
            fix.conn
                .change_property32(
                    PropMode::REPLACE,
                    fix.root,
                    fix.net_supporting_wm,
                    AtomEnum::WINDOW,
                    &[claimed],
                )
                .unwrap();
            fix.conn
                .change_property32(
                    PropMode::REPLACE,
                    claimed,
                    fix.net_supporting_wm,
                    AtomEnum::WINDOW,
                    &[claimed],
                )
                .unwrap();
        }
        fix.conn.sync().unwrap();
        (fix, ours, foreign, marker)
    }

    #[test]
    fn without_a_window_manager_the_strip_is_raised_and_focus_stays() {
        let xvfb = Xvfb::start();
        let (fix, ours, foreign, marker) = place_strip(&xvfb.display, false);
        let before = fix.stacking();
        assert!(
            index_of(&before, ours) < index_of(&before, marker),
            "the strip starts underneath: {before:?}"
        );
        assert!(
            index_of(&before, foreign) < index_of(&before, marker),
            "the other OpenMic level starts underneath: {before:?}"
        );
        fix.listen();

        let mut server = Server::connect(Some(&xvfb.display)).unwrap();
        assert_eq!(server.active_window().unwrap(), Some(marker as isize));
        server.raise_titled("OpenMic level", 42).unwrap();
        // The cached window is raised again without scanning.
        server.raise_titled("OpenMic level", 42).unwrap();

        let after = fix.stacking();
        assert!(
            index_of(&after, ours) > index_of(&after, marker),
            "the strip is above the other window: {after:?}"
        );
        assert!(
            index_of(&after, foreign) < index_of(&after, marker),
            "another process with the same title stays underneath: {after:?}"
        );
        assert_eq!(fix.focus(), marker, "raising the strip does not take focus");
        let event = fix.next_client_message();
        assert_eq!(event.window, ours);
        assert_eq!(event.type_, fix.net_wm_state);
        assert_eq!(
            event.data.as_data32(),
            [STATE_ADD, fix.net_wm_state_above, 0, SOURCE_APPLICATION, 0]
        );
    }

    #[test]
    fn a_window_manager_is_asked_and_the_strip_is_not_restacked_directly() {
        let xvfb = Xvfb::start();
        let (fix, ours, _foreign, marker) = place_strip(&xvfb.display, true);
        let before = fix.stacking();
        fix.listen();
        let mut server = Server::connect(Some(&xvfb.display)).unwrap();
        assert!(server.wm_present().unwrap());
        server.raise_titled("OpenMic level", 42).unwrap();
        fix.conn.sync().unwrap();
        assert_eq!(
            fix.stacking(),
            before,
            "a window manager owns stacking; the client message is the request"
        );
        assert_eq!(fix.focus(), marker, "the request does not take focus");
        let event = fix.next_client_message();
        assert_eq!(event.window, ours);
        assert_eq!(
            event.data.as_data32(),
            above_message(fix.net_wm_state_above)
        );
    }
}
