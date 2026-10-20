//! Pure window-placement decisions: the X11 backend choice and where the panel opens.

use std::ffi::OsStr;
use std::time::{Duration, Instant};

/// Panel width and default height, in logical pixels.
pub const PANEL_SIZE: (f64, f64) = (380.0, 480.0);
const GAP: f64 = 8.0;
/// The monitor whose rect (x, y, width, height, physical pixels) contains `point`. Not GDK's monitor_from_point,
/// which takes logical coordinates and so picks the wrong monitor on HiDPI multi-monitor setups.
pub fn monitor_at(point: (f64, f64), monitors: &[(f64, f64, f64, f64)]) -> Option<usize> {
    let (px, py) = point;
    monitors
        .iter()
        .position(|&(x, y, w, h)| px >= x && px < x + w && py >= y && py < y + h)
}

/// A tray click this soon after the panel hid on focus loss is the same click that hid it.
const CLICK_AFTER_HIDE: Duration = Duration::from_millis(300);

/// Wayland apps can't place their windows, so run under XWayland when it's there and the user hasn't chosen a backend.
pub fn force_x11(
    wayland: Option<&OsStr>,
    display: Option<&OsStr>,
    gdk_backend: Option<&OsStr>,
) -> bool {
    let set = |v: Option<&OsStr>| v.is_some_and(|v| !v.is_empty());
    set(wayland) && set(display) && !set(gdk_backend)
}

/// The panel's top-left in physical pixels: centered on `anchor`, above it in the lower half of `work_area`
/// (x, y, width, height) and below it in the upper half, kept inside the work area; a panel taller than the
/// work area sits at its top. With no anchor it's centered in the work area.
pub fn panel_origin(
    anchor: Option<(f64, f64)>,
    logical_size: (f64, f64),
    scale: f64,
    work_area: (f64, f64, f64, f64),
) -> (f64, f64) {
    let (w, h) = (logical_size.0 * scale, logical_size.1 * scale);
    let (wx, wy, ww, wh) = work_area;
    let (ax, y) = match anchor {
        Some((ax, ay)) if ay > wy + wh / 2.0 => (ax, ay - h - GAP * scale),
        Some((ax, ay)) => (ax, ay + GAP * scale),
        None => (wx + ww / 2.0, wy + (wh - h) / 2.0),
    };
    let x = (ax - w / 2.0).min(wx + ww - w).max(wx);
    (x, y.min(wy + wh - h).max(wy))
}

/// Muffin applies a move before the resize asked for with it and keeps the still-taller panel on screen by pushing it
/// up, so a pending re-place runs when a configure first reports the height asked for (once: every move configures).
pub fn place_on_configure(pending: bool, reported: u32, height: f64, scale: f64) -> bool {
    pending && (f64::from(reported) - height * scale).abs() < 1.0
}

/// Open PullDay's anchor. Under forced XWayland the pointer is XWayland's last one, stale while it was over the
/// compositor's own menu: then the last tray click, else the work area's top-right (GNOME's tray). X11 keeps the pointer.
pub fn menu_anchor(
    forced_x11: bool,
    pointer: Option<(f64, f64)>,
    click: Option<(f64, f64)>,
    top_right: Option<(f64, f64)>,
) -> Option<(f64, f64)> {
    if forced_x11 {
        click.or(top_right)
    } else {
        pointer
    }
}

/// Clicking the tray icon while the panel is open first hides it (focus loss), then delivers the click:
/// that click must not reopen it.
pub fn ignore_activate(hidden_at: Option<Instant>, now: Instant) -> bool {
    hidden_at.is_some_and(|t| now.saturating_duration_since(t) < CLICK_AFTER_HIDE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn force_x11_only_on_wayland_with_xwayland_and_no_backend_chosen() {
        let s = |v: &'static str| Some(OsStr::new(v));
        assert!(force_x11(s("wayland-0"), s(":0"), None));
        assert!(!force_x11(s("wayland-0"), None, None));
        assert!(!force_x11(s("wayland-0"), s(":0"), s("wayland")));
        assert!(!force_x11(None, s(":0"), None));
        assert!(!force_x11(None, None, None));
    }

    const SCREEN: (f64, f64, f64, f64) = (0.0, 0.0, 1920.0, 1047.0); // a 33 px bottom panel

    #[test]
    fn bottom_panel_opens_above_the_click() {
        assert_eq!(
            panel_origin(Some((960.0, 1064.0)), PANEL_SIZE, 1.0, SCREEN),
            (770.0, 567.0)
        );
    }

    #[test]
    fn top_panel_opens_below_the_click_inside_the_work_area() {
        let work = (0.0, 32.0, 1920.0, 1048.0);
        assert_eq!(
            panel_origin(Some((960.0, 16.0)), PANEL_SIZE, 1.0, work),
            (770.0, 32.0)
        );
    }

    #[test]
    fn clamped_to_the_right_and_left_edges() {
        assert_eq!(
            panel_origin(Some((1900.0, 1064.0)), PANEL_SIZE, 1.0, SCREEN).0,
            1540.0
        );
        assert_eq!(
            panel_origin(Some((20.0, 1064.0)), PANEL_SIZE, 1.0, SCREEN).0,
            0.0
        );
    }

    #[test]
    fn taller_than_the_work_area_sits_at_its_top() {
        let work = (0.0, 24.0, 1280.0, 400.0);
        assert_eq!(
            panel_origin(Some((640.0, 410.0)), PANEL_SIZE, 1.0, work).1,
            24.0
        );
    }

    #[test]
    fn second_monitor_to_the_right() {
        let work = (1920.0, 0.0, 1920.0, 1047.0);
        assert_eq!(
            panel_origin(Some((3830.0, 1064.0)), PANEL_SIZE, 1.0, work),
            (3460.0, 567.0)
        );
        assert_eq!(
            panel_origin(Some((1930.0, 1064.0)), PANEL_SIZE, 1.0, work).0,
            1920.0
        );
    }

    #[test]
    fn scale_2_uses_physical_size() {
        let work = (0.0, 0.0, 3840.0, 2094.0);
        assert_eq!(
            panel_origin(Some((3830.0, 2128.0)), PANEL_SIZE, 2.0, work),
            (3080.0, 1134.0)
        );
        // The 8 px gap is logical: 16 physical, above and below the anchor.
        assert_eq!(
            panel_origin(Some((1000.0, 1500.0)), PANEL_SIZE, 2.0, work),
            (620.0, 524.0)
        );
        assert_eq!(
            panel_origin(Some((1000.0, 40.0)), PANEL_SIZE, 2.0, work),
            (620.0, 56.0)
        );
    }

    #[test]
    fn work_area_offset_decides_above_or_below_from_its_own_middle() {
        // Middle of this work area is y = 590: a click at 560 is in its upper half, so the panel opens below.
        let work = (0.0, 100.0, 1920.0, 980.0);
        assert_eq!(
            panel_origin(Some((960.0, 560.0)), PANEL_SIZE, 1.0, work),
            (770.0, 568.0)
        );
    }

    #[test]
    fn monitor_at_picks_the_monitor_containing_the_physical_point() {
        // Two 4K monitors at scale 2, side by side, in physical pixels.
        let monitors = [(0.0, 0.0, 3840.0, 2160.0), (3840.0, 0.0, 3840.0, 2160.0)];
        assert_eq!(monitor_at((3700.0, 2150.0), &monitors), Some(0));
        assert_eq!(monitor_at((3840.0, 10.0), &monitors), Some(1));
        assert_eq!(monitor_at((7700.0, 10.0), &monitors), None);
    }

    #[test]
    fn no_anchor_centers_in_the_work_area() {
        let work = (1920.0, 32.0, 1920.0, 1048.0);
        assert_eq!(panel_origin(None, PANEL_SIZE, 1.0, work), (2690.0, 316.0));
    }

    #[test]
    fn re_placed_once_when_the_asked_height_lands() {
        assert!(place_on_configure(true, 237, 237.0, 1.0));
        assert!(!place_on_configure(true, 480, 237.0, 1.0)); // the resize hasn't landed yet
        assert!(!place_on_configure(false, 237, 237.0, 1.0)); // already re-placed: the move configures too
        assert!(place_on_configure(true, 474, 237.0, 2.0));
    }

    #[test]
    fn open_pullday_anchor() {
        let (pointer, click, corner) = (
            Some((640.0, 400.0)),
            Some((1100.0, 16.0)),
            Some((1279.0, 32.0)),
        );
        assert_eq!(menu_anchor(false, pointer, click, corner), pointer);
        assert_eq!(menu_anchor(true, pointer, click, corner), click);
        assert_eq!(menu_anchor(true, pointer, None, corner), corner);
    }

    #[test]
    fn a_click_right_after_a_blur_hide_is_ignored() {
        let t = Instant::now();
        assert!(ignore_activate(Some(t), t + Duration::from_millis(100)));
        assert!(!ignore_activate(Some(t), t + Duration::from_millis(500)));
        assert!(!ignore_activate(None, t));
    }
}
