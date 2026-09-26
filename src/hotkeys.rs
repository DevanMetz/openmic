//! System-wide shortcuts: turn a key press in the window into a shortcut
//! such as "Ctrl+Alt+1", register shortcuts with Windows, and deliver their
//! presses as app commands even while the window is hidden.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Context, Result};
use crossbeam_channel::Sender;
use eframe::egui;
use global_hotkey::hotkey::HotKey;
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
use parking_lot::Mutex;

use crate::gui::Command;

/// The registered shortcuts and what each one does.
pub struct Hotkeys {
    manager: GlobalHotKeyManager,
    /// Read by the event handler, which runs in the window's message loop.
    actions: Arc<Mutex<Actions>>,
    registered: Vec<HotKey>,
    bindings: Vec<(String, Command)>,
}

#[derive(Default)]
struct Actions {
    bindings: HashMap<u32, Command>,
    /// A release belongs to the action that handled its press, even if the
    /// shortcut was cleared or reassigned while the key was held.
    pressed: HashMap<u32, Command>,
}

impl Actions {
    fn event(&mut self, id: u32, state: HotKeyState) -> Option<Command> {
        match state {
            HotKeyState::Pressed => {
                let command = self.bindings.get(&id)?.clone();
                self.pressed.insert(id, command.clone());
                Some(command)
            }
            HotKeyState::Released => self.pressed.remove(&id).and_then(|c| c.released()),
        }
    }
}

impl Hotkeys {
    /// Presses are sent to `commands` and wake the app through `ctx`.
    pub fn new(ctx: &egui::Context, commands: Sender<Command>) -> Result<Self> {
        let manager = GlobalHotKeyManager::new().context("start global hotkeys")?;
        let actions: Arc<Mutex<Actions>> = Arc::default();
        let lookup = Arc::clone(&actions);
        let ctx = ctx.clone();
        GlobalHotKeyEvent::set_event_handler(Some(move |event: GlobalHotKeyEvent| {
            let command = lookup.lock().event(event.id(), event.state());
            if let Some(command) = command {
                let _ = commands.send(command);
                ctx.request_repaint();
            }
        }));
        Ok(Self {
            manager,
            actions,
            registered: Vec::new(),
            bindings: Vec::new(),
        })
    }

    /// Register exactly `bindings` (if they changed since the last call).
    /// Returns the shortcuts Windows refused, usually because another app
    /// already owns them.
    pub fn set(&mut self, bindings: Vec<(String, Command)>) -> Vec<String> {
        if bindings == self.bindings {
            return Vec::new();
        }
        let _ = self.manager.unregister_all(&self.registered);
        self.registered.clear();
        let mut actions = HashMap::new();
        let mut refused = Vec::new();
        for (text, command) in &bindings {
            match text.parse::<HotKey>() {
                Ok(hotkey) if self.manager.register(hotkey).is_ok() => {
                    actions.insert(hotkey.id(), command.clone());
                    self.registered.push(hotkey);
                }
                _ => refused.push(text.clone()),
            }
        }
        self.actions.lock().bindings = actions;
        self.bindings = bindings;
        refused
    }
}

/// The shortcut for `key` pressed with `mods`, as "Ctrl+Shift+M".
///
/// Ordinary keys need Ctrl or Alt so a shortcut can't swallow normal
/// typing; F13-F24 (spare keys on macro pads and stream decks) work alone.
pub fn combo(key: egui::Key, mods: egui::Modifiers) -> Result<String, &'static str> {
    let name = match key.name() {
        "Equals" | "Plus" => "Equal",
        "Backtick" => "Backquote",
        "OpenBracket" => "BracketLeft",
        "CloseBracket" => "BracketRight",
        name => name,
    };
    let spare = matches!(name.strip_prefix('F').and_then(|n| n.parse::<u8>().ok()), Some(13..=24));
    if !spare && !mods.ctrl && !mods.alt {
        return Err("Hold Ctrl or Alt with the key (F13-F24 work alone)");
    }
    let mut text = String::new();
    for (held, label) in [(mods.ctrl, "Ctrl+"), (mods.alt, "Alt+"), (mods.shift, "Shift+")] {
        if held {
            text.push_str(label);
        }
    }
    text.push_str(name);
    match text.parse::<HotKey>() {
        Ok(_) => Ok(text),
        Err(_) => Err("That key can't be a shortcut"),
    }
}

/// Whether two shortcut strings mean the same key combination.
pub fn same(a: &str, b: &str) -> bool {
    match (a.parse::<HotKey>(), b.parse::<HotKey>()) {
        (Ok(a), Ok(b)) => a.id() == b.id(),
        _ => a.eq_ignore_ascii_case(b),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CTRL_ALT: egui::Modifiers = egui::Modifiers {
        alt: true,
        ctrl: true,
        shift: false,
        mac_cmd: false,
        command: true,
    };

    #[test]
    fn combos_read_naturally_and_parse() {
        assert_eq!(combo(egui::Key::Num1, CTRL_ALT).unwrap(), "Ctrl+Alt+1");
        assert_eq!(combo(egui::Key::M, egui::Modifiers::CTRL | egui::Modifiers::SHIFT).unwrap(), "Ctrl+Shift+M");
        assert_eq!(combo(egui::Key::Equals, egui::Modifiers::ALT).unwrap(), "Alt+Equal");
        assert_eq!(combo(egui::Key::F13, egui::Modifiers::NONE).unwrap(), "F13");
    }

    #[test]
    fn plain_typing_keys_are_refused() {
        assert!(combo(egui::Key::M, egui::Modifiers::NONE).is_err());
        assert!(combo(egui::Key::M, egui::Modifiers::SHIFT).is_err());
        assert!(combo(egui::Key::F5, egui::Modifiers::NONE).is_err());
        assert!(combo(egui::Key::F30, CTRL_ALT).is_err(), "Windows has no F30 shortcut");
    }

    #[test]
    fn same_ignores_spelling() {
        assert!(same("Ctrl+Alt+1", "control+alt+Digit1"));
        assert!(!same("Ctrl+Alt+1", "Ctrl+Alt+2"));
    }

    #[test]
    fn held_dictation_releases_after_shortcuts_are_cleared_or_reassigned() {
        let mut actions = Actions::default();
        actions.bindings.insert(1, Command::Dictate);
        assert_eq!(actions.event(1, HotKeyState::Pressed), Some(Command::Dictate));
        actions.bindings.clear();
        assert_eq!(actions.event(1, HotKeyState::Released), Some(Command::DictateReleased));

        actions.bindings.insert(1, Command::Dictate);
        actions.event(1, HotKeyState::Pressed);
        actions.bindings.insert(1, Command::ToggleMute);
        assert_eq!(actions.event(1, HotKeyState::Released), Some(Command::DictateReleased));
        assert_eq!(actions.event(1, HotKeyState::Released), None, "a release is handled only once");
    }

    #[test]
    fn rebinding_a_held_key_does_not_release_the_new_action() {
        let mut actions = Actions::default();
        actions.bindings.insert(1, Command::ToggleMute);
        assert_eq!(actions.event(1, HotKeyState::Pressed), Some(Command::ToggleMute));
        actions.bindings.insert(1, Command::Dictate);
        assert_eq!(actions.event(1, HotKeyState::Released), None);
    }
}
