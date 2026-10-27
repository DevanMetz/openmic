//! Notification-area icon: shows whether the mic is muted and offers the
//! main controls while the window is hidden.
//!
//! Windows uses the shell notification area. Linux uses a StatusNotifierItem
//! (the ksni backend), which needs a desktop that shows those icons. Anywhere
//! the icon cannot be created, [`Tray::new`] fails and closing the window quits.

#[cfg(any(windows, target_os = "linux"))]
pub use native::Tray;
#[cfg(not(any(windows, target_os = "linux")))]
pub use fallback::Tray;

#[cfg(any(windows, target_os = "linux"))]
mod native {
    use anyhow::{Context, Result};
    use crossbeam_channel::Sender;
    use eframe::egui;
    use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem};
    use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

    use crate::gui::Command;
    use crate::icon;

    enum Inner {
        Live(Live),
        #[cfg(test)]
        Stub,
    }

    pub struct Tray {
        inner: Inner,
    }

    impl Tray {
        /// Menu picks and left clicks are sent to `commands` and wake the
        /// app through `ctx`, even while the window is hidden.
        pub fn new(ctx: &egui::Context, commands: Sender<Command>) -> Result<Self> {
            Ok(Self { inner: Inner::Live(Live::new(ctx, commands)?) })
        }

        /// A tray that is present for close-to-tray tests, without a desktop session.
        #[cfg(test)]
        pub(crate) fn present_for_test() -> Self {
            Self { inner: Inner::Stub }
        }

        /// `listening`: dictation is recording speech to type.
        pub fn sync(&mut self, muted: bool, bypassed: bool, running: bool, listening: bool, collapsed: bool) {
            match &mut self.inner {
                Inner::Live(live) => live.sync(muted, bypassed, running, listening, collapsed),
                #[cfg(test)]
                Inner::Stub => {}
            }
        }
    }

    struct Live {
        icon: TrayIcon,
        mute: CheckMenuItem,
        bypass: CheckMenuItem,
        running: MenuItem,
        meter: CheckMenuItem,
        /// (muted, bypassed, running, listening, collapsed) as last shown.
        shown: Option<(bool, bool, bool, bool, bool)>,
    }

    impl Live {
        fn new(ctx: &egui::Context, commands: Sender<Command>) -> Result<Self> {
            let show = MenuItem::new("Show OpenMic", true, None);
            let mute = CheckMenuItem::new("Mute mic", true, false, None);
            let bypass = CheckMenuItem::new("Bypass processing", true, false, None);
            let running = MenuItem::new("Start OpenMic", true, None);
            let meter = CheckMenuItem::new("Collapse to strip", true, false, None);
            let quit = MenuItem::new("Quit OpenMic", true, None);
            let menu = Menu::new();
            menu.append_items(&[
                &show,
                &PredefinedMenuItem::separator(),
                &mute,
                &bypass,
                &running,
                &meter,
                &PredefinedMenuItem::separator(),
                &quit,
            ])
            .context("build tray menu")?;

            let items = [
                (show.id().clone(), Command::Show),
                (mute.id().clone(), Command::ToggleMute),
                (bypass.id().clone(), Command::ToggleBypass),
                (running.id().clone(), Command::ToggleRunning),
                (meter.id().clone(), Command::ToggleCollapse),
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
            Ok(Self { icon, mute, bypass, running, meter, shown: None })
        }

        fn sync(&mut self, muted: bool, bypassed: bool, running: bool, listening: bool, collapsed: bool) {
            let state = (muted, bypassed, running, listening, collapsed);
            if self.shown == Some(state) {
                return;
            }
            self.shown = Some(state);
            self.mute.set_checked(muted);
            self.bypass.set_checked(bypassed);
            self.meter.set_checked(collapsed);
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

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn tray_icons_are_square_rgba() {
            const SIZE: u32 = 32;
            for pixels in [icon::rgba(SIZE, false), icon::rgba(SIZE, true), icon::listening_rgba(SIZE)] {
                assert_eq!(pixels.len(), (SIZE * SIZE * 4) as usize);
                Icon::from_rgba(pixels, SIZE, SIZE).expect("tray icon pixels");
            }
        }

        /// Startup calls this on the UI thread. A missing session bus or
        /// StatusNotifier host must come back as an error, not a hang or a panic.
        #[cfg(target_os = "linux")]
        #[test]
        fn starting_the_tray_returns() {
            let (tx, rx) = std::sync::mpsc::channel();
            let ctx = egui::Context::default();
            let (commands, _rx) = crossbeam_channel::unbounded();
            std::thread::spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| Tray::new(&ctx, commands)));
                let _ = tx.send(result.map(|tray| tray.is_ok()));
            });
            match rx.recv_timeout(std::time::Duration::from_secs(5)) {
                Ok(Ok(_started)) => {}
                Ok(Err(_)) => panic!("tray startup panicked"),
                Err(_) => panic!("tray startup did not return"),
            }
        }
    }
}

/// No notification area on this platform; the window simply closes.
#[cfg(not(any(windows, target_os = "linux")))]
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

        #[cfg(test)]
        pub(crate) fn present_for_test() -> Self {
            Self
        }

        pub fn sync(&mut self, _muted: bool, _bypassed: bool, _running: bool, _listening: bool, _collapsed: bool) {}
    }
}
