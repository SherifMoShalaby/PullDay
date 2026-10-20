//! Desktop integration: single instance (a lock, then a socket), start at login, opening files, the systemd check
//! timer, the wake test, folder polling and notifications.

use crate::worker::{IDLE, UPDATING};
use pullday_core::shell::{child_env, log_append, log_path};
use pullday_core::state::Notice;
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs::{self, DirBuilder, File, OpenOptions, TryLockError};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::fs::DirBuilderExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{channel, Sender};
use std::time::{Duration, Instant, SystemTime};

/// `$XDG_RUNTIME_DIR/pullday`, else `$XDG_STATE_HOME/pullday`, else `~/.local/state/pullday`; `None` with none
/// of them set (never a directory relative to wherever PullDay was started).
pub fn runtime_dir(env: impl Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
    let var = |k| env(k).filter(|v| !v.is_empty()).map(PathBuf::from);
    let base = var("XDG_RUNTIME_DIR")
        .or_else(|| var("XDG_STATE_HOME"))
        .or_else(|| var("HOME").map(|h| h.join(".local/state")))?;
    Some(base.join("pullday"))
}

/// This process is PullDay's instance for as long as it holds the lock; the kernel drops it when the process dies.
pub struct Instance {
    _lock: File,
    listener: UnixListener,
}

/// Takes the lock and binds the socket, or `Ok(None)` when another instance holds the lock.
pub fn acquire(dir: &Path) -> io::Result<Option<Instance>> {
    DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join("app.lock"))?;
    match lock.try_lock() {
        Ok(()) => {}
        Err(TryLockError::WouldBlock) => return Ok(None),
        Err(TryLockError::Error(e)) => return Err(e),
    }
    // With the lock held, any socket file is a dead instance's leftover.
    let sock = dir.join("app.sock");
    match fs::remove_file(&sock) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
        _ => {}
    }
    Ok(Some(Instance {
        _lock: lock,
        listener: UnixListener::bind(&sock)
            .map_err(|e| io::Error::new(e.kind(), format!("can't bind {}: {e}", sock.display())))?,
    }))
}

/// Sends `line` to the running instance, retrying for ~2 s in case it hasn't bound its socket yet.
pub fn send(dir: &Path, line: &str) -> io::Result<()> {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match UnixStream::connect(dir.join("app.sock")) {
            Ok(mut stream) => return writeln!(stream, "{line}"),
            Err(e) if Instant::now() >= deadline => return Err(e),
            Err(_) => std::thread::sleep(Duration::from_millis(100)),
        }
    }
}

/// What another process may ask the running instance: a second launch's `show`, the systemd timer's `check`.
#[derive(Debug, PartialEq)]
pub enum Request {
    Show,
    Check,
}

/// Serves the socket for the life of the process: `on` for each exact `show` or `check` line, anything else ignored.
pub fn serve(instance: Instance, on: impl Fn(Request)) {
    for stream in instance.listener.incoming().flatten() {
        // A client that never writes must not block the next one.
        let _ = stream.set_read_timeout(Some(Duration::from_secs(1)));
        // Only short command lines are expected; don't buffer whatever else a client sends.
        for line in BufReader::new(stream.take(64))
            .lines()
            .map_while(Result::ok)
        {
            match line.as_str() {
                "show" => on(Request::Show),
                "check" => on(Request::Check),
                _ => {}
            }
        }
    }
}

/// Show in Files opens the item's folder; for a CLI the item's path is the program file, so its folder.
pub fn folder_to_open(path: &Path) -> &Path {
    if path.is_dir() {
        path
    } else {
        path.parent().unwrap_or(path)
    }
}

/// `program` as one `Exec=` argument, per the Desktop Entry spec: always double-quoted, with `"`, `` ` ``, `$` and
/// `\` backslash-escaped inside the quotes, `%` doubled (field codes), then the string-value escapes on top
/// (so a literal backslash ends up as four).
pub fn desktop_exec(program: &Path) -> String {
    let mut quoted = String::from("\"");
    for c in program.to_string_lossy().chars() {
        match c {
            '"' | '`' | '$' | '\\' => quoted.extend(['\\', c]),
            '%' => quoted.push_str("%%"),
            _ => quoted.push(c),
        }
    }
    quoted.push('"');
    quoted
        .replace('\\', "\\\\")
        .replace('\n', "\\n")
        .replace('\t', "\\t")
        .replace('\r', "\\r")
}

/// The XDG autostart entry that starts `program` at login. The delay gives a tray host started in the same
/// session phase (Mint's xapp-sn-watcher) time to appear first.
pub fn autostart_entry(program: &Path) -> String {
    format!(
        "[Desktop Entry]\nType=Application\nName=PullDay\nComment=Keeps your AI coding agents' skills, plugins and extensions up to date\nExec={}\nTerminal=false\nX-GNOME-Autostart-Delay=5\n",
        desktop_exec(program)
    )
}

/// `$XDG_CONFIG_HOME/autostart/pullday.desktop`; `config` is the config folder (`~/.config`).
pub fn autostart_file(config: &Path) -> PathBuf {
    config.join("autostart/pullday.desktop")
}

/// Open PullDay at login: writes the entry for `program`, or with `None` removes it. Only this one file.
pub fn set_autostart(file: &Path, program: Option<&Path>) -> io::Result<()> {
    match program {
        Some(program) => {
            if let Some(dir) = file.parent() {
                fs::create_dir_all(dir)?;
            }
            // Renamed into place, so the session never reads half a file.
            let temp = file.with_extension(format!("desktop.tmp.{}", std::process::id()));
            fs::write(&temp, autostart_entry(program))?;
            fs::rename(&temp, file)
        }
        None => match fs::remove_file(file) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        },
    }
}

/// Whether an autostart entry starts anything: the session skips one that says `Hidden=true` or
/// `X-GNOME-Autostart-enabled=false` (how desktop settings panels turn an entry off without deleting it).
pub fn autostart_on(entry: &str) -> bool {
    !entry.lines().map(str::trim).any(|line| {
        matches!(
            line.replace(' ', "").as_str(),
            "Hidden=true" | "X-GNOME-Autostart-enabled=false"
        )
    })
}

/// The program to start at login, from `current_exe`: without the " (deleted)" Linux appends once the binary was
/// replaced (a rebuild or an upgrade), and only as UTF-8, since the entry is a UTF-8 file.
pub fn autostart_program(exe: &Path) -> Result<PathBuf, String> {
    let path = exe
        .to_str()
        .ok_or_else(|| format!("PullDay's path isn't valid UTF-8: {}", exe.display()))?;
    Ok(path.strip_suffix(" (deleted)").unwrap_or(path).into())
}

/// `xdg-open <path>` with no shell, so the path is one argument whatever it contains.
pub fn open_command(path: &Path) -> Command {
    let mut c = Command::new("xdg-open");
    c.arg(path).stdin(Stdio::null());
    c
}

/// `pullday --check`, run by the systemd timer: asks a running PullDay to check if a check is due. One connect, and
/// nothing is created or started when no PullDay runs, so Quit still stops everything. True when delivered.
pub fn poke_check(dir: &Path) -> bool {
    UnixStream::connect(dir.join("app.sock"))
        .and_then(|mut stream| writeln!(stream, "check"))
        .is_ok()
}

/// `program` as the first word of an `ExecStart=` line, per systemd's rules: double-quoted with `\` and `"`
/// backslash-escaped and `%` doubled (specifiers). `$` stays: systemd 255 expands variables only in the arguments,
/// never in the program path (verified: a `$$` there stays two characters and the exec fails).
pub fn systemd_exec(program: &Path) -> String {
    let mut quoted = String::from("\"");
    for c in program.to_string_lossy().chars() {
        match c {
            '\\' | '"' => quoted.extend(['\\', c]),
            '%' => quoted.push_str("%%"),
            '\n' => quoted.push_str("\\n"),
            _ => quoted.push(c),
        }
    }
    quoted.push('"');
    quoted
}

/// The service the timer starts: after a minute (the network, after a wake, as on the Mac) it pokes the app.
/// Skipped by its condition while PullDay isn't running (`%t` is the runtime dir; `sleep` is found on
/// systemd's executable search path).
pub fn check_service(program: &Path) -> String {
    format!(
        "[Unit]\nDescription=Ask a running PullDay to check for updates\nConditionPathExists=%t/pullday/app.sock\n\n[Service]\nType=oneshot\nExecStartPre=sleep 60\nExecStart={} --check\n",
        systemd_exec(program)
    )
}

/// Every 15 minutes; `Persistent=` also catches up a run missed while the machine was off.
pub const CHECK_TIMER: &str = "[Unit]\nDescription=PullDay's scheduled check\n\n[Timer]\nOnCalendar=*:0/15\nPersistent=true\nAccuracySec=1min\n\n[Install]\nWantedBy=timers.target\n";

/// Writes `text` to `file` only when it differs, renamed into place; true when it wrote.
pub fn write_if_changed(file: &Path, text: &str) -> io::Result<bool> {
    if fs::read_to_string(file).is_ok_and(|old| old == text) {
        return Ok(false);
    }
    if let Some(dir) = file.parent() {
        fs::create_dir_all(dir)?;
    }
    let temp = file.with_extension(format!("tmp.{}", std::process::id()));
    fs::write(&temp, text)?;
    fs::rename(&temp, file)?;
    Ok(true)
}

/// Writes the timer's two units into `$XDG_CONFIG_HOME/systemd/user` (`config` is that config folder) when they
/// differ, then enables the timer. The error is for the log: the in-app schedule runs either way.
pub fn install_check_timer(config: &Path, program: &Path) -> Result<(), String> {
    let dir = config.join("systemd/user");
    let write = |name: &str, text: &str| {
        write_if_changed(&dir.join(name), text).map_err(|e| format!("can't write {name}: {e}"))
    };
    // Both, even when the first wrote.
    let service = write("pullday-check.service", &check_service(program))?;
    let timer = write("pullday-check.timer", CHECK_TIMER)?;
    for call in timer_calls(service, timer) {
        if let Err(e) = systemctl(call) {
            if call[0] == "enable" {
                // So the next launch writes it again and retries.
                let _ = fs::remove_file(dir.join("pullday-check.timer"));
            }
            return Err(e);
        }
    }
    Ok(())
}

/// The `systemctl --user` calls after writing the units: a reload when either file changed, and enabling only a
/// timer file just written (a first install), so a timer the user disabled stays off.
pub fn timer_calls(service_written: bool, timer_written: bool) -> Vec<&'static [&'static str]> {
    let mut calls: Vec<&'static [&'static str]> = Vec::new();
    if service_written || timer_written {
        calls.push(&["daemon-reload"]);
    }
    if timer_written {
        calls.push(&["enable", "--now", "pullday-check.timer"]);
    }
    calls
}

fn systemctl(args: &[&str]) -> Result<(), String> {
    let mut c = Command::new("systemctl");
    c.arg("--user").args(args).stdin(Stdio::null());
    let out = child_env(&mut c)
        .output()
        .map_err(|e| format!("can't run systemctl: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!(
            "systemctl --user {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

/// The machine slept when wall-clock time ran more than a minute ahead of the monotonic clock, which stops
/// during suspend on Linux (so a sleeping 15-minute loop alone would fire late after a wake).
pub fn slept(wall: Duration, mono: Duration) -> bool {
    wall.saturating_sub(mono) > Duration::from_secs(60)
}

/// How long the watched folders must be quiet before the rescan, so a burst of changes is one rescan.
pub const QUIET: Duration = Duration::from_secs(5);

/// Notices entries added, removed or renamed in the watched folders through each folder's own mtime (what the
/// Mac's DispatchSource watches), so a change deeper down, inside a repo, doesn't count.
/// shortcut: polling; move to inotify if it ever shows in CPU.
pub struct Poller {
    seen: BTreeMap<String, Option<SystemTime>>,
    changed: Option<Instant>,
}

fn mtimes(dirs: &[String]) -> BTreeMap<String, Option<SystemTime>> {
    dirs.iter()
        .map(|d| (d.clone(), fs::metadata(d).and_then(|m| m.modified()).ok()))
        .collect()
}

impl Poller {
    pub fn new(dirs: &[String]) -> Poller {
        Poller {
            seen: mtimes(dirs),
            changed: None,
        }
    }

    /// Takes the folders as they are now and forgets any change: after an update, which rewrites them, so the
    /// rescan doesn't wipe the "Updated" rows (the Mac's `phase != .updating` guard).
    pub fn rebaseline(&mut self, dirs: &[String]) {
        *self = Poller::new(dirs);
    }

    /// One poll at `now`. Changes while updating are ignored; `rescan` runs once the folders were quiet for
    /// `QUIET` and nothing runs, and is tried again on the next poll when it returns false (it couldn't start).
    pub fn poll(
        &mut self,
        dirs: &[String],
        phase: u8,
        now: Instant,
        rescan: impl FnOnce() -> bool,
    ) {
        if phase == UPDATING {
            return self.rebaseline(dirs);
        }
        let seen = mtimes(dirs);
        // A folder new to the list (its kind was just turned on, which rescans anyway) is only a baseline;
        // a listed root that appears (None → a time) is a change.
        if seen
            .iter()
            .any(|(dir, t)| self.seen.get(dir).is_some_and(|old| old != t))
        {
            self.changed = Some(now);
        }
        self.seen = seen;
        if phase == IDLE && self.changed.is_some_and(|t| now.duration_since(t) >= QUIET) && rescan()
        {
            self.changed = None;
        }
    }
}

/// The app icon as the notification spec's `image-data` hint (RGBA, 8 bits per sample).
pub struct Icon {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// Shows each notice through org.freedesktop.Notifications on its own thread, so a slow or missing notification
/// server never holds up a check. A failure is logged, never fatal.
pub fn notifier(icon: Icon) -> Sender<Notice> {
    let (tx, rx) = channel::<Notice>();
    std::thread::spawn(move || {
        let mut bus: Option<zbus::blocking::Connection> = None;
        for notice in rx {
            if bus.is_none() {
                // A wedged notification server must not hold the thread for zbus's default 25 s per notice.
                bus = zbus::blocking::connection::Builder::session()
                    .map(|b| b.method_timeout(Duration::from_secs(10)))
                    .and_then(|b| b.build())
                    .map_err(|e| log_failure("Couldn't show a notification", &e.to_string()))
                    .ok();
            }
            let Some(conn) = &bus else { continue };
            if let Err(e) = notify(conn, &icon, &notice) {
                log_failure("Couldn't show a notification", &e.to_string());
                bus = None; // reconnect next time, in case the session bus went away
            }
        }
    });
    tx
}

fn notify(conn: &zbus::blocking::Connection, icon: &Icon, notice: &Notice) -> zbus::Result<()> {
    use zbus::zvariant::Value;
    let image = (
        icon.width as i32,
        icon.height as i32,
        icon.width as i32 * 4,
        true,
        8i32,
        4i32,
        icon.rgba.clone(),
    );
    let hints = std::collections::HashMap::from([("image-data", Value::Structure(image.into()))]);
    conn.call_method(
        Some("org.freedesktop.Notifications"),
        "/org/freedesktop/Notifications",
        Some("org.freedesktop.Notifications"),
        "Notify",
        &(
            "PullDay",
            0u32,
            "",
            notice.title.as_str(),
            notice.body.as_str(),
            Vec::<&str>::new(),
            hints,
            -1i32,
        ),
    )
    .map(drop)
}

/// A failure the user can't otherwise see (PullDay usually starts with no terminal): stderr and PullDay's log file.
pub fn log_failure(title: &str, why: &str) {
    eprintln!("PullDay: {title}: {why}");
    log_append(&log_path(), SystemTime::now(), title, why, false);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::{mpsc, Arc, Barrier};

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("pullday-test-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    /// Serves `instance` on a thread; each `show` arrives on the returned channel.
    fn shows(instance: Instance) -> mpsc::Receiver<()> {
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            serve(instance, move |r| {
                if r == Request::Show {
                    tx.send(()).unwrap()
                }
            })
        });
        rx
    }

    #[test]
    fn two_starts_at_once_yield_one_owner_and_the_other_delivers_show() {
        let dir = temp_dir("race");
        let barrier = Arc::new(Barrier::new(2));
        let starts: Vec<_> = (0..2)
            .map(|_| {
                let (dir, barrier) = (dir.clone(), barrier.clone());
                std::thread::spawn(move || {
                    barrier.wait();
                    let owner = acquire(&dir).unwrap();
                    if owner.is_none() {
                        send(&dir, "show").unwrap();
                    }
                    owner
                })
            })
            .collect();
        let mut owners: Vec<_> = starts
            .into_iter()
            .filter_map(|t| t.join().unwrap())
            .collect();
        assert_eq!(owners.len(), 1);
        let rx = shows(owners.pop().unwrap());
        assert!(rx.recv_timeout(Duration::from_secs(2)).is_ok());
        assert_eq!(
            fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
            0o700
        );
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_leftover_socket_without_a_lock_holder_is_replaced() {
        let dir = temp_dir("leftover");
        fs::create_dir_all(&dir).unwrap();
        drop(UnixListener::bind(dir.join("app.sock")).unwrap()); // the file stays, nobody listens
        let rx = shows(acquire(&dir).unwrap().expect("owner"));
        send(&dir, "show").unwrap();
        assert!(rx.recv_timeout(Duration::from_secs(2)).is_ok());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn only_an_exact_show_line_counts() {
        let dir = temp_dir("lines");
        let rx = shows(acquire(&dir).unwrap().expect("owner"));
        send(&dir, "show now\nbogus\nSHOW\nshow").unwrap();
        assert!(rx.recv_timeout(Duration::from_secs(2)).is_ok());
        assert!(rx.recv_timeout(Duration::from_millis(300)).is_err());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_connection_is_read_only_up_to_64_bytes() {
        let dir = temp_dir("long");
        let rx = shows(acquire(&dir).unwrap().expect("owner"));
        send(&dir, &format!("{}\nshow", "x".repeat(70))).unwrap();
        assert!(rx.recv_timeout(Duration::from_millis(500)).is_err());
        send(&dir, "show").unwrap(); // the next connection still works
        assert!(rx.recv_timeout(Duration::from_secs(2)).is_ok());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn runtime_dir_falls_back_and_is_none_without_any_base() {
        fn env<'a>(vars: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<OsString> + 'a {
            move |k| vars.iter().find(|v| v.0 == k).map(|v| OsString::from(v.1))
        }
        let all = [
            ("XDG_RUNTIME_DIR", "/run/user/1000"),
            ("XDG_STATE_HOME", "/s"),
            ("HOME", "/h"),
        ];
        assert_eq!(
            runtime_dir(env(&all)),
            Some("/run/user/1000/pullday".into())
        );
        assert_eq!(
            runtime_dir(env(&[("XDG_RUNTIME_DIR", ""), ("XDG_STATE_HOME", "/s")])),
            Some("/s/pullday".into())
        );
        assert_eq!(
            runtime_dir(env(&[("HOME", "/h")])),
            Some("/h/.local/state/pullday".into())
        );
        assert_eq!(runtime_dir(env(&[])), None);
    }

    #[test]
    fn show_in_files_opens_a_folder_never_a_program() {
        let dir = temp_dir("open");
        fs::create_dir_all(&dir).unwrap();
        let program = dir.join("claude");
        fs::write(&program, "#!/bin/sh\n").unwrap();
        assert_eq!(folder_to_open(&dir), dir.as_path());
        assert_eq!(folder_to_open(&program), dir.as_path());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn desktop_exec_quotes_per_the_desktop_entry_spec() {
        let exec = |p: &str| desktop_exec(Path::new(p));
        assert_eq!(exec("/usr/bin/pullday"), r#""/usr/bin/pullday""#);
        assert_eq!(exec("/home/a b/pullday"), r#""/home/a b/pullday""#);
        assert_eq!(
            exec(r#"/x"y"#),
            r#""/x\\"y""#,
            "quote: \\\" then string-escaped"
        );
        assert_eq!(exec("/x`y"), r#""/x\\`y""#);
        assert_eq!(exec("/x$HOME"), r#""/x\\$HOME""#);
        assert_eq!(exec("/100%u"), r#""/100%%u""#, "not a field code");
        assert_eq!(exec(r"/x\y"), r#""/x\\\\y""#, "a literal backslash is four");
        assert_eq!(exec("/x\ny"), r#""/x\ny""#, "no raw newline in the file");
    }

    #[test]
    fn autostart_is_written_and_removed() {
        let dir = temp_dir("autostart");
        let file = autostart_file(&dir);
        assert_eq!(file, dir.join("autostart/pullday.desktop"));
        let other = dir.join("autostart/other.desktop");
        fs::create_dir_all(dir.join("autostart")).unwrap();
        fs::write(&other, "x").unwrap();

        set_autostart(&file, Some(Path::new("/opt/Pull Day/pullday"))).unwrap();
        let text = fs::read_to_string(&file).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], "[Desktop Entry]");
        for line in [
            "Type=Application",
            "Name=PullDay",
            r#"Exec="/opt/Pull Day/pullday""#,
            "Terminal=false",
            "X-GNOME-Autostart-Delay=5",
        ] {
            assert!(lines.contains(&line), "{line} in {text}");
        }
        set_autostart(&file, Some(Path::new("/usr/bin/pullday"))).unwrap();
        assert!(fs::read_to_string(&file)
            .unwrap()
            .contains("Exec=\"/usr/bin/pullday\"\n"));

        set_autostart(&file, None).unwrap();
        assert!(!file.exists());
        set_autostart(&file, None).unwrap(); // already off
        assert!(other.exists(), "other autostart entries are left alone");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn files_open_with_xdg_open_and_no_shell() {
        let c = open_command(Path::new("/home/a b/PullDay; rm -rf ~.log"));
        assert_eq!(c.get_program(), "xdg-open");
        let args: Vec<_> = c.get_args().collect();
        assert_eq!(args, ["/home/a b/PullDay; rm -rf ~.log"]);
    }

    #[test]
    fn an_entry_turned_off_by_the_desktop_counts_as_off() {
        let entry = autostart_entry(Path::new("/usr/bin/pullday"));
        assert!(autostart_on(&entry));
        assert!(!autostart_on(&format!("{entry}Hidden=true\n")));
        assert!(!autostart_on(&format!(
            "{entry}X-GNOME-Autostart-enabled=false\n"
        )));
        assert!(!autostart_on(&format!("{entry}Hidden = true\n")));
        assert!(autostart_on(&format!(
            "{entry}Hidden=false\nX-GNOME-Autostart-enabled=true\n"
        )));
    }

    #[test]
    fn the_autostart_program_is_the_live_utf8_path() {
        assert_eq!(
            autostart_program(Path::new("/usr/bin/pullday (deleted)")),
            Ok("/usr/bin/pullday".into())
        );
        assert_eq!(
            autostart_program(Path::new("/opt/a b/pullday")),
            Ok("/opt/a b/pullday".into())
        );
        use std::os::unix::ffi::OsStrExt;
        let bad = Path::new(std::ffi::OsStr::from_bytes(b"/opt/\xff/pullday"));
        assert!(autostart_program(bad).is_err());
    }
    #[test]
    fn only_an_exact_check_line_asks_for_a_check() {
        let dir = temp_dir("check-lines");
        let instance = acquire(&dir).unwrap().expect("owner");
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || serve(instance, move |r| tx.send(r).unwrap()));
        send(&dir, "check now\nCHECK\n check\ncheck").unwrap();
        assert_eq!(rx.recv_timeout(Duration::from_secs(2)), Ok(Request::Check));
        assert!(rx.recv_timeout(Duration::from_millis(300)).is_err());
        assert!(poke_check(&dir));
        assert_eq!(rx.recv_timeout(Duration::from_secs(2)), Ok(Request::Check));
        send(&dir, "show").unwrap();
        assert_eq!(rx.recv_timeout(Duration::from_secs(2)), Ok(Request::Show));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn check_with_no_running_app_does_nothing() {
        let dir = temp_dir("check-alone");
        let started = Instant::now();
        assert!(!poke_check(&dir));
        assert!(!dir.exists(), "creates nothing");
        // A dead instance's leftover socket: still nothing, and no waiting.
        fs::create_dir_all(&dir).unwrap();
        drop(UnixListener::bind(dir.join("app.sock")).unwrap());
        assert!(!poke_check(&dir));
        assert!(started.elapsed() < Duration::from_secs(1));
        let left: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name())
            .collect();
        assert_eq!(left, ["app.sock"]);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_wake_is_wall_time_running_ahead_of_monotonic_time() {
        let s = Duration::from_secs;
        assert!(slept(s(660), s(60)), "a 10-minute suspend");
        assert!(!slept(s(62), s(60)), "2 s of jitter");
        assert!(!slept(s(60), s(62)));
        assert!(!slept(s(0), s(60)), "the wall clock set back");
        assert!(slept(s(122), s(61)));
    }

    #[test]
    fn the_units_quote_the_program_per_systemd() {
        let exec = |p: &str| systemd_exec(Path::new(p));
        assert_eq!(exec("/usr/bin/pullday"), r#""/usr/bin/pullday""#);
        assert_eq!(exec("/home/a b/pullday"), r#""/home/a b/pullday""#);
        assert_eq!(exec("/opt/100%/pullday"), r#""/opt/100%%/pullday""#);
        assert_eq!(
            exec("/opt/$HOME/pullday"),
            r#""/opt/$HOME/pullday""#,
            "systemd doesn't expand the program path"
        );
        assert_eq!(exec(r#"/x"y\z"#), r#""/x\"y\\z""#);
        assert_eq!(exec("/x\ny"), r#""/x\ny""#, "no raw newline in the unit");

        let service = check_service(Path::new("/home/a b/100%/pullday"));
        let lines: Vec<&str> = service.lines().collect();
        for line in [
            "[Service]",
            "Type=oneshot",
            "ConditionPathExists=%t/pullday/app.sock",
            "ExecStartPre=sleep 60",
            r#"ExecStart="/home/a b/100%%/pullday" --check"#,
        ] {
            assert!(lines.contains(&line), "{line} in {service}");
        }
        let timer: Vec<&str> = CHECK_TIMER.lines().collect();
        for line in [
            "[Timer]",
            "OnCalendar=*:0/15",
            "Persistent=true",
            "AccuracySec=1min",
            "WantedBy=timers.target",
        ] {
            assert!(timer.contains(&line), "{line} in {CHECK_TIMER}");
        }
    }

    #[test]
    fn a_disabled_timer_is_only_enabled_when_its_file_was_just_written() {
        let reload: &[&str] = &["daemon-reload"];
        let enable: &[&str] = &["enable", "--now", "pullday-check.timer"];
        assert!(
            timer_calls(false, false).is_empty(),
            "nothing changed: the user's choice stands"
        );
        assert_eq!(
            timer_calls(true, false),
            [reload],
            "the binary moved: reload only"
        );
        assert_eq!(timer_calls(false, true), [reload, enable], "first install");
        assert_eq!(timer_calls(true, true), [reload, enable]);
    }

    #[test]
    fn a_unit_is_written_only_when_it_differs() {
        use std::os::unix::fs::MetadataExt;
        let dir = temp_dir("units");
        let file = dir.join("systemd/user/pullday-check.timer");
        assert!(write_if_changed(&file, CHECK_TIMER).unwrap());
        let inode = fs::metadata(&file).unwrap().ino();
        assert!(!write_if_changed(&file, CHECK_TIMER).unwrap());
        assert_eq!(fs::metadata(&file).unwrap().ino(), inode, "not rewritten");
        assert!(write_if_changed(&file, "changed").unwrap());
        assert_eq!(fs::read_to_string(&file).unwrap(), "changed");
        let names: Vec<_> = fs::read_dir(file.parent().unwrap())
            .unwrap()
            .flatten()
            .map(|e| e.file_name())
            .collect();
        assert_eq!(names, ["pullday-check.timer"], "no temp file left");
        fs::remove_dir_all(&dir).unwrap();
    }

    /// One poll at `t0 + secs`; true when it started a rescan (`ok` is what starting it returns).
    fn poll(p: &mut Poller, dirs: &[String], t0: Instant, secs: u64, phase: u8, ok: bool) -> bool {
        let mut ran = false;
        p.poll(dirs, phase, t0 + Duration::from_secs(secs), || {
            ran = true;
            ok
        });
        ran
    }

    /// A folder's mtime has the kernel's coarse clock (a few ms), so a change right after a baseline could look like none.
    fn later() {
        std::thread::sleep(Duration::from_millis(50));
    }

    #[test]
    fn the_poller_rescans_for_an_entry_in_a_root_not_a_change_inside_a_repo() {
        use crate::worker::SCANNING;
        let dir = temp_dir("poll");
        let root = dir.join("skills");
        fs::create_dir_all(root.join("repo")).unwrap();
        let dirs = vec![root.to_string_lossy().into_owned()];
        let mut p = Poller::new(&dirs);
        let t0 = Instant::now();
        later();
        fs::write(root.join("repo/SKILL.md"), "x").unwrap();
        fs::create_dir(root.join("repo/sub")).unwrap();
        for secs in [5, 10, 15] {
            assert!(!poll(&mut p, &dirs, t0, secs, IDLE, true), "inside a repo");
        }

        fs::create_dir(root.join("new-skill")).unwrap();
        assert!(
            !poll(&mut p, &dirs, t0, 20, IDLE, true),
            "seen, but not quiet yet"
        );
        assert!(poll(&mut p, &dirs, t0, 25, IDLE, true), "5 s of quiet");
        assert!(!poll(&mut p, &dirs, t0, 30, IDLE, true), "once");

        later();
        fs::rename(root.join("new-skill"), root.join("renamed")).unwrap();
        assert!(!poll(&mut p, &dirs, t0, 35, IDLE, true));
        assert!(
            !poll(&mut p, &dirs, t0, 40, SCANNING, true),
            "waits for the running check"
        );
        assert!(
            poll(&mut p, &dirs, t0, 45, IDLE, false),
            "tried, couldn't start"
        );
        assert!(poll(&mut p, &dirs, t0, 50, IDLE, true), "tried again");
        assert!(!poll(&mut p, &dirs, t0, 55, IDLE, true));

        later();
        fs::remove_dir(root.join("renamed")).unwrap();
        assert!(!poll(&mut p, &dirs, t0, 60, UPDATING, true));
        for secs in [65, 70] {
            assert!(
                !poll(&mut p, &dirs, t0, secs, IDLE, true),
                "a change while updating"
            );
        }
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_poller_picks_up_a_root_created_later() {
        let dir = temp_dir("poll-later");
        fs::create_dir_all(&dir).unwrap();
        let root = dir.join(".agents/skills");
        let dirs = vec![root.to_string_lossy().into_owned()];
        let mut p = Poller::new(&dirs);
        let t0 = Instant::now();
        assert!(!poll(&mut p, &dirs, t0, 5, IDLE, true));
        fs::create_dir_all(&root).unwrap();
        assert!(!poll(&mut p, &dirs, t0, 10, IDLE, true));
        assert!(poll(&mut p, &dirs, t0, 15, IDLE, true), "the new root");
        later();
        fs::create_dir(root.join("skill")).unwrap();
        assert!(!poll(&mut p, &dirs, t0, 20, IDLE, true));
        assert!(poll(&mut p, &dirs, t0, 25, IDLE, true), "an entry in it");
        fs::remove_dir_all(&dir).unwrap();
    }
}
