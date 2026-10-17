//! Pure: app state -> what the tray menu and windows show.

/// One tray menu entry: id, label, enabled.
pub type MenuEntry = (&'static str, String, bool);

/// The tray menu, Open PullDay first (on GNOME a click shows this menu, not the panel); a separator goes before Settings….
pub fn tray_menu(updates: usize, busy: bool) -> [MenuEntry; 5] {
    [
        ("open", "Open PullDay".into(), true),
        (
            "update_all",
            format!("Update all ({updates})"),
            updates > 0 && !busy,
        ),
        ("check", "Check now".into(), !busy),
        ("settings", "Settings…".into(), true),
        ("quit", "Quit".into(), true),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tray_menu_labels_and_enabled_flags() {
        let menu = tray_menu(0, false);
        let ids: Vec<_> = menu.iter().map(|e| e.0).collect();
        assert_eq!(ids, ["open", "update_all", "check", "settings", "quit"]);
        assert_eq!(menu[0].1, "Open PullDay");
        assert_eq!(menu[1], ("update_all", "Update all (0)".into(), false));
        assert_eq!(
            tray_menu(3, false)[1],
            ("update_all", "Update all (3)".into(), true)
        );
        assert!(tray_menu(0, false)[2].2);

        let busy = tray_menu(3, true);
        assert!(!busy[1].2 && !busy[2].2);
        assert!(busy[0].2 && busy[3].2 && busy[4].2);
    }
}
