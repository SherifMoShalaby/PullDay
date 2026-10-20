//! PullDay for Linux: the tray icon, its panel and the app's lifetime.

mod desktop;
mod place;
mod view;
mod worker;

use pullday_core::prefs::{default_path, Prefs};
use pullday_core::scanner::Scanner;
use pullday_core::shell::{child_env, home, log_path, DROP_GDK_BACKEND};
use pullday_core::state::Core;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime};
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{
    AppHandle, Emitter, LogicalSize, Manager, PhysicalPosition, RunEvent, State, WebviewUrl,
    WebviewWindow, WebviewWindowBuilder, WindowEvent, Wry,
};
use worker::{Job, Worker};

const NO_TRAY: &str =
    "No tray icon: your desktop has no StatusNotifierItem host. On GNOME, install and enable AppIndicator support.";

/// State shared by the tray, window and socket handlers.
struct Shell {
    has_tray: AtomicBool,
    /// When the panel last hid on focus loss (click-to-close).
    hidden_at: Mutex<Option<Instant>>,
    /// Where the panel last opened, so a height change re-places it there.
    anchor: Mutex<Option<PhysicalPosition<f64>>>,
    /// The panel's height in logical pixels, as its page last reported it.
    height: Mutex<f64>,
    /// The tray menu's entries, in `view::tray_menu` order.
    menu: Mutex<Vec<MenuItem<Wry>>>,
    /// The Settings tab last shown, so reopening Settings returns to it (the Mac's `settingsTab`).
    settings_tab: Mutex<String>,
}

impl Default for Shell {
    fn default() -> Self {
        Shell {
            has_tray: AtomicBool::default(),
            hidden_at: Mutex::default(),
            anchor: Mutex::default(),
            height: Mutex::new(place::PANEL_SIZE.1),
            menu: Mutex::default(),
            settings_tab: Mutex::new("general".into()),
        }
    }
}

fn main() {
    // The systemd timer's poke: never starts PullDay or touches Core, and always exits 0.
    if std::env::args_os().skip(1).any(|a| a == "--check") {
        if let Some(dir) = desktop::runtime_dir(|k| std::env::var_os(k)) {
            desktop::poke_check(&dir);
        }
        return;
    }
    // Before GTK or any thread starts.
    let var = std::env::var_os;
    if place::force_x11(
        var("WAYLAND_DISPLAY").as_deref(),
        var("DISPLAY").as_deref(),
        var("GDK_BACKEND").as_deref(),
    ) {
        std::env::set_var("GDK_BACKEND", "x11");
        // Only PullDay's own GTK needs it; the tools and file manager it starts pick their own backend.
        DROP_GDK_BACKEND.store(true, SeqCst);
    }

    let runtime = desktop::runtime_dir(|k| std::env::var_os(k));
    let instance = match runtime.clone() {
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

    let has_instance = instance.is_some();
    let app = tauri::Builder::default()
        .manage(Shell::default())
        .invoke_handler(tauri::generate_handler![
            snapshot,
            check_now,
            update,
            update_all,
            skip_update,
            stop_skipping,
            set_ignored,
            set_setting,
            clear_log,
            set_launch_at_login,
            show_window,
            remember_tab,
            open_item,
            open_log,
            panel_height,
            hide_panel,
            quit
        ])
        .setup(move |app| {
            let (w, h) = place::PANEL_SIZE;
            // Created hidden once, so a later show never races a build.
            WebviewWindowBuilder::new(app, "panel", WebviewUrl::App("panel.html".into()))
                .title("PullDay")
                .inner_size(w, h)
                // GTK won't shrink a non-resizable window to a smaller requested height (panel_height).
                .resizable(true)
                .decorations(false)
                .skip_taskbar(true)
                .always_on_top(true)
                .visible(false)
                .build()?;
            let core = Core::new(
                Scanner::new(home().to_string_lossy()),
                Prefs::load(&default_path()),
                log_path(),
            );
            let handle = app.handle().clone();
            let icon = tauri::include_image!("icons/icon.png");
            let notices = desktop::notifier(desktop::Icon {
                width: icon.width(),
                height: icon.height(),
                rgba: icon.rgba().to_vec(),
            });
            let worker = Worker::start(core, move |snap| on_snapshot(&handle, snap), notices);
            worker.submit(Job::Startup);
            app.manage(worker);
            start_schedule(app.handle());
            start_polling(app.handle());
            std::thread::spawn(install_check_timer);
            if let Some(instance) = instance {
                let app = app.handle().clone();
                std::thread::spawn(move || {
                    desktop::serve(instance, |request| match request {
                        desktop::Request::Show => show_panel(&app, None),
                        desktop::Request::Check => {
                            app.state::<Worker>().submit(Job::RunIfDue);
                        }
                    })
                });
            }
            // No tray host is normal (GNOME without AppIndicator support): run as a window and keep trying.
            if let Err(e) = build_tray(app.handle()) {
                eprintln!("PullDay: {NO_TRAY} ({e})");
                set_tray_style(app.handle(), false);
                republish(app.handle());
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

    // Only the instance's own socket, and only while it still holds the lock: then the timer's
    // `ConditionPathExists` skips the service once PullDay quits (a crash leaves it; `--check` then exits 0).
    let socket = runtime
        .filter(|_| has_instance)
        .map(|dir| dir.join("app.sock"));
    app.run(move |_, event| match event {
        // A tray app outlives its windows; only Quit (code Some(0)) exits.
        RunEvent::ExitRequested {
            code: None, api, ..
        } => api.prevent_exit(),
        RunEvent::Exit => {
            if let Some(socket) = &socket {
                let _ = std::fs::remove_file(socket);
            }
        }
        _ => {}
    });
}

/// The Mac's 15-minute timer plus its wake notification: a minute's tick that notices a suspend (the wall clock ran
/// ahead of the monotonic one), waits a minute for the network and runs a scheduled check if one is due.
fn start_schedule(app: &AppHandle) {
    let app = app.clone();
    std::thread::spawn(move || {
        let minute = Duration::from_secs(60);
        let mut ticks = 0u32;
        let mut last = (SystemTime::now(), Instant::now());
        loop {
            std::thread::sleep(minute);
            let wall = SystemTime::now().duration_since(last.0).unwrap_or_default();
            let woke = desktop::slept(wall, last.1.elapsed());
            ticks += 1;
            if woke {
                std::thread::sleep(minute);
            }
            if woke || ticks.is_multiple_of(15) {
                app.state::<Worker>().submit(Job::RunIfDue);
            }
            last = (SystemTime::now(), Instant::now());
        }
    });
}

/// Rescans soon after an entry is added, removed or renamed in a watched folder (the Mac's `watchFolders`).
fn start_polling(app: &AppHandle) {
    let app = app.clone();
    std::thread::spawn(move || loop {
        std::thread::sleep(desktop::QUIET);
        let worker = app.state::<Worker>();
        let dirs = worker.cache.lock().unwrap().watched.clone();
        let phase = worker.phase.load(SeqCst);
        worker
            .poller
            .lock()
            .unwrap()
            .poll(&dirs, phase, Instant::now(), || {
                worker.submit(Job::Refresh { record_run: false })
            });
    });
}

/// The systemd user timer that pokes this PullDay (`--check`), written for wherever this binary now is.
fn install_check_timer() {
    let result = std::env::current_exe()
        .map_err(|e| e.to_string())
        .and_then(|exe| desktop::autostart_program(&exe))
        .and_then(|program| {
            let config = default_path()
                .parent()
                .and_then(Path::parent)
                .map(Path::to_path_buf);
            // The user manager reads its own XDG_CONFIG_HOME, which may not be this session's; a failure is logged.
            desktop::install_check_timer(&config.ok_or("No config folder")?, &program)
        });
    if let Err(e) = result {
        desktop::log_failure("Couldn't set up the scheduled check timer", &e);
    }
}

fn build_tray(app: &AppHandle) -> tauri::Result<()> {
    let shell = app.state::<Shell>();
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
            "update_all" => {
                app.state::<Worker>().submit(Job::UpdateAll);
            }
            "check" => {
                app.state::<Worker>()
                    .submit(Job::Refresh { record_run: true });
            }
            "settings" => show_window(app.clone(), "settings".into(), None),
            "quit" => app.exit(0),
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
    *shell.menu.lock().unwrap() = items;
    shell.has_tray.store(true, SeqCst);
    republish(app);
    Ok(())
}

/// Sends a snapshot to the windows and the tray menu.
fn on_snapshot(app: &AppHandle, snap: &Value) {
    let shell = app.state::<Shell>();
    let count = snap["updateAllIds"].as_array().map_or(0, Vec::len);
    let busy = snap["phase"] != "idle";
    // A copy, so no lock is held while these calls wait for the GTK thread.
    let menu = shell.menu.lock().unwrap().clone();
    for (item, (_, label, enabled)) in menu.iter().zip(view::tray_menu(count, busy)) {
        let _ = item.set_text(label);
        let _ = item.set_enabled(enabled);
    }
    if let Some(tray) = app.tray_by_id("main") {
        let tip = match count {
            0 => "PullDay".to_string(),
            1 => "PullDay: 1 update available".to_string(),
            n => format!("PullDay: {n} updates available"),
        };
        let _ = tray.set_tooltip(Some(tip));
    }
    let _ = app.emit("snapshot", with_note(app, snap.clone()));
}

/// Re-sends the cached snapshot, e.g. once the tray appears.
fn republish(app: &AppHandle) {
    if let Some(worker) = app.try_state::<Worker>() {
        let snap = worker.cache.lock().unwrap().snapshot.clone();
        on_snapshot(app, &snap);
    }
}

/// Adds what the app shell knows and the worker doesn't.
fn with_note(app: &AppHandle, mut snap: Value) -> Value {
    snap["trayNote"] = if app.state::<Shell>().has_tray.load(SeqCst) {
        Value::Null
    } else {
        NO_TRAY.into()
    };
    // Read from the file each time, so removing it by hand shows as off.
    snap["prefs"]["launchAtLogin"] = autostart_file()
        .and_then(|f| std::fs::read_to_string(f).ok())
        .is_some_and(|entry| desktop::autostart_on(&entry))
        .into();
    snap
}

/// `~/.config/autostart/pullday.desktop` (under `$XDG_CONFIG_HOME` when that's set), next to PullDay's own config.
fn autostart_file() -> Option<PathBuf> {
    let config = default_path().parent()?.parent()?.to_path_buf();
    Some(desktop::autostart_file(&config))
}

// Commands. The page sends item ids only; paths and commands are looked up here.

#[tauri::command]
fn snapshot(app: AppHandle, worker: State<Worker>) -> Value {
    let snap = worker.cache.lock().unwrap().snapshot.clone();
    with_note(&app, snap)
}

#[tauri::command]
fn check_now(worker: State<Worker>) {
    worker.submit(Job::Refresh { record_run: true });
}

#[tauri::command]
fn update(ids: Vec<String>, worker: State<Worker>) {
    worker.submit(Job::Update(ids));
}

#[tauri::command]
fn update_all(worker: State<Worker>) {
    worker.submit(Job::UpdateAll);
}

#[tauri::command]
fn skip_update(id: String, worker: State<Worker>) {
    worker.submit(Job::Skip(id));
}

#[tauri::command]
fn stop_skipping(id: String, worker: State<Worker>) {
    worker.submit(Job::StopSkipping(id));
}

#[tauri::command]
fn set_ignored(id: String, on: bool, worker: State<Worker>) {
    worker.submit(Job::SetIgnored(id, on));
}

/// A Settings control, checked here and saved on the worker. A source toggle rescans, as the Mac's
/// `onChange(of: enabledKinds)` does (dropped while a check or update runs, as there).
#[tauri::command]
fn set_setting(key: String, value: Value, worker: State<Worker>) -> Result<(), String> {
    let setting = worker::parse_setting(&key, &value)?;
    let rescan = matches!(setting, worker::Setting::Kind(..));
    worker.submit(Job::SetSetting(setting));
    if rescan {
        worker.submit(Job::Refresh { record_run: true });
    }
    Ok(())
}

#[tauri::command]
fn clear_log(worker: State<Worker>) {
    worker.submit(Job::ClearLog);
}

/// Open PullDay at login: an XDG autostart entry for this binary.
#[tauri::command]
fn set_launch_at_login(app: AppHandle, on: bool) -> Result<(), String> {
    let file = autostart_file().ok_or("No config folder")?;
    let program = if on {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        Some(desktop::autostart_program(&exe)?)
    } else {
        None
    };
    let result = desktop::set_autostart(&file, program.as_deref()).map_err(|e| e.to_string());
    republish(&app);
    result
}

/// Settings (tab `general`, `sources` or `items`; none keeps the current one) or the Log, created on demand and
/// destroyed on close.
#[tauri::command]
fn show_window(app: AppHandle, name: String, tab: Option<String>) {
    if !["settings", "log"].contains(&name.as_str()) {
        return;
    }
    let tab = tab.filter(|t| SETTINGS_TABS.contains(&t.as_str()));
    if let Some(window) = app.get_webview_window(&name) {
        if let Some(tab) = tab.as_ref().filter(|_| name == "settings") {
            let _ = app.emit_to("settings", "tab", tab);
        }
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
        return;
    }
    let built = match name.as_str() {
        "settings" => {
            let tab =
                tab.unwrap_or_else(|| app.state::<Shell>().settings_tab.lock().unwrap().clone());
            let page = format!("settings.html#{tab}");
            WebviewWindowBuilder::new(&app, "settings", WebviewUrl::App(page.into()))
                .title("PullDay Settings")
                .inner_size(580.0, 480.0)
                .resizable(false)
                .build()
        }
        "log" => WebviewWindowBuilder::new(&app, "log", WebviewUrl::App("log.html".into()))
            .title("PullDay Log")
            .inner_size(640.0, 460.0)
            .min_inner_size(560.0, 380.0)
            .build(),
        _ => return,
    };
    match built {
        Ok(window) => {
            let _ = window.set_focus();
        }
        Err(e) => eprintln!("PullDay: couldn't open the {name} window: {e}"),
    }
}

const SETTINGS_TABS: [&str; 3] = ["general", "sources", "items"];

/// The Settings page reports the tab it shows.
#[tauri::command]
fn remember_tab(app: AppHandle, tab: String) {
    if SETTINGS_TABS.contains(&tab.as_str()) {
        *app.state::<Shell>().settings_tab.lock().unwrap() = tab;
    }
}

/// Show in Files.
#[tauri::command]
fn open_item(id: String, worker: State<Worker>) {
    let path = worker.cache.lock().unwrap().paths.get(&id).cloned();
    if let Some(path) = path {
        xdg_open(desktop::folder_to_open(Path::new(&path)));
    }
}

#[tauri::command]
fn open_log() {
    xdg_open(&log_path());
}

/// The panel page reports its content height; the window follows it.
#[tauri::command]
fn panel_height(app: AppHandle, px: f64) {
    let shell = app.state::<Shell>();
    let height = px.clamp(120.0, 720.0);
    *shell.height.lock().unwrap() = height;
    let Some(panel) = app.get_webview_window("panel") else {
        return;
    };
    let _ = panel.set_size(LogicalSize::new(place::PANEL_SIZE.0, height));
    // Without a tray it's a normal window the user may have moved.
    if shell.has_tray.load(SeqCst) && panel.is_visible().unwrap_or(false) {
        place_panel(&app, &panel);
    }
}

/// Escape. Only the tray's popup hides; without a tray the panel is a normal window, as on focus loss.
#[tauri::command]
fn hide_panel(window: WebviewWindow) {
    if window.label() == "panel" && window.state::<Shell>().has_tray.load(SeqCst) {
        let _ = window.hide();
    }
}

#[tauri::command]
fn quit(app: AppHandle) {
    app.exit(0);
}

fn xdg_open(path: &Path) {
    let mut c = desktop::open_command(path);
    match child_env(&mut c).spawn() {
        Ok(mut child) => {
            std::thread::spawn(move || child.wait());
        }
        Err(e) => eprintln!("PullDay: couldn't run xdg-open: {e}"),
    }
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
    *app.state::<Shell>().anchor.lock().unwrap() = anchor;
    // Show first, then place: window managers ignore positions set on a hidden window.
    let _ = panel.show();
    place_panel(app, &panel);
    let _ = panel.set_focus();
}

fn place_panel(app: &AppHandle, panel: &WebviewWindow) {
    let shell = app.state::<Shell>();
    let anchor = *shell.anchor.lock().unwrap();
    let size = (place::PANEL_SIZE.0, *shell.height.lock().unwrap());
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
            size,
            monitor.scale_factor(),
            work,
        );
        let _ = panel.set_position(PhysicalPosition::new(x, y));
    }
}
