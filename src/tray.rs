//! Notification-area icon: shows whether the mic is muted and offers the
//! main controls while the window is hidden.

#[cfg(not(windows))]
pub use fallback::Tray;
#[cfg(windows)]
pub use windows_tray::Tray;

#[cfg(windows)]
mod windows_tray {
    use anyhow::{Context, Result};
    use crossbeam_channel::Sender;
    use eframe::egui;
    use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem};
    use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

    use crate::gui::Command;
    use crate::icon;

    pub struct Tray {
        icon: TrayIcon,
        mute: CheckMenuItem,
        bypass: CheckMenuItem,
        running: MenuItem,
        /// (muted, bypassed, running, listening) as last shown.
        shown: Option<(bool, bool, bool, bool)>,
    }

    impl Tray {
        /// Menu picks and left clicks are sent to `commands` and wake the
        /// app through `ctx`, even while the window is hidden.
        pub fn new(ctx: &egui::Context, commands: Sender<Command>) -> Result<Self> {
            let show = MenuItem::new("Show OpenMic", true, None);
            let mute = CheckMenuItem::new("Mute mic", true, false, None);
            let bypass = CheckMenuItem::new("Bypass processing", true, false, None);
            let running = MenuItem::new("Start OpenMic", true, None);
            let quit = MenuItem::new("Quit OpenMic", true, None);
            let menu = Menu::new();
            menu.append_items(&[
                &show,
                &PredefinedMenuItem::separator(),
                &mute,
                &bypass,
                &running,
                &PredefinedMenuItem::separator(),
                &quit,
            ])
            .context("build tray menu")?;

            let items = [
                (show.id().clone(), Command::Show),
                (mute.id().clone(), Command::ToggleMute),
                (bypass.id().clone(), Command::ToggleBypass),
                (running.id().clone(), Command::ToggleRunning),
                (quit.id().clone(), Command::Quit),
            ];
            {
                let commands = commands.clone();
                let ctx = ctx.clone();
                MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
                    if let Some((_, command)) = items.iter().find(|(id, _)| *id == event.id) {
                        let _ = commands.send(command.clone());
                        ctx.request_repaint();
                    }
                }));
            }
            let ctx = ctx.clone();
            TrayIconEvent::set_event_handler(Some(move |event: TrayIconEvent| {
                if let TrayIconEvent::Click {
                    button: MouseButton::Left,
                    button_state: MouseButtonState::Up,
                    ..
                } = event
                {
                    let _ = commands.send(Command::Show);
                    ctx.request_repaint();
                }
            }));

            let icon = TrayIconBuilder::new()
                .with_menu(Box::new(menu))
                .with_tooltip("OpenMic")
                .with_icon(tray_icon(false)?)
                .build()
                .context("create tray icon")?;
            Ok(Self { icon, mute, bypass, running, shown: None })
        }

        /// `listening`: dictation is recording speech to type.
        pub fn sync(&mut self, muted: bool, bypassed: bool, running: bool, listening: bool) {
            let state = (muted, bypassed, running, listening);
            if self.shown == Some(state) {
                return;
            }
            self.shown = Some(state);
            self.mute.set_checked(muted);
            self.bypass.set_checked(bypassed);
            self.running.set_text(if running { "Stop OpenMic" } else { "Start OpenMic" });
            let icon = if listening { listening_icon() } else { tray_icon(muted) };
            let _ = self.icon.set_icon(icon.ok());
            let _ = self.icon.set_tooltip(Some(match (listening, running, muted) {
                (true, _, _) => "OpenMic · listening for dictation",
                (false, false, _) => "OpenMic · stopped",
                (false, true, true) => "OpenMic · mic muted",
                (false, true, false) => "OpenMic · running",
            }));
        }
    }

    fn tray_icon(muted: bool) -> Result<Icon> {
        const SIZE: u32 = 32;
        Icon::from_rgba(icon::rgba(SIZE, muted), SIZE, SIZE).context("draw tray icon")
    }

    fn listening_icon() -> Result<Icon> {
        const SIZE: u32 = 32;
        Icon::from_rgba(icon::listening_rgba(SIZE), SIZE, SIZE).context("draw tray icon")
    }
}

/// No notification area outside Windows; the window simply closes.
#[cfg(not(windows))]
mod fallback {
    use anyhow::Result;
    use crossbeam_channel::Sender;
    use eframe::egui;

    use crate::gui::Command;

    pub struct Tray;

    impl Tray {
        pub fn new(_ctx: &egui::Context, _commands: Sender<Command>) -> Result<Self> {
            anyhow::bail!("no notification area on this platform")
        }

        pub fn sync(&mut self, _muted: bool, _bypassed: bool, _running: bool, _listening: bool) {}
    }
}
