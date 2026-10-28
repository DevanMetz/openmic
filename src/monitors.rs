//! Where the screens are, so a saved window or strip is not restored onto a
//! monitor that has been unplugged, and so the window fits the work area.
//!
//! X11 publishes this through RandR and `_NET_WORKAREA`. Wayland does not tell
//! a client its global position, and winit ignores a request to move a window
//! there, so those sessions leave the saved position unchecked.

/// A rectangle in screen pixels. `width` and `height` are never negative.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PxRect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

impl PxRect {
    pub(crate) fn from_f32_edges(left: f32, top: f32, right: f32, bottom: f32) -> Option<Self> {
        let x = finite_px(left)?;
        let y = finite_px(top)?;
        let right = finite_px(right)?;
        let bottom = finite_px(bottom)?;
        let width = right.checked_sub(x).filter(|width| *width >= 0)?;
        let height = bottom.checked_sub(y).filter(|height| *height >= 0)?;
        Some(Self {
            x,
            y,
            width,
            height,
        })
    }

    pub(crate) fn right(self) -> i32 {
        self.x.saturating_add(self.width)
    }

    pub(crate) fn bottom(self) -> i32 {
        self.y.saturating_add(self.height)
    }

    fn contains(self, x: i32, y: i32) -> bool {
        self.width > 0
            && self.height > 0
            && x >= self.x
            && y >= self.y
            && (x as i64) < self.x as i64 + self.width as i64
            && (y as i64) < self.y as i64 + self.height as i64
    }

    fn intersection(self, other: Self) -> Option<Self> {
        let left = i64::from(self.x).max(i64::from(other.x));
        let top = i64::from(self.y).max(i64::from(other.y));
        let right = (i64::from(self.x) + i64::from(self.width))
            .min(i64::from(other.x) + i64::from(other.width));
        let bottom = (i64::from(self.y) + i64::from(self.height))
            .min(i64::from(other.y) + i64::from(other.height));
        let width = i32::try_from(right - left).ok()?;
        let height = i32::try_from(bottom - top).ok()?;
        (width > 0 && height > 0).then_some(Self {
            x: i32::try_from(left).ok()?,
            y: i32::try_from(top).ok()?,
            width,
            height,
        })
    }

    fn overlap_area(self, other: Self) -> i64 {
        self.intersection(other)
            .map(|rect| i64::from(rect.width) * i64::from(rect.height))
            .unwrap_or(0)
    }

    /// Squared distance between the rectangles, or zero when they meet.
    fn gap(self, other: Self) -> i64 {
        let dx = interval_gap(
            i64::from(self.x),
            i64::from(self.width),
            i64::from(other.x),
            i64::from(other.width),
        );
        let dy = interval_gap(
            i64::from(self.y),
            i64::from(self.height),
            i64::from(other.y),
            i64::from(other.height),
        );
        dx.saturating_mul(dx).saturating_add(dy.saturating_mul(dy))
    }
}

fn finite_px(value: f32) -> Option<i32> {
    if !value.is_finite() {
        return None;
    }
    i32::try_from(value.round() as i64).ok()
}

fn interval_gap(origin: i64, length: i64, other_origin: i64, other_length: i64) -> i64 {
    let end = origin.saturating_add(length);
    let other_end = other_origin.saturating_add(other_length);
    if end < other_origin {
        other_origin - end
    } else if other_end < origin {
        origin - other_end
    } else {
        0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Monitor {
    rect: PxRect,
    mm_width: u32,
    mm_height: u32,
    primary: bool,
}

#[derive(Clone, Debug, PartialEq)]
struct Layout {
    monitors: Vec<Monitor>,
    /// EWMH work area of the current desktop, when the window manager published one.
    work_area: Option<PxRect>,
    /// X pixels per egui point. 2.0 means a 192 dpi screen.
    scale: f64,
}

/// Whether `x11_display` is the session winit will use. Wayland wins when
/// both are set, matching winit, because a Wayland window's position is not
/// an X11 coordinate.
fn positions_use_x11(wayland_display: bool, wayland_socket: bool, x11_display: bool) -> bool {
    x11_display && !wayland_display && !wayland_socket
}

/// A point 16 px inside the saved top-left (the title bar, or the strip's
/// microphone) lands on a connected monitor.
fn title_on_monitor(layout: &Layout, x: i32, y: i32) -> bool {
    let x = x.saturating_add(16);
    let y = y.saturating_add(16);
    layout
        .monitors
        .iter()
        .any(|monitor| monitor.rect.contains(x, y))
}

fn primary(layout: &Layout) -> Option<&Monitor> {
    layout
        .monitors
        .iter()
        .find(|monitor| monitor.primary)
        .or(layout.monitors.first())
}

fn work_rect(layout: &Layout, monitor: &Monitor) -> PxRect {
    layout
        .work_area
        .and_then(|work| monitor.rect.intersection(work))
        .unwrap_or(monitor.rect)
}

fn nearest_monitor(layout: &Layout, window: PxRect) -> Option<&Monitor> {
    let mut best: Option<(&Monitor, i64, i64)> = None;
    for monitor in &layout.monitors {
        let overlap = monitor.rect.overlap_area(window);
        let gap = monitor.rect.gap(window);
        let better = best.is_none_or(|(_, best_overlap, best_gap)| {
            overlap > best_overlap || (overlap == best_overlap && gap < best_gap)
        });
        if better {
            best = Some((monitor, overlap, gap));
        }
    }
    best.map(|(monitor, _, _)| monitor)
}

fn nearest_work_rect(layout: &Layout, window: PxRect) -> Option<PxRect> {
    nearest_monitor(layout, window).map(|monitor| work_rect(layout, monitor))
}

fn primary_work_points(layout: &Layout) -> Option<[f32; 2]> {
    let rect = work_rect(layout, primary(layout)?);
    if !layout.scale.is_finite() || layout.scale <= 0.0 {
        return None;
    }
    let width = rect.width as f32 / layout.scale as f32;
    let height = rect.height as f32 / layout.scale as f32;
    (width.is_finite() && height.is_finite() && width > 0.0 && height > 0.0)
        .then_some([width, height])
}

/// `_NET_WORKAREA` is four cardinals (x, y, width, height) per desktop.
fn work_area_from_cardinals(desktop: usize, values: &[u32]) -> Option<PxRect> {
    let start = if values.len() >= desktop.saturating_add(1).saturating_mul(4) {
        desktop.saturating_mul(4)
    } else {
        0
    };
    let slice = values.get(start..start + 4)?;
    let rect = PxRect {
        x: i32::try_from(slice[0]).ok()?,
        y: i32::try_from(slice[1]).ok()?,
        width: i32::try_from(slice[2]).ok()?,
        height: i32::try_from(slice[3]).ok()?,
    };
    (rect.width > 0 && rect.height > 0).then_some(rect)
}

fn scale_from_dpi(dpi: f64) -> Option<f64> {
    let factor = dpi / 96.0;
    (factor.is_finite() && (0.25..=20.0).contains(&factor)).then_some(factor)
}

/// winit's RandR fallback: quantize pixels-per-mm to 1/12, and never below 1.
fn randr_scale(monitor: &Monitor) -> f64 {
    if monitor.rect.width <= 0
        || monitor.rect.height <= 0
        || monitor.mm_width == 0
        || monitor.mm_height == 0
    {
        return 1.0;
    }
    let px = f64::from(monitor.rect.width) * f64::from(monitor.rect.height);
    let mm = f64::from(monitor.mm_width) * f64::from(monitor.mm_height);
    let ppmm = (px / mm).sqrt();
    let factor = ((ppmm * (12.0 * 25.4 / 96.0)).round() / 12.0).max(1.0);
    if factor.is_finite() && factor <= 20.0 {
        factor
    } else {
        1.0
    }
}

fn layout_scale(monitor: &Monitor, xft_dpi: Option<f64>) -> f64 {
    xft_dpi
        .and_then(scale_from_dpi)
        .unwrap_or_else(|| randr_scale(monitor))
}

/// DPI from an XSETTINGS blob. The integer `Xft/DPI` is the dpi times 1024.
fn xft_dpi_from_xsettings(data: &[u8]) -> Option<f64> {
    let mut bytes = XSettings::new(data)?;
    let count = bytes.i32()?;
    if count < 0 {
        return None;
    }
    for _ in 0..count {
        let kind = bytes.u8()?;
        let _ = bytes.u8()?;
        let name_len = bytes.i16()?;
        if name_len < 0 {
            return None;
        }
        let name = bytes.take(name_len as usize)?;
        bytes.pad(name.len())?;
        let _serial = bytes.i32()?;
        match kind {
            0 => {
                let value = bytes.i32()?;
                if name == b"Xft/DPI" && value > 0 {
                    return Some(f64::from(value) / 1024.0);
                }
            }
            1 => {
                let len = bytes.i32()?;
                if len < 0 {
                    return None;
                }
                let text = bytes.take(len as usize)?;
                bytes.pad(text.len())?;
            }
            2 => {
                for _ in 0..4 {
                    bytes.i16()?;
                }
            }
            _ => return None,
        }
    }
    None
}

struct XSettings<'a> {
    data: &'a [u8],
    at: usize,
    little_endian: bool,
}

impl<'a> XSettings<'a> {
    /// Byte 0 is `l` or `B`, then 3 bytes of padding and a 4-byte serial.
    fn new(data: &'a [u8]) -> Option<Self> {
        let (&endian, rest) = data.split_first()?;
        let little_endian = match endian {
            b'l' => true,
            b'B' => false,
            _ => return None,
        };
        let data = rest.get(7..)?;
        Some(Self {
            data,
            at: 0,
            little_endian,
        })
    }

    fn take(&mut self, len: usize) -> Option<&'a [u8]> {
        let end = self.at.checked_add(len)?;
        let bytes = self.data.get(self.at..end)?;
        self.at = end;
        Some(bytes)
    }

    fn u8(&mut self) -> Option<u8> {
        self.take(1).map(|bytes| bytes[0])
    }

    fn i16(&mut self) -> Option<i16> {
        let bytes = self.take(2)?;
        let raw = if self.little_endian {
            u16::from_le_bytes([bytes[0], bytes[1]])
        } else {
            u16::from_be_bytes([bytes[0], bytes[1]])
        };
        Some(raw as i16)
    }

    fn i32(&mut self) -> Option<i32> {
        let bytes = self.take(4)?;
        let raw = if self.little_endian {
            u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
        } else {
            u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
        };
        Some(raw as i32)
    }

    fn pad(&mut self, size: usize) -> Option<()> {
        let extra = (4 - (size % 4)) % 4;
        self.take(extra).map(|_| ())
    }
}

#[cfg(target_os = "linux")]
mod x11 {
    use super::{
        Layout, Monitor, PxRect, layout_scale, work_area_from_cardinals, xft_dpi_from_xsettings,
    };

    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};

    use x11rb::connection::Connection;
    use x11rb::protocol::randr::ConnectionExt as _;
    use x11rb::protocol::xproto::{self, ConnectionExt as _};

    struct Snapshot {
        at: Instant,
        layout: Option<Layout>,
    }

    static CACHE: Mutex<Option<Snapshot>> = Mutex::new(None);
    static WARNED: AtomicBool = AtomicBool::new(false);
    const TTL: Duration = Duration::from_secs(1);

    pub(super) fn cached() -> Option<Layout> {
        let mut slot = CACHE.lock().unwrap_or_else(|err| err.into_inner());
        if let Some(snapshot) = slot.as_ref()
            && snapshot.at.elapsed() < TTL
        {
            return snapshot.layout.clone();
        }
        let layout = load();
        *slot = Some(Snapshot {
            at: Instant::now(),
            layout: layout.clone(),
        });
        layout
    }

    pub(super) fn load() -> Option<Layout> {
        if !x11_positions_apply() {
            return None;
        }
        let layout = read_layout();
        if layout.is_none()
            && WARNED
                .compare_exchange(false, true, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
        {
            crate::logfile::warn("X11 monitor layout unavailable; saved window positions are kept");
        }
        layout
    }

    fn x11_positions_apply() -> bool {
        super::positions_use_x11(
            env_set("WAYLAND_DISPLAY"),
            env_set("WAYLAND_SOCKET"),
            env_set("DISPLAY"),
        )
    }

    fn env_set(name: &str) -> bool {
        std::env::var(name).is_ok_and(|value| !value.is_empty())
    }

    fn read_layout() -> Option<Layout> {
        let (conn, screen_num) = x11rb::connect(None).ok()?;
        let root = {
            let screen = conn.setup().roots.get(screen_num)?;
            (
                screen.root,
                screen.width_in_pixels,
                screen.height_in_pixels,
                screen.width_in_millimeters,
                screen.height_in_millimeters,
            )
        };
        let (root, root_width, root_height, root_mm_width, root_mm_height) = root;
        let mut monitors = read_randr(&conn, root);
        if monitors.is_empty() {
            monitors.push(Monitor {
                rect: PxRect {
                    x: 0,
                    y: 0,
                    width: i32::from(root_width),
                    height: i32::from(root_height),
                },
                mm_width: u32::from(root_mm_width),
                mm_height: u32::from(root_mm_height),
                primary: true,
            });
        }
        if !monitors.iter().any(|monitor| monitor.primary)
            && let Some(first) = monitors.first_mut()
        {
            first.primary = true;
        }
        let primary = monitors.iter().find(|monitor| monitor.primary)?;
        if primary.rect.width <= 0 || primary.rect.height <= 0 {
            return None;
        }
        let scale = layout_scale(primary, xft_dpi(&conn, screen_num));
        let desktop = cardinal(&conn, root, b"_NET_CURRENT_DESKTOP")
            .and_then(|values| values.first().copied())
            .unwrap_or(0);
        let work_area = cardinal(&conn, root, b"_NET_WORKAREA")
            .as_deref()
            .and_then(|values| {
                work_area_from_cardinals(usize::try_from(desktop).unwrap_or(0), values)
            });
        Some(Layout {
            monitors,
            work_area,
            scale,
        })
    }

    fn read_randr(
        conn: &x11rb::rust_connection::RustConnection,
        root: xproto::Window,
    ) -> Vec<Monitor> {
        if conn
            .randr_query_version(1, 5)
            .ok()
            .and_then(|cookie| cookie.reply().ok())
            .is_none()
        {
            return Vec::new();
        }
        let Some(resources) = conn
            .randr_get_screen_resources_current(root)
            .ok()
            .and_then(|cookie| cookie.reply().ok())
        else {
            return Vec::new();
        };
        let primary = conn
            .randr_get_output_primary(root)
            .ok()
            .and_then(|cookie| cookie.reply().ok())
            .map(|reply| reply.output)
            .unwrap_or(0);
        let mut monitors = Vec::new();
        for crtc in resources.crtcs {
            let Some(info) = conn
                .randr_get_crtc_info(crtc, x11rb::CURRENT_TIME)
                .ok()
                .and_then(|cookie| cookie.reply().ok())
            else {
                continue;
            };
            if info.width == 0 || info.height == 0 || info.outputs.is_empty() {
                continue;
            }
            let output = info
                .outputs
                .iter()
                .copied()
                .find(|output| *output == primary)
                .unwrap_or(info.outputs[0]);
            let (mm_width, mm_height) = conn
                .randr_get_output_info(output, x11rb::CURRENT_TIME)
                .ok()
                .and_then(|cookie| cookie.reply().ok())
                .map(|reply| (reply.mm_width, reply.mm_height))
                .unwrap_or((0, 0));
            monitors.push(Monitor {
                rect: PxRect {
                    x: i32::from(info.x),
                    y: i32::from(info.y),
                    width: i32::from(info.width),
                    height: i32::from(info.height),
                },
                mm_width,
                mm_height,
                primary: info.outputs.contains(&primary),
            });
        }
        monitors
    }

    fn xft_dpi(conn: &x11rb::rust_connection::RustConnection, screen: usize) -> Option<f64> {
        if let Some(dpi) = xsettings_dpi(conn, screen) {
            return Some(dpi);
        }
        let db = x11rb::resource_manager::new_from_default(conn).ok()?;
        db.get_value("Xft.dpi", "").ok().flatten()
    }

    fn xsettings_dpi(conn: &x11rb::rust_connection::RustConnection, screen: usize) -> Option<f64> {
        let selection = atom(conn, format!("_XSETTINGS_S{screen}").as_bytes())?;
        let owner = conn
            .get_selection_owner(selection)
            .ok()?
            .reply()
            .ok()?
            .owner;
        if owner == x11rb::NONE {
            return None;
        }
        let settings = atom(conn, b"_XSETTINGS_SETTINGS")?;
        let reply = conn
            .get_property(false, owner, settings, xproto::AtomEnum::ANY, 0, 65_536)
            .ok()?
            .reply()
            .ok()?;
        xft_dpi_from_xsettings(&reply.value)
    }

    fn cardinal(
        conn: &x11rb::rust_connection::RustConnection,
        window: xproto::Window,
        name: &[u8],
    ) -> Option<Vec<u32>> {
        let property = atom(conn, name)?;
        let reply = conn
            .get_property(false, window, property, xproto::AtomEnum::CARDINAL, 0, 256)
            .ok()?
            .reply()
            .ok()?;
        Some(reply.value32()?.collect())
    }

    fn atom(conn: &x11rb::rust_connection::RustConnection, name: &[u8]) -> Option<xproto::Atom> {
        let atom = conn.intern_atom(true, name).ok()?.reply().ok()?.atom;
        (atom != x11rb::NONE).then_some(atom)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn a_live_x11_server_rejects_positions_past_every_monitor() {
            use super::super::{primary_work_points, title_on_monitor};
            let Some(layout) = load() else { return };
            assert!(
                layout
                    .monitors
                    .iter()
                    .any(|monitor| monitor.rect.width > 0 && monitor.rect.height > 0)
            );
            assert!(!title_on_monitor(&layout, -100_000, -100_000));
            assert!(!title_on_monitor(&layout, i32::MAX - 20, i32::MAX - 20));
            let area = primary_work_points(&layout).expect("primary work area");
            assert!(area[0] > 0.0 && area[1] > 0.0, "{area:?}");
        }
    }
}

#[cfg(target_os = "linux")]
pub(crate) fn on_a_monitor([x, y]: [i32; 2]) -> bool {
    x11::cached().is_none_or(|layout| title_on_monitor(&layout, x, y))
}

#[cfg(target_os = "linux")]
pub(crate) fn primary_work_area_points() -> Option<[f32; 2]> {
    x11::cached().as_ref().and_then(primary_work_points)
}

#[cfg(target_os = "linux")]
pub(crate) fn work_area_pixels(window: PxRect) -> Option<PxRect> {
    x11::cached()
        .as_ref()
        .and_then(|layout| nearest_work_rect(layout, window))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn monitor(x: i32, y: i32, width: i32, height: i32, primary: bool) -> Monitor {
        Monitor {
            rect: PxRect {
                x,
                y,
                width,
                height,
            },
            mm_width: 300,
            mm_height: 170,
            primary,
        }
    }

    fn layout(monitors: Vec<Monitor>, work_area: Option<PxRect>, scale: f64) -> Layout {
        Layout {
            monitors,
            work_area,
            scale,
        }
    }

    #[test]
    fn wayland_wins_over_xwayland_when_both_displays_are_set() {
        assert!(positions_use_x11(false, false, true));
        assert!(
            !positions_use_x11(true, false, true),
            "winit uses Wayland, whose positions are not X11 coordinates"
        );
        assert!(!positions_use_x11(false, true, true));
        assert!(!positions_use_x11(false, false, false));
    }

    #[test]
    fn a_gap_between_monitors_is_not_a_place_to_restore() {
        let screens = layout(
            vec![
                monitor(0, 0, 1920, 1080, true),
                monitor(2000, 0, 1920, 1080, false),
            ],
            None,
            1.0,
        );
        assert!(title_on_monitor(&screens, 10, 10));
        assert!(title_on_monitor(&screens, 2000, 0));
        assert!(
            !title_on_monitor(&screens, 1930, 100),
            "the gap between the two screens"
        );
        assert!(
            title_on_monitor(&screens, -10, 0),
            "16 px inside the title bar still hits the screen"
        );
        assert!(!title_on_monitor(&screens, -20, 0));
        assert!(!title_on_monitor(&screens, -100_000, -100_000));
    }

    #[test]
    fn the_work_area_trims_the_panel_and_ignores_a_desktop_it_does_not_cover() {
        let primary = monitor(0, 0, 1920, 1080, true);
        let secondary = monitor(1920, 0, 1920, 1080, false);
        let work = PxRect {
            x: 0,
            y: 0,
            width: 3840,
            height: 1040,
        };
        let screens = layout(vec![primary, secondary], Some(work), 1.0);
        assert_eq!(
            work_rect(&screens, &primary),
            PxRect {
                x: 0,
                y: 0,
                width: 1920,
                height: 1040
            }
        );
        assert_eq!(
            work_rect(&screens, &secondary),
            PxRect {
                x: 1920,
                y: 0,
                width: 1920,
                height: 1040
            }
        );

        let primary_only = layout(
            vec![primary, secondary],
            Some(PxRect {
                x: 0,
                y: 0,
                width: 1920,
                height: 1040,
            }),
            1.0,
        );
        assert_eq!(
            work_rect(&primary_only, &secondary),
            secondary.rect,
            "a work area that misses this monitor leaves it whole"
        );
        assert_eq!(primary_work_points(&screens), Some([1920.0, 1040.0]));
        let hidpi = layout(vec![primary], Some(work), 2.0);
        assert_eq!(primary_work_points(&hidpi), Some([960.0, 520.0]));
    }

    #[test]
    fn the_nearest_monitor_supplies_the_work_area_for_a_window() {
        let screens = layout(
            vec![
                monitor(0, 0, 1000, 800, true),
                monitor(1200, 0, 800, 600, false),
            ],
            Some(PxRect {
                x: 0,
                y: 0,
                width: 2000,
                height: 760,
            }),
            1.0,
        );
        let on_secondary = PxRect {
            x: 1300,
            y: 40,
            width: 200,
            height: 100,
        };
        assert_eq!(
            nearest_work_rect(&screens, on_secondary),
            Some(PxRect {
                x: 1200,
                y: 0,
                width: 800,
                height: 600
            })
        );
        // 120 px past the primary, 60 px short of the secondary.
        let in_the_gap = PxRect {
            x: 1120,
            y: 10,
            width: 20,
            height: 20,
        };
        assert_eq!(
            nearest_work_rect(&screens, in_the_gap).unwrap().x,
            1200,
            "closer to the secondary screen"
        );
    }

    #[test]
    fn work_area_cardinals_follow_the_current_desktop() {
        let values = [0, 0, 1920, 1040, 10, 20, 1280, 800];
        assert_eq!(
            work_area_from_cardinals(0, &values),
            Some(PxRect {
                x: 0,
                y: 0,
                width: 1920,
                height: 1040
            })
        );
        assert_eq!(
            work_area_from_cardinals(1, &values),
            Some(PxRect {
                x: 10,
                y: 20,
                width: 1280,
                height: 800
            })
        );
        assert_eq!(
            work_area_from_cardinals(3, &values),
            Some(PxRect {
                x: 0,
                y: 0,
                width: 1920,
                height: 1040
            })
        );
        assert_eq!(
            work_area_from_cardinals(0, &[0, 0, 0, 100]),
            None,
            "an empty work area is not usable"
        );
        assert_eq!(work_area_from_cardinals(0, &[0, 0]), None);
    }

    #[test]
    fn scale_prefers_xft_dpi_and_otherwise_follows_randr() {
        let panel = monitor(0, 0, 2880, 1800, true);
        assert_eq!(layout_scale(&panel, Some(192.0)), 2.0);
        let measured = Monitor {
            mm_width: 290,
            mm_height: 180,
            ..panel
        };
        assert!(
            (randr_scale(&measured) - 8.0 / 3.0).abs() < 1e-9,
            "2880x1800 over 290x180 mm"
        );
        let exact = Monitor {
            rect: PxRect {
                x: 0,
                y: 0,
                width: 2560,
                height: 1440,
            },
            mm_width: 310,
            mm_height: 170,
            primary: true,
        };
        assert_eq!(randr_scale(&exact), 2.25);
        let unknown = Monitor {
            mm_width: 0,
            mm_height: 0,
            ..panel
        };
        assert_eq!(randr_scale(&unknown), 1.0);
        assert_eq!(
            layout_scale(&unknown, Some(0.0)),
            1.0,
            "a nonsense dpi falls through"
        );
        assert!(scale_from_dpi(f64::NAN).is_none());
    }

    #[test]
    fn xsettings_dpi_is_the_integer_divided_by_1024() {
        let mut data = Vec::new();
        push_header(&mut data, 2, true);
        push_string(&mut data, "Gtk/ThemeName", "Adwaita", true);
        push_int(&mut data, "Xft/DPI", 192 * 1024, true);
        assert_eq!(xft_dpi_from_xsettings(&data), Some(192.0));

        let mut big = Vec::new();
        push_header(&mut big, 1, false);
        push_int(&mut big, "Xft/DPI", 144 * 1024, false);
        assert_eq!(xft_dpi_from_xsettings(&big), Some(144.0));

        assert_eq!(xft_dpi_from_xsettings(&[]), None);
        assert_eq!(xft_dpi_from_xsettings(b"l"), None);
        let mut other = Vec::new();
        push_header(&mut other, 1, true);
        push_int(&mut other, "Xft/Antialias", 1, true);
        assert_eq!(xft_dpi_from_xsettings(&other), None);
    }

    fn push_header(buf: &mut Vec<u8>, count: i32, little: bool) {
        buf.push(if little { b'l' } else { b'B' });
        buf.extend([0, 0, 0]);
        push_i32(buf, 0, little);
        push_i32(buf, count, little);
    }

    fn push_int(buf: &mut Vec<u8>, name: &str, value: i32, little: bool) {
        buf.push(0);
        buf.push(0);
        let bytes = name.as_bytes();
        push_i16(buf, bytes.len() as i16, little);
        buf.extend(bytes);
        buf.extend(std::iter::repeat_n(0, (4 - (bytes.len() % 4)) % 4));
        push_i32(buf, 1, little);
        push_i32(buf, value, little);
    }

    fn push_string(buf: &mut Vec<u8>, name: &str, value: &str, little: bool) {
        buf.push(1);
        buf.push(0);
        let bytes = name.as_bytes();
        push_i16(buf, bytes.len() as i16, little);
        buf.extend(bytes);
        buf.extend(std::iter::repeat_n(0, (4 - (bytes.len() % 4)) % 4));
        push_i32(buf, 1, little);
        let text = value.as_bytes();
        push_i32(buf, text.len() as i32, little);
        buf.extend(text);
        buf.extend(std::iter::repeat_n(0, (4 - (text.len() % 4)) % 4));
    }

    fn push_i16(buf: &mut Vec<u8>, value: i16, little: bool) {
        buf.extend(if little {
            value.to_le_bytes()
        } else {
            value.to_be_bytes()
        });
    }

    fn push_i32(buf: &mut Vec<u8>, value: i32, little: bool) {
        buf.extend(if little {
            value.to_le_bytes()
        } else {
            value.to_be_bytes()
        });
    }

    #[test]
    fn edges_that_are_not_finite_do_not_become_a_rectangle() {
        let rect = PxRect::from_f32_edges(1.2, 3.6, 10.0, 8.4).unwrap();
        assert_eq!((rect.x, rect.y, rect.right(), rect.bottom()), (1, 4, 10, 8));
        assert!(PxRect::from_f32_edges(f32::NAN, 0.0, 10.0, 10.0).is_none());
        assert!(PxRect::from_f32_edges(0.0, 0.0, -1.0, 10.0).is_none());
    }
}
