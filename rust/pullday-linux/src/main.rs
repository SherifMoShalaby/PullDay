//! PullDay for Linux: the tray icon, its panel and the app's lifetime.

mod desktop;
mod place;
mod view;

use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{
    AppHandle, Manager, PhysicalPosition, RunEvent, WebviewUrl, WebviewWindowBuilder, WindowEvent,
};

const NO_TRAY: &str =
    "No tray icon: your desktop has no StatusNotifierItem host. On GNOME, install and enable AppIndicator support.";

/// State shared by the tray, window and socket handlers.
#[derive(Default)]
struct Shell {
    has_tray: AtomicBool,
    /// When the panel last hid on focus loss (click-to-close).
    hidden_at: Mutex<Option<Instant>>,
}

fn main() {
    // Before GTK or any thread starts.
    let var = std::env::var_os;
    if place::force_x11(
        var("WAYLAND_DISPLAY").as_deref(),
        var("DISPLAY").as_deref(),
        var("GDK_BACKEND").as_deref(),
    ) {
        std::env::set_var("GDK_BACKEND", "x11");
    }

    let instance = match desktop::runtime_dir(|k| std::env::var_os(k)) {
        None => {
            eprintln!(
                "PullDay: no XDG_RUNTIME_DIR, XDG_STATE_HOME or HOME, so no single-instance check"
            );
            None
        }
        Some(dir) => match desktop::acquire(&dir) {
            Ok(Some(instance)) => Some(instance),
            Ok(None) => {
                if let Err(e) = desktop::send(&dir, "show") {
                    eprintln!("PullDay is already running but didn't answer: {e}");
                    std::process::exit(1);
                }
                return;
            }
            Err(e) => {
                eprintln!("PullDay: no single-instance lock in {}: {e}", dir.display());
                None
            }
        },
    };

    let app = tauri::Builder::default()
        .manage(Shell::default())
        .setup(move |app| {
            let (w, h) = place::PANEL_SIZE;
            // Created hidden once, so a later show never races a build.
            WebviewWindowBuilder::new(app, "panel", WebviewUrl::App("panel.html".into()))
                .title("PullDay")
                .inner_size(w, h)
                .resizable(false)
                .decorations(false)
                .skip_taskbar(true)
                .always_on_top(true)
                .visible(false)
                .build()?;
            if let Some(instance) = instance {
                let app = app.handle().clone();
                std::thread::spawn(move || desktop::serve(instance, || show_panel(&app, None)));
            }
            // No tray host is normal (GNOME without AppIndicator support): run as a window and keep trying.
            if let Err(e) = build_tray(app.handle()) {
                eprintln!("PullDay: {NO_TRAY} ({e})");
                set_tray_style(app.handle(), false);
                let app = app.handle().clone();
                std::thread::spawn(move || {
                    for _ in 0..30 {
                        std::thread::sleep(Duration::from_secs(2));
                        if build_tray(&app).is_ok() {
                            return set_tray_style(&app, true);
                        }
                    }
                });
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            if window.label() != "panel" {
                return;
            }
            let shell = window.state::<Shell>();
            match event {
                WindowEvent::Focused(false) if shell.has_tray.load(SeqCst) => {
                    let _ = window.hide();
                    *shell.hidden_at.lock().unwrap() = Some(Instant::now());
                }
                WindowEvent::CloseRequested { api, .. } => {
                    api.prevent_close();
                    let _ = window.hide();
                }
                _ => {}
            }
        })
        .build(tauri::generate_context!())
        .expect("PullDay failed to start");

    app.run(|_, event| {
        // A tray app outlives its windows; only Quit (code Some(0)) exits.
        if let RunEvent::ExitRequested {
            code: None, api, ..
        } = event
        {
            api.prevent_exit();
        }
    });
}

fn build_tray(app: &AppHandle) -> tauri::Result<()> {
    let items = view::tray_menu(0, false)
        .into_iter()
        .map(|(id, label, enabled)| MenuItem::with_id(app, id, label, enabled, None::<&str>))
        .collect::<tauri::Result<Vec<_>>>()?;
    let separator = PredefinedMenuItem::separator(app)?;
    let menu = Menu::with_items(
        app,
        &[
            &items[0], &items[1], &items[2], &separator, &items[3], &items[4],
        ],
    )?;
    TrayIconBuilder::with_id("main")
        .icon(tauri::include_image!("icons/tray.png"))
        .tooltip("PullDay")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "open" => show_panel(app, app.cursor_position().ok()),
            "quit" => app.exit(0),
            // shortcut: Update all, Check now and Settings… do nothing until the worker and Settings window exist.
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                position,
                ..
            } = event
            {
                toggle_panel(tray.app_handle(), position);
            }
        })
        .build(app)?;
    app.state::<Shell>().has_tray.store(true, SeqCst);
    Ok(())
}

/// With a tray the panel is a borderless popup that hides on focus loss; without one it's a normal window.
fn set_tray_style(app: &AppHandle, tray: bool) {
    let Some(panel) = app.get_webview_window("panel") else {
        return;
    };
    let _ = panel.set_decorations(!tray);
    let _ = panel.set_skip_taskbar(tray);
    let _ = panel.set_always_on_top(tray);
    if tray {
        let _ = panel.hide();
    } else {
        show_panel(app, None);
    }
}

fn toggle_panel(app: &AppHandle, at: PhysicalPosition<f64>) {
    let Some(panel) = app.get_webview_window("panel") else {
        return;
    };
    if panel.is_visible().unwrap_or(false) {
        let _ = panel.hide();
    } else if !place::ignore_activate(
        *app.state::<Shell>().hidden_at.lock().unwrap(),
        Instant::now(),
    ) {
        show_panel(app, Some(at));
    }
}

/// Shows the panel at `anchor` (physical pixels), or centered when there's none.
fn show_panel(app: &AppHandle, anchor: Option<PhysicalPosition<f64>>) {
    let Some(panel) = app.get_webview_window("panel") else {
        return;
    };
    let monitors = app.available_monitors().unwrap_or_default();
    let rects: Vec<_> = monitors
        .iter()
        .map(|m| {
            (
                m.position().x as f64,
                m.position().y as f64,
                m.size().width as f64,
                m.size().height as f64,
            )
        })
        .collect();
    let monitor = anchor
        .and_then(|p| place::monitor_at((p.x, p.y), &rects))
        .and_then(|i| monitors.into_iter().nth(i))
        .or_else(|| app.primary_monitor().ok().flatten());
    // Show first, then place: window managers ignore positions set on a hidden window.
    let _ = panel.show();
    if let Some(monitor) = monitor {
        let area = monitor.work_area();
        let work = (
            area.position.x as f64,
            area.position.y as f64,
            area.size.width as f64,
            area.size.height as f64,
        );
        let (x, y) = place::panel_origin(
            anchor.map(|p| (p.x, p.y)),
            place::PANEL_SIZE,
            monitor.scale_factor(),
            work,
        );
        let _ = panel.set_position(PhysicalPosition::new(x, y));
    }
    let _ = panel.set_focus();
}
