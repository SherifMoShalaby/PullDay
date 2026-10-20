//! `pullday --check` is what the systemd timer runs: with no PullDay running it must exit 0 at once and start nothing.

use std::process::Command;
use std::time::{Duration, Instant};

#[test]
fn check_with_no_app_running_exits_0_and_starts_nothing() {
    let dir = std::env::temp_dir().join(format!("pullday-check-bin-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let started = Instant::now();
    let mut child = Command::new(env!("CARGO_BIN_EXE_pullday"))
        .arg("--check")
        .env("XDG_RUNTIME_DIR", &dir)
        .env("HOME", &dir)
        // No display: were it to start the app, it fails (or is killed below) instead of opening windows.
        .env_remove("DISPLAY")
        .env_remove("WAYLAND_DISPLAY")
        .spawn()
        .unwrap();
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if started.elapsed() > Duration::from_secs(5) {
            let _ = child.kill();
            let _ = child.wait();
            panic!("--check didn't exit");
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    assert!(status.success(), "{status}");
    assert!(started.elapsed() < Duration::from_secs(2));
    let left: Vec<_> = std::fs::read_dir(&dir).unwrap().flatten().collect();
    assert!(left.is_empty(), "created {left:?}");
    std::fs::remove_dir_all(&dir).unwrap();
}
