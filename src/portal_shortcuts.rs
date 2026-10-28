//! Wayland global shortcuts through the XDG desktop portal.
//!
//! X11 grabs (what `global-hotkey` uses) do not see keys while a Wayland
//! window is focused. Compositors that ship `org.freedesktop.portal.GlobalShortcuts`
//! (KDE Plasma 6, GNOME 48, Hyprland's portal) deliver press and release
//! instead. The desktop confirms the set once. Binding the same ids again
//! usually does not ask a second time; an empty set closes the session so a
//! shortcut capture still reaches this window.
//!
//! The portal is probed only for a Wayland session. If it is missing, or does
//! not answer, the caller keeps the X11 grab.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use ashpd::desktop::Session;
use ashpd::desktop::global_shortcuts::{GlobalShortcuts, NewShortcut};
use crossbeam_channel::Sender;
use eframe::egui;
use futures_channel::mpsc::{self, UnboundedReceiver, UnboundedSender};
use futures_util::{StreamExt, select};
use global_hotkey::hotkey::{Code, HotKey, Modifiers};

use crate::gui::Command;
use crate::logfile;

const APP_ID: &str = "com.github.DevanMetz.OpenMic";
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// Whether this process should try the portal before an X11 grab.
///
/// `wayland_display` is `WAYLAND_DISPLAY` being non-empty. `session_type` is
/// `XDG_SESSION_TYPE`. `winit_backend` is `WINIT_UNIX_BACKEND` (`x11` forces
/// the X11 backend even on a Wayland desktop).
pub(crate) fn wants_global_shortcut_portal(
    wayland_display: bool,
    session_type: Option<&str>,
    winit_backend: Option<&str>,
) -> bool {
    if winit_backend.is_some_and(|backend| backend.eq_ignore_ascii_case("x11")) {
        return false;
    }
    if session_type.is_some_and(|kind| {
        let kind = kind.to_ascii_lowercase();
        kind == "x11" || kind == "tty"
    }) {
        return false;
    }
    wayland_display
}

pub(crate) fn session_wants_portal() -> bool {
    let wayland = std::env::var_os("WAYLAND_DISPLAY").is_some_and(|value| !value.is_empty());
    let session = std::env::var("XDG_SESSION_TYPE").ok();
    let backend = std::env::var("WINIT_UNIX_BACKEND").ok();
    wants_global_shortcut_portal(wayland, session.as_deref(), backend.as_deref())
}

/// Portal trigger for a stored shortcut, such as `CTRL+ALT+m`.
///
/// Modifiers are emitted in portal order. The key is an XKB keysym name.
/// `None` when the text is not a single shortcut or the key has no keysym.
pub(crate) fn portal_trigger(shortcut: &str) -> Option<String> {
    let hotkey = shortcut.parse::<HotKey>().ok()?;
    let key = xkb_name(hotkey.key)?;
    let mut parts = Vec::new();
    if hotkey.mods.contains(Modifiers::CONTROL) {
        parts.push("CTRL");
    }
    if hotkey.mods.contains(Modifiers::ALT) {
        parts.push("ALT");
    }
    if hotkey.mods.contains(Modifiers::SHIFT) {
        parts.push("SHIFT");
    }
    if hotkey.mods.contains(Modifiers::SUPER) {
        parts.push("LOGO");
    }
    parts.push(key);
    Some(parts.join("+"))
}

fn xkb_name(key: Code) -> Option<&'static str> {
    Some(match key {
        Code::KeyA => "a",
        Code::KeyB => "b",
        Code::KeyC => "c",
        Code::KeyD => "d",
        Code::KeyE => "e",
        Code::KeyF => "f",
        Code::KeyG => "g",
        Code::KeyH => "h",
        Code::KeyI => "i",
        Code::KeyJ => "j",
        Code::KeyK => "k",
        Code::KeyL => "l",
        Code::KeyM => "m",
        Code::KeyN => "n",
        Code::KeyO => "o",
        Code::KeyP => "p",
        Code::KeyQ => "q",
        Code::KeyR => "r",
        Code::KeyS => "s",
        Code::KeyT => "t",
        Code::KeyU => "u",
        Code::KeyV => "v",
        Code::KeyW => "w",
        Code::KeyX => "x",
        Code::KeyY => "y",
        Code::KeyZ => "z",
        Code::Digit0 => "0",
        Code::Digit1 => "1",
        Code::Digit2 => "2",
        Code::Digit3 => "3",
        Code::Digit4 => "4",
        Code::Digit5 => "5",
        Code::Digit6 => "6",
        Code::Digit7 => "7",
        Code::Digit8 => "8",
        Code::Digit9 => "9",
        Code::F1 => "F1",
        Code::F2 => "F2",
        Code::F3 => "F3",
        Code::F4 => "F4",
        Code::F5 => "F5",
        Code::F6 => "F6",
        Code::F7 => "F7",
        Code::F8 => "F8",
        Code::F9 => "F9",
        Code::F10 => "F10",
        Code::F11 => "F11",
        Code::F12 => "F12",
        Code::F13 => "F13",
        Code::F14 => "F14",
        Code::F15 => "F15",
        Code::F16 => "F16",
        Code::F17 => "F17",
        Code::F18 => "F18",
        Code::F19 => "F19",
        Code::F20 => "F20",
        Code::F21 => "F21",
        Code::F22 => "F22",
        Code::F23 => "F23",
        Code::F24 => "F24",
        Code::Equal => "equal",
        Code::Backquote => "grave",
        Code::BracketLeft => "bracketleft",
        Code::BracketRight => "bracketright",
        Code::Minus => "minus",
        Code::Comma => "comma",
        Code::Period => "period",
        Code::Slash => "slash",
        Code::Backslash => "backslash",
        Code::Semicolon => "semicolon",
        Code::Quote => "apostrophe",
        Code::Space => "space",
        Code::Enter => "Return",
        Code::Tab => "Tab",
        Code::Escape => "Escape",
        Code::Backspace => "BackSpace",
        Code::Delete => "Delete",
        Code::Home => "Home",
        Code::End => "End",
        Code::PageUp => "Page_Up",
        Code::PageDown => "Page_Down",
        Code::Insert => "Insert",
        Code::ArrowUp => "Up",
        Code::ArrowDown => "Down",
        Code::ArrowLeft => "Left",
        Code::ArrowRight => "Right",
        Code::Pause => "Pause",
        _ => return None,
    })
}

/// Stable portal id. Pads use FNV-1a of the path so a restart binds the same
/// shortcut and the desktop can remember the grant.
pub(crate) fn shortcut_id(command: &Command) -> Option<String> {
    Some(match command {
        Command::ToggleMute => "mute".to_owned(),
        Command::ToggleBypass => "bypass".to_owned(),
        Command::StopClips => "stop-clips".to_owned(),
        Command::Dictate => "dictate".to_owned(),
        Command::PlayPad(path) => {
            format!("pad-{:016x}", fnv1a64(path.to_string_lossy().as_bytes()))
        }
        _ => return None,
    })
}

fn shortcut_description(command: &Command) -> String {
    match command {
        Command::ToggleMute => "Mute microphone".to_owned(),
        Command::ToggleBypass => "Bypass processing".to_owned(),
        Command::StopClips => "Stop all clips".to_owned(),
        Command::Dictate => "Speech to text".to_owned(),
        Command::PlayPad(path) => {
            let stem = path
                .file_stem()
                .and_then(|name| name.to_str())
                .unwrap_or("clip");
            format!("Play {stem}")
        }
        _ => String::new(),
    }
}

fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

/// One shortcut the portal should bind.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PortalBinding {
    id: String,
    trigger: String,
    description: String,
    command: Command,
}

/// Split `bindings` into shortcuts the portal can bind and texts it cannot.
///
/// `actions` is keyed by the same hotkey id the X11 path uses, so the window
/// can show a refused shortcut without waiting for the desktop dialog.
/// Two pads whose paths hash to one id: the later one is refused.
pub(crate) fn plan(
    bindings: &[(String, Command)],
) -> (HashMap<u32, Command>, Vec<PortalBinding>, Vec<String>) {
    let mut actions = HashMap::new();
    let mut planned = Vec::new();
    let mut refused = Vec::new();
    let mut seen = HashSet::new();
    for (text, command) in bindings {
        let Ok(hotkey) = text.parse::<HotKey>() else {
            refused.push(text.clone());
            continue;
        };
        let Some(trigger) = portal_trigger(text) else {
            refused.push(text.clone());
            continue;
        };
        let Some(id) = shortcut_id(command) else {
            refused.push(text.clone());
            continue;
        };
        if !seen.insert(id.clone()) {
            refused.push(text.clone());
            continue;
        }
        actions.insert(hotkey.id(), command.clone());
        planned.push(PortalBinding {
            id,
            trigger,
            description: shortcut_description(command),
            command: command.clone(),
        });
    }
    (actions, planned, refused)
}

enum Probe {
    Ready,
    Missing,
    TimedOut,
}

/// `Ok(None)` means the portal is not usable and the X11 grab should be used.
/// The reason is already logged.
pub(crate) fn connect(
    ctx: &egui::Context,
    commands: Sender<Command>,
) -> Result<Option<PortalShortcuts>> {
    match probe() {
        Probe::Ready => start(ctx.clone(), commands, Hook::default()).map(Some),
        Probe::Missing => {
            logfile::warn(format_args!(
                "Wayland global-shortcuts portal is not available; shortcuts only reach X11 windows"
            ));
            Ok(None)
        }
        Probe::TimedOut => {
            logfile::warn(format_args!(
                "Wayland global-shortcuts portal did not answer; shortcuts only reach X11 windows"
            ));
            Ok(None)
        }
    }
}

fn probe() -> Probe {
    let (tx, rx) = crossbeam_channel::bounded(1);
    let spawned = std::thread::Builder::new()
        .name("openmic-shortcuts-probe".into())
        .spawn(move || {
            let present = pollster::block_on(portal_present());
            let _ = tx.send(present);
        });
    if spawned.is_err() {
        return Probe::Missing;
    }
    match rx.recv_timeout(PROBE_TIMEOUT) {
        Ok(true) => Probe::Ready,
        Ok(false) => Probe::Missing,
        Err(_) => Probe::TimedOut,
    }
}

/// `GlobalShortcuts::new` reports success when the bus name is merely absent
/// (the version read is ignored). Introspection is the check that does not
/// open a dialog.
async fn portal_present() -> bool {
    let Ok(portal) = GlobalShortcuts::new().await else {
        return false;
    };
    portal
        .introspect()
        .await
        .is_ok_and(|xml| xml.contains("org.freedesktop.portal.GlobalShortcuts"))
}

#[derive(Default)]
struct Hook {
    errors: Option<Arc<std::sync::Mutex<Option<String>>>>,
    done: Option<Arc<std::sync::atomic::AtomicBool>>,
    /// Bumped after each bind attempt. Tests wait on it; the running app does not.
    generation: Arc<AtomicU64>,
    /// Bumped after each portal press or release. Tests wait on it.
    handled: Arc<AtomicU64>,
}

enum Cmd {
    Set(Vec<PortalBinding>),
    Shutdown,
}

/// Portal session. Drop asks the worker to release held keys and close it.
/// The worker is not joined: a desktop dialog must not freeze shutdown.
pub(crate) struct PortalShortcuts {
    tx: UnboundedSender<Cmd>,
}

impl PortalShortcuts {
    pub(crate) fn replace(&self, bindings: Vec<PortalBinding>) {
        if self.tx.unbounded_send(Cmd::Set(bindings)).is_err() {
            logfile::warn(format_args!("Wayland global shortcuts are not running"));
        }
    }
}

impl Drop for PortalShortcuts {
    fn drop(&mut self) {
        let _ = self.tx.unbounded_send(Cmd::Shutdown);
    }
}

fn start(ctx: egui::Context, commands: Sender<Command>, hook: Hook) -> Result<PortalShortcuts> {
    let (tx, rx) = mpsc::unbounded();
    let generation = Arc::clone(&hook.generation);
    let handled = Arc::clone(&hook.handled);
    std::thread::Builder::new()
        .name("openmic-shortcuts".into())
        .spawn(move || {
            let result = pollster::block_on(serve(rx, commands, ctx, &generation, &handled, &hook));
            if let Err(err) = result {
                note(&hook, &err);
                logfile::warn(format_args!("Wayland global shortcuts stopped: {err:#}"));
            }
            if let Some(done) = &hook.done {
                done.store(true, Ordering::SeqCst);
            }
        })
        .context("start Wayland global shortcuts")?;
    Ok(PortalShortcuts { tx })
}

fn note(hook: &Hook, err: &impl std::fmt::Display) {
    if let Some(slot) = &hook.errors {
        *slot.lock().unwrap_or_else(|poison| poison.into_inner()) = Some(err.to_string());
    }
}

async fn serve(
    incoming: UnboundedReceiver<Cmd>,
    commands: Sender<Command>,
    ctx: egui::Context,
    generation: &AtomicU64,
    handled: &AtomicU64,
    hook: &Hook,
) -> Result<()> {
    if let Err(err) = register_app().await {
        logfile::warn(format_args!(
            "desktop portal did not record the app id ({err})"
        ));
    }
    let portal = GlobalShortcuts::new()
        .await
        .context("open the global-shortcuts portal")?;
    // The signal streams borrow the proxy. Rebinding only needs shared access.
    let mut activated = portal
        .receive_activated()
        .await
        .context("listen for shortcut presses")?
        .fuse();
    let mut deactivated = portal
        .receive_deactivated()
        .await
        .context("listen for shortcut releases")?
        .fuse();
    let mut incoming = incoming.fuse();

    let mut session: Option<Session<'_, GlobalShortcuts<'_>>> = None;
    let mut bound: HashMap<String, Command> = HashMap::new();
    let mut pressed: HashMap<String, Command> = HashMap::new();
    let mut desired: Vec<PortalBinding> = Vec::new();
    let mut applied: Vec<PortalBinding> = Vec::new();

    loop {
        if desired != applied {
            release_pressed(&mut pressed, &commands, &ctx);
            close_session(&mut session).await;
            bound.clear();
            if desired.is_empty() {
                applied.clear();
            } else if let Err(err) = bind_session(&portal, &desired, &mut session, &mut bound).await
            {
                note(hook, &err);
                logfile::warn(format_args!("Wayland shortcuts were not bound ({err:#})"));
                // Mark the set applied so the next frame does not open the
                // dialog again. A later edit, or the next launch, tries once more.
                applied = desired.clone();
                session = None;
                bound.clear();
            } else {
                logfile::info(format_args!(
                    "bound {} Wayland shortcut(s); the desktop may ask for confirmation once",
                    desired.len()
                ));
                applied = desired.clone();
            }
            generation.fetch_add(1, Ordering::SeqCst);
            continue;
        }

        select! {
            cmd = incoming.next() => {
                let Some(cmd) = cmd else { break };
                match cmd {
                    Cmd::Set(next) => desired = next,
                    Cmd::Shutdown => break,
                }
            }
            event = activated.next() => {
                let Some(event) = event else {
                    logfile::warn(format_args!("Wayland global shortcuts stopped listening"));
                    break;
                };
                on_activated(&event, session.as_ref(), &bound, &mut pressed, &commands, &ctx);
                handled.fetch_add(1, Ordering::SeqCst);
            }
            event = deactivated.next() => {
                let Some(event) = event else {
                    logfile::warn(format_args!("Wayland global shortcuts stopped listening"));
                    break;
                };
                on_deactivated(&event, session.as_ref(), &mut pressed, &commands, &ctx);
                handled.fetch_add(1, Ordering::SeqCst);
            }
            complete => break,
        }
    }

    release_pressed(&mut pressed, &commands, &ctx);
    close_session(&mut session).await;
    Ok(())
}

async fn register_app() -> ashpd::Result<()> {
    let app_id = ashpd::AppID::try_from(APP_ID).expect("OpenMic app id is valid");
    ashpd::register_host_app(app_id).await
}

async fn close_session(session: &mut Option<Session<'_, GlobalShortcuts<'_>>>) {
    if let Some(session) = session.take()
        && let Err(err) = session.close().await
    {
        logfile::warn(format_args!(
            "Wayland shortcut session was not closed ({err})"
        ));
    }
}

async fn bind_session<'a>(
    portal: &GlobalShortcuts<'a>,
    desired: &[PortalBinding],
    session: &mut Option<Session<'a, GlobalShortcuts<'a>>>,
    bound: &mut HashMap<String, Command>,
) -> Result<()> {
    let opened = portal
        .create_session()
        .await
        .context("create shortcut session")?;
    let shortcuts: Vec<NewShortcut> = desired
        .iter()
        .map(|binding| {
            NewShortcut::new(binding.id.clone(), binding.description.clone())
                .preferred_trigger(binding.trigger.as_str())
        })
        .collect();
    let bound_shortcuts = match portal.bind_shortcuts(&opened, &shortcuts, None).await {
        Ok(request) => request,
        Err(err) => {
            let _ = opened.close().await;
            return Err(err).context("bind shortcuts");
        }
    };
    let response = match bound_shortcuts.response() {
        Ok(response) => response,
        Err(err) => {
            let _ = opened.close().await;
            return Err(err).context("shortcut bind was refused");
        }
    };
    if response.shortcuts().is_empty() {
        let _ = opened.close().await;
        bail!("desktop returned no shortcuts");
    }
    for binding in desired {
        bound.insert(binding.id.clone(), binding.command.clone());
    }
    *session = Some(opened);
    Ok(())
}

fn on_activated(
    event: &ashpd::desktop::global_shortcuts::Activated,
    session: Option<&Session<'_, GlobalShortcuts<'_>>>,
    bound: &HashMap<String, Command>,
    pressed: &mut HashMap<String, Command>,
    commands: &Sender<Command>,
    ctx: &egui::Context,
) {
    if !same_session(session, event.session_handle().as_str()) {
        return;
    }
    let Some(command) = bound.get(event.shortcut_id()).cloned() else {
        return;
    };
    pressed.insert(event.shortcut_id().to_owned(), command.clone());
    let _ = commands.send(command);
    ctx.request_repaint();
}

fn on_deactivated(
    event: &ashpd::desktop::global_shortcuts::Deactivated,
    session: Option<&Session<'_, GlobalShortcuts<'_>>>,
    pressed: &mut HashMap<String, Command>,
    commands: &Sender<Command>,
    ctx: &egui::Context,
) {
    if !same_session(session, event.session_handle().as_str()) {
        return;
    }
    let Some(command) = pressed.remove(event.shortcut_id()) else {
        return;
    };
    if let Some(released) = command.released() {
        let _ = commands.send(released);
        ctx.request_repaint();
    }
}

fn same_session(session: Option<&Session<'_, GlobalShortcuts<'_>>>, handle: &str) -> bool {
    session.is_some_and(|session| session_handle(session).as_deref() == Some(handle))
}

/// `Session::path` is private. Its `Debug` is `Session("/object/path")`, and
/// an object path has no quotes or backslashes, so this is the handle the
/// Activated signal carries.
fn session_handle(session: &Session<'_, GlobalShortcuts<'_>>) -> Option<String> {
    let text = format!("{session:?}");
    let path = text.strip_prefix("Session(\"")?.strip_suffix("\")")?;
    Some(path.to_owned())
}

fn release_pressed(
    pressed: &mut HashMap<String, Command>,
    commands: &Sender<Command>,
    ctx: &egui::Context,
) {
    for command in pressed.drain().map(|(_, command)| command) {
        if let Some(released) = command.released() {
            let _ = commands.send(released);
            ctx.request_repaint();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn triggers_use_portal_modifier_order_and_keysyms() {
        assert_eq!(portal_trigger("Ctrl+Alt+M").as_deref(), Some("CTRL+ALT+m"));
        assert_eq!(
            portal_trigger("Ctrl+Shift+M").as_deref(),
            Some("CTRL+SHIFT+m")
        );
        assert_eq!(
            portal_trigger("Shift+Ctrl+Alt+M").as_deref(),
            Some("CTRL+ALT+SHIFT+m")
        );
        assert_eq!(portal_trigger("Alt+Equal").as_deref(), Some("ALT+equal"));
        assert_eq!(portal_trigger("F13").as_deref(), Some("F13"));
        assert_eq!(portal_trigger("Ctrl+Alt+1").as_deref(), Some("CTRL+ALT+1"));
        assert_eq!(
            portal_trigger("control+alt+Digit1").as_deref(),
            Some("CTRL+ALT+1")
        );
        assert_eq!(portal_trigger("Backquote").as_deref(), Some("grave"));
        assert_eq!(portal_trigger("super+KeyA").as_deref(), Some("LOGO+a"));
        assert_eq!(
            portal_trigger("super+ctrl+SHIFT+alt+ArrowUp").as_deref(),
            Some("CTRL+ALT+SHIFT+LOGO+Up")
        );
        assert_eq!(portal_trigger("Ctrl+Alt+M+K"), None);
        assert_eq!(portal_trigger(""), None);
        assert_eq!(portal_trigger("Ctrl+NotAKey"), None);
    }

    #[test]
    fn shortcut_ids_stay_stable_and_pads_do_not_share_one() {
        assert_eq!(shortcut_id(&Command::ToggleMute).as_deref(), Some("mute"));
        assert_eq!(
            shortcut_id(&Command::ToggleBypass).as_deref(),
            Some("bypass")
        );
        assert_eq!(
            shortcut_id(&Command::StopClips).as_deref(),
            Some("stop-clips")
        );
        assert_eq!(shortcut_id(&Command::Dictate).as_deref(), Some("dictate"));
        assert_eq!(shortcut_id(&Command::Quit), None);
        let again = shortcut_id(&Command::PlayPad("pads/a.wav".into())).unwrap();
        assert_eq!(
            shortcut_id(&Command::PlayPad("pads/a.wav".into())).as_deref(),
            Some(again.as_str())
        );
        assert_ne!(
            shortcut_id(&Command::PlayPad("pads/b.wav".into())).as_deref(),
            Some(again.as_str())
        );
        assert!(again.starts_with("pad-") && again.len() == "pad-".len() + 16);
        assert_eq!(
            shortcut_description(&Command::PlayPad("pads/a.wav".into())),
            "Play a"
        );
    }

    #[test]
    fn plan_refuses_a_duplicate_pad_id_and_an_unusable_shortcut() {
        let pad = Command::PlayPad("a.wav".into());
        let (actions, planned, refused) = plan(&[
            ("Ctrl+Alt+1".into(), pad.clone()),
            ("Ctrl+Alt+2".into(), pad),
            ("Ctrl+Nope".into(), Command::ToggleMute),
            ("Ctrl+Alt+M".into(), Command::ToggleMute),
        ]);
        assert_eq!(planned.len(), 2);
        assert_eq!(
            planned[0].id,
            shortcut_id(&Command::PlayPad("a.wav".into())).unwrap()
        );
        assert_eq!(planned[1].id, "mute");
        assert_eq!(planned[1].trigger, "CTRL+ALT+m");
        assert_eq!(
            refused,
            vec!["Ctrl+Alt+2".to_string(), "Ctrl+Nope".to_string()]
        );
        assert_eq!(actions.len(), 2);
        assert!(
            !actions
                .values()
                .any(|command| command == &Command::PlayPad("missing.wav".into()))
        );
    }

    #[test]
    fn wayland_session_uses_the_portal_and_x11_does_not() {
        assert!(!wants_global_shortcut_portal(true, Some("x11"), None));
        assert!(wants_global_shortcut_portal(true, Some("wayland"), None));
        assert!(wants_global_shortcut_portal(true, None, None));
        assert!(!wants_global_shortcut_portal(false, Some("wayland"), None));
        assert!(!wants_global_shortcut_portal(
            true,
            Some("wayland"),
            Some("x11")
        ));
        assert!(!wants_global_shortcut_portal(true, Some("tty"), None));
        assert!(!wants_global_shortcut_portal(true, Some("X11"), None));
        assert!(wants_global_shortcut_portal(
            true,
            Some("Wayland"),
            Some("wayland")
        ));
    }

    #[test]
    fn portal_activation_delivers_press_and_dictation_release() {
        let bus = PrivateBus::start();
        let desktop = FakePortal::serve();
        let errors = Arc::new(std::sync::Mutex::new(None));
        let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let hook = Hook {
            errors: Some(Arc::clone(&errors)),
            done: Some(Arc::clone(&done)),
            ..Hook::default()
        };
        let generation = Arc::clone(&hook.generation);
        let handled = Arc::clone(&hook.handled);
        let (tx, rx) = crossbeam_channel::unbounded();
        let portal = start(egui::Context::default(), tx, hook).unwrap();
        let (actions, planned, refused) = plan(&[
            ("Ctrl+Alt+M".into(), Command::ToggleMute),
            ("Ctrl+Alt+D".into(), Command::Dictate),
        ]);
        assert!(refused.is_empty(), "{refused:?}");
        assert_eq!(actions.len(), 2);
        let seen_generation = generation.load(Ordering::SeqCst);
        portal.replace(planned);
        wait_until(
            Duration::from_secs(5),
            || generation.load(Ordering::SeqCst) != seen_generation,
            &errors,
            "bind",
        );
        let grant = desktop.grant();

        let seen = handled.load(Ordering::SeqCst);
        emit(&desktop.conn, &grant, "Activated", "mute");
        wait_handled(&handled, seen, &errors);
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(2)).unwrap(),
            Command::ToggleMute
        );

        let seen = handled.load(Ordering::SeqCst);
        emit(&desktop.conn, &grant, "Deactivated", "mute");
        wait_handled(&handled, seen, &errors);
        assert!(rx.try_recv().is_err(), "mute has no release command");

        let seen = handled.load(Ordering::SeqCst);
        emit(&desktop.conn, &grant, "Activated", "dictate");
        wait_handled(&handled, seen, &errors);
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(2)).unwrap(),
            Command::Dictate
        );

        let seen = handled.load(Ordering::SeqCst);
        emit(&desktop.conn, &grant, "Deactivated", "dictate");
        wait_handled(&handled, seen, &errors);
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(2)).unwrap(),
            Command::DictateReleased
        );

        drop(portal);
        let started = std::time::Instant::now();
        while !done.load(Ordering::SeqCst) && started.elapsed() < Duration::from_secs(2) {
            std::thread::sleep(Duration::from_millis(20));
        }
        drop(desktop);
        drop(bus);
    }

    fn wait_handled(handled: &AtomicU64, seen: u64, errors: &std::sync::Mutex<Option<String>>) {
        // `seen` is the count before this signal. The fake portal emits one at a time.
        wait_until(
            Duration::from_secs(2),
            || handled.load(Ordering::SeqCst) > seen,
            errors,
            "signal",
        );
    }

    fn wait_until(
        limit: Duration,
        mut ready: impl FnMut() -> bool,
        errors: &std::sync::Mutex<Option<String>>,
        what: &str,
    ) {
        let started = std::time::Instant::now();
        while !ready() {
            if started.elapsed() > limit {
                let err = errors
                    .lock()
                    .unwrap_or_else(|poison| poison.into_inner())
                    .clone();
                panic!("timed out waiting for the portal to {what}: {err:?}");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn emit(conn: &zbus::Connection, grant: &Grant, signal: &str, shortcut: &str) {
        let path = zbus::zvariant::OwnedObjectPath::try_from(grant.session.as_str()).unwrap();
        let body = (
            path,
            shortcut,
            0u64,
            HashMap::<&str, zbus::zvariant::Value>::new(),
        );
        pollster::block_on(conn.emit_signal(
            Some(grant.sender.as_str()),
            "/org/freedesktop/portal/desktop",
            "org.freedesktop.portal.GlobalShortcuts",
            signal,
            &body,
        ))
        .unwrap_or_else(|err| panic!("emit {signal}: {err}"));
    }

    struct Grant {
        session: String,
        sender: String,
    }

    struct FakePortal {
        conn: zbus::Connection,
        grants: Arc<std::sync::Mutex<Option<Grant>>>,
    }

    impl FakePortal {
        fn serve() -> Self {
            let grants = Arc::new(std::sync::Mutex::new(None));
            let portal = PortalIface {
                grants: Arc::clone(&grants),
            };
            let conn = pollster::block_on(async {
                zbus::connection::Builder::session()
                    .unwrap()
                    .name("org.freedesktop.portal.Desktop")
                    .unwrap()
                    .serve_at("/org/freedesktop/portal/desktop", portal)
                    .unwrap()
                    .build()
                    .await
                    .unwrap()
            });
            Self { conn, grants }
        }

        fn grant(&self) -> Grant {
            let slot = self
                .grants
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            let grant = slot.as_ref().expect("portal bind did not record a session");
            Grant {
                session: grant.session.clone(),
                sender: grant.sender.clone(),
            }
        }
    }

    struct PortalIface {
        grants: Arc<std::sync::Mutex<Option<Grant>>>,
    }

    #[zbus::interface(name = "org.freedesktop.portal.GlobalShortcuts")]
    impl PortalIface {
        async fn create_session(
            &self,
            options: HashMap<String, zbus::zvariant::OwnedValue>,
            #[zbus(header)] header: zbus::message::Header<'_>,
            #[zbus(connection)] conn: &zbus::Connection,
            #[zbus(object_server)] server: &zbus::ObjectServer,
        ) -> zbus::fdo::Result<zbus::zvariant::OwnedObjectPath> {
            let sender = header
                .sender()
                .ok_or_else(|| zbus::fdo::Error::Failed("no sender".into()))?;
            let unique = sender_token(sender.as_str());
            let handle = dict_string(&options, "handle_token")?;
            let session_token = dict_string(&options, "session_handle_token")?;
            let session =
                format!("/org/freedesktop/portal/desktop/session/{unique}/{session_token}");
            let request = format!("/org/freedesktop/portal/desktop/request/{unique}/{handle}");
            server
                .at(session.as_str(), SessionIface)
                .await
                .map_err(|err| zbus::fdo::Error::Failed(err.to_string()))?;
            *self
                .grants
                .lock()
                .unwrap_or_else(|poison| poison.into_inner()) = Some(Grant {
                session: session.clone(),
                sender: sender.as_str().to_owned(),
            });
            respond(conn, sender.as_str(), &request, session_body(&session)).await?;
            zbus::zvariant::OwnedObjectPath::try_from(request.as_str())
                .map_err(|err| zbus::fdo::Error::Failed(err.to_string()))
        }

        async fn bind_shortcuts(
            &self,
            session_handle: zbus::zvariant::OwnedObjectPath,
            shortcuts: Vec<(String, HashMap<String, zbus::zvariant::OwnedValue>)>,
            _parent_window: String,
            options: HashMap<String, zbus::zvariant::OwnedValue>,
            #[zbus(header)] header: zbus::message::Header<'_>,
            #[zbus(connection)] conn: &zbus::Connection,
        ) -> zbus::fdo::Result<zbus::zvariant::OwnedObjectPath> {
            let sender = header
                .sender()
                .ok_or_else(|| zbus::fdo::Error::Failed("no sender".into()))?;
            let unique = sender_token(sender.as_str());
            let handle = dict_string(&options, "handle_token")?;
            let request = format!("/org/freedesktop/portal/desktop/request/{unique}/{handle}");
            let session = session_handle.as_str().to_owned();
            *self
                .grants
                .lock()
                .unwrap_or_else(|poison| poison.into_inner()) = Some(Grant {
                session: session.clone(),
                sender: sender.as_str().to_owned(),
            });
            respond(conn, sender.as_str(), &request, shortcuts_body(&shortcuts)).await?;
            zbus::zvariant::OwnedObjectPath::try_from(request.as_str())
                .map_err(|err| zbus::fdo::Error::Failed(err.to_string()))
        }

        #[zbus(property)]
        fn version(&self) -> u32 {
            1
        }
    }

    struct SessionIface;

    #[zbus::interface(name = "org.freedesktop.portal.Session")]
    impl SessionIface {
        fn close(&self) {}
    }

    fn sender_token(unique: &str) -> String {
        unique.trim_start_matches(':').replace('.', "_")
    }

    fn dict_string(
        options: &HashMap<String, zbus::zvariant::OwnedValue>,
        key: &str,
    ) -> zbus::fdo::Result<String> {
        let value = options
            .get(key)
            .ok_or_else(|| zbus::fdo::Error::InvalidArgs(format!("missing {key}")))?;
        if let Ok(text) = value.downcast_ref::<String>() {
            return Ok(text);
        }
        value
            .downcast_ref::<&str>()
            .map(str::to_owned)
            .map_err(|_| zbus::fdo::Error::InvalidArgs(format!("{key} is not a string")))
    }

    fn session_body(session: &str) -> HashMap<&str, zbus::zvariant::Value<'_>> {
        HashMap::from([(
            "session_handle",
            zbus::zvariant::Value::new(session.to_owned()),
        )])
    }

    fn shortcuts_body(
        shortcuts: &[(String, HashMap<String, zbus::zvariant::OwnedValue>)],
    ) -> HashMap<&str, zbus::zvariant::Value<'_>> {
        let echoed: Vec<(String, HashMap<&str, zbus::zvariant::Value<'_>>)> = shortcuts
            .iter()
            .map(|(id, info)| {
                let description = info
                    .get("description")
                    .and_then(|value| value.downcast_ref::<String>().ok())
                    .unwrap_or_else(|| "shortcut".to_owned());
                let trigger = info
                    .get("preferred_trigger")
                    .and_then(|value| value.downcast_ref::<String>().ok())
                    .unwrap_or_default();
                let fields = HashMap::from([
                    ("description", zbus::zvariant::Value::new(description)),
                    ("trigger_description", zbus::zvariant::Value::new(trigger)),
                ]);
                (id.clone(), fields)
            })
            .collect();
        HashMap::from([("shortcuts", zbus::zvariant::Value::new(echoed))])
    }

    async fn respond(
        conn: &zbus::Connection,
        destination: &str,
        request: &str,
        results: HashMap<&str, zbus::zvariant::Value<'_>>,
    ) -> zbus::fdo::Result<()> {
        let body = (0u32, results);
        conn.emit_signal(
            Some(destination),
            request,
            "org.freedesktop.portal.Request",
            "Response",
            &body,
        )
        .await
        .map_err(|err| zbus::fdo::Error::Failed(err.to_string()))
    }

    struct PrivateBus {
        child: std::process::Child,
        previous: Option<std::ffi::OsString>,
        _dir: tempfile::TempDir,
    }

    impl PrivateBus {
        fn start() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let config_path = dir.path().join("session.conf");
            let listen = dir.path().join("bus");
            std::fs::write(
                &config_path,
                format!(
                    "<busconfig>\n  <type>session</type>\n  <listen>unix:path={}</listen>\n  <auth>EXTERNAL</auth>\n  <policy context=\"default\">\n    <allow own=\"*\"/>\n    <allow send_destination=\"*\"/>\n    <allow receive_sender=\"*\"/>\n    <allow eavesdrop=\"true\"/>\n    <allow send_type=\"signal\"/>\n    <allow send_type=\"method_call\"/>\n    <allow send_type=\"method_return\"/>\n    <allow send_type=\"error\"/>\n  </policy>\n</busconfig>\n",
                    listen.display()
                ),
            )
            .unwrap();
            let mut child = std::process::Command::new("dbus-daemon")
                .args(["--nofork", "--config-file"])
                .arg(&config_path)
                .arg("--print-address")
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .expect("dbus-daemon is required to test the global-shortcuts portal");
            let stdout = child.stdout.take().unwrap();
            let mut stderr = child.stderr.take().unwrap();
            let errors = Arc::new(std::sync::Mutex::new(String::new()));
            let logged = Arc::clone(&errors);
            std::thread::spawn(move || {
                use std::io::Read;
                let mut text = String::new();
                let _ = stderr.read_to_string(&mut text);
                *logged.lock().unwrap() = text;
            });
            let (tx, rx) = crossbeam_channel::bounded(1);
            std::thread::spawn(move || {
                use std::io::BufRead;
                let mut line = String::new();
                let _ = std::io::BufReader::new(stdout).read_line(&mut line);
                let _ = tx.send(line);
            });
            let address = rx.recv_timeout(Duration::from_secs(5)).unwrap_or_default();
            let address = address.trim().to_owned();
            if address.is_empty() {
                let _ = child.kill();
                panic!("dbus-daemon printed no address: {}", errors.lock().unwrap());
            }
            // SAFETY: this test is the only writer of the session address. It
            // restores the previous value when dropped, before another test
            // can connect. ashpd caches the first session connection, so the
            // private bus has to be selected before that connection exists.
            let previous = std::env::var_os("DBUS_SESSION_BUS_ADDRESS");
            unsafe { std::env::set_var("DBUS_SESSION_BUS_ADDRESS", &address) };
            Self {
                child,
                previous,
                _dir: dir,
            }
        }
    }

    impl Drop for PrivateBus {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
            // SAFETY: paired with the set in `start`, on the same test thread.
            unsafe {
                match &self.previous {
                    Some(value) => std::env::set_var("DBUS_SESSION_BUS_ADDRESS", value),
                    None => std::env::remove_var("DBUS_SESSION_BUS_ADDRESS"),
                }
            }
        }
    }
}
