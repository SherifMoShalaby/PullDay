//! Port of `Sources/Shell.swift`: runs commands through the user's login shell, so tools installed with
//! Homebrew, uv, pipx, npm, nvm etc. are found even though this is a GUI app.

use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex, Weak};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

extern "C" {
    fn kill(pid: i32, sig: i32) -> i32;
}
const SIGTERM: i32 = 15;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShellResult {
    pub status: i32,
    pub output: String,
    pub timed_out: bool,
}

impl ShellResult {
    pub fn ok(&self) -> bool {
        self.status == 0 && !self.timed_out
    }

    pub fn lines(&self) -> Vec<&str> {
        split_lines(&self.output)
    }

    pub fn last_line(&self, fallback: &str) -> String {
        if self.timed_out {
            return "Timed out".into();
        }
        self.lines()
            .last()
            .map_or(fallback.into(), |l| l.trim().into())
    }
}

/// Non-empty lines, split on every character Swift's `isNewline` matches, so a progress bar's lone `\r` splits too.
pub fn split_lines(text: &str) -> Vec<&str> {
    let newline = |c| {
        matches!(
            c,
            '\n' | '\r' | '\u{b}' | '\u{c}' | '\u{85}' | '\u{2028}' | '\u{2029}'
        )
    };
    text.split(newline).filter(|l| !l.is_empty()).collect()
}

pub fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| "/".into())
}

/// Folders appended to PATH. Appended, not prepended, so whatever the user's profile sets up wins.
pub fn extra_paths(home: &Path) -> Vec<String> {
    let h = home.display();
    let mut paths: Vec<String> = [
        format!("{h}/.local/bin"),
        format!("{h}/.claude/local"),
        "/opt/homebrew/bin".into(),
        "/opt/homebrew/sbin".into(),
        "/home/linuxbrew/.linuxbrew/bin".into(),
        "/usr/local/bin".into(),
        format!("{h}/.cargo/bin"),
        format!("{h}/.npm-global/bin"),
        format!("{h}/.bun/bin"),
        format!("{h}/.volta/bin"),
        "/usr/bin".into(),
        "/bin".into(),
        "/usr/sbin".into(),
        "/sbin".into(),
    ]
    .into();
    // Newest nvm-managed Node, since nvm is usually only loaded in the interactive rc file.
    let nvm = home.join(".nvm/versions/node");
    let newest = std::fs::read_dir(&nvm)
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .filter(|v| v.starts_with('v'))
        .max_by_key(|v| version_key(v));
    if let Some(v) = newest {
        paths.push(format!("{}/{v}/bin", nvm.display()));
    }
    paths
}

fn version_key(v: &str) -> Vec<u64> {
    v.trim_start_matches('v')
        .split('.')
        .map(|p| p.parse().unwrap_or(0))
        .collect()
}

/// `$SHELL` if it's a POSIX shell (`quote` is POSIX-only), else the platform's own POSIX shell.
pub fn login_shell_from(shell: Option<&str>) -> String {
    let posix = ["sh", "bash", "zsh", "dash", "ksh"];
    if let Some(shell) = shell.filter(|s| !s.is_empty()) {
        let name = Path::new(shell)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("");
        let executable = std::fs::metadata(shell)
            .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0);
        if posix.contains(&name) && executable {
            return shell.into();
        }
    }
    // zsh is often missing on Linux.
    if cfg!(target_os = "macos") {
        "/bin/zsh"
    } else {
        "/bin/sh"
    }
    .into()
}

pub fn login_shell() -> String {
    login_shell_from(std::env::var("SHELL").ok().as_deref())
}

/// Set by the Linux app when it forced `GDK_BACKEND=x11` on itself (XWayland), so the tools it starts don't get it.
pub static DROP_GDK_BACKEND: AtomicBool = AtomicBool::new(false);

/// The environment every child PullDay starts gets.
pub fn child_env(c: &mut Command) -> &mut Command {
    if DROP_GDK_BACKEND.load(Ordering::Relaxed) {
        c.env_remove("GDK_BACKEND");
    }
    c
}

pub fn run(command: &str, timeout: Duration) -> ShellResult {
    let shell = login_shell();
    let path = extra_paths(&home()).join(":");
    let (mut reader, writer) = match std::io::pipe() {
        Ok(p) => p,
        Err(e) => return failed_to_start(&shell, e),
    };
    let child = {
        // Built in this block so the Command, which holds the pipe's write ends, is dropped after the
        // spawn; otherwise end-of-file would never arrive.
        let mut c = Command::new(&shell);
        child_env(&mut c)
            .args([
                "-l",
                "-c",
                &format!("export PATH=\"$PATH:{path}\"; {command}"),
            ])
            .env("TERM", "dumb")
            .env("NO_COLOR", "1")
            .env("GIT_TERMINAL_PROMPT", "0") // never hang on a password prompt
            .env("GIT_SSH_COMMAND", "ssh -o BatchMode=yes")
            .env("PIP_DISABLE_PIP_VERSION_CHECK", "1")
            .env("npm_config_fund", "false")
            .env("npm_config_update_notifier", "false")
            .stdin(Stdio::null())
            // Its own process group, so the timeout's signal to -pid also reaches what the shell spawned.
            .process_group(0);
        match writer.try_clone() {
            Ok(w) => c.stdout(w).stderr(writer).spawn(),
            Err(e) => Err(e),
        }
    };
    let mut child = match child {
        Ok(c) => c,
        Err(e) => return failed_to_start(&shell, e),
    };
    let pid = child.id() as i32;

    // Read as output arrives rather than to end-of-file: a child the command left running in the background
    // (a post-update command starting a daemon, say) can hold the pipe open long after the shell exits.
    let data = Arc::new(Mutex::new(Vec::new()));
    let (eof_tx, eof_rx) = mpsc::channel();
    // Weak, so a reader left behind by a background child stops (and closes the pipe) once `run` returns.
    let sink: Weak<Mutex<Vec<u8>>> = Arc::downgrade(&data);
    thread::spawn(move || {
        let mut buf = [0u8; 65536];
        while let Ok(n @ 1..) = reader.read(&mut buf) {
            let Some(data) = sink.upgrade() else { break };
            data.lock().unwrap().extend_from_slice(&buf[..n]);
        }
        let _ = eof_tx.send(());
    });

    let (exit_tx, exit_rx) = mpsc::channel();
    thread::spawn(move || {
        let _ = exit_tx.send(child.wait());
    });
    let mut timed_out = false;
    let exit = match exit_rx.recv_timeout(timeout) {
        Ok(exit) => exit,
        Err(_) => {
            timed_out = true;
            unsafe { kill(-pid, SIGTERM) };
            exit_rx
                .recv()
                .unwrap_or_else(|e| Err(std::io::Error::other(e)))
        }
    };
    let status = match exit {
        Ok(s) => s.code().or(s.signal()).unwrap_or(-1),
        Err(_) => -1,
    };
    // End-of-file normally follows the exit at once; give up on it if a background child still holds the
    // pipe (the reader thread then ends whenever that child closes it).
    let _ = eof_rx.recv_timeout(Duration::from_secs(2));

    let output = String::from_utf8_lossy(&data.lock().unwrap())
        .trim()
        .to_string();
    ShellResult {
        status,
        output,
        timed_out,
    }
}

fn failed_to_start(shell: &str, e: std::io::Error) -> ShellResult {
    ShellResult {
        status: -1,
        output: format!("Couldn't start {shell}: {e}"),
        timed_out: false,
    }
}

/// Single-quotes a value for safe use inside a shell command.
pub fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

/// Collects the commands run for one update, for the log.
#[derive(Default)]
pub struct Transcript {
    parts: Vec<String>,
    pub last_failure: Option<String>,
}

impl Transcript {
    pub fn run(&mut self, command: &str, timeout: Duration) -> ShellResult {
        let result = run(command, timeout);
        let body = if result.output.is_empty() {
            "(no output)"
        } else {
            &result.output
        };
        let end = if result.timed_out {
            "timed out".into()
        } else {
            format!("exit {}", result.status)
        };
        self.parts.push(format!("$ {command}\n{body}\n→ {end}"));
        if !result.ok() {
            self.last_failure = Some(result.last_line(&format!("Command failed: {command}")));
        }
        result
    }

    pub fn note(&mut self, text: &str) {
        self.parts.push(text.into());
        self.last_failure = Some(text.into());
    }

    pub fn text(&self) -> String {
        self.parts.join("\n\n")
    }
}

/// The log file: `$XDG_STATE_HOME/pullday/pullday.log` (default `~/.local/state`) on Linux.
pub fn log_path() -> PathBuf {
    if cfg!(target_os = "macos") {
        return home().join("Library/Logs/PullDay.log");
    }
    let state = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| home().join(".local/state"));
    state.join("pullday/pullday.log")
}

pub fn log_append(path: &Path, date: SystemTime, title: &str, output: &str, success: bool) {
    let stamp = iso8601(date);
    let text = format!(
        "[{stamp}] {} {title}\n{output}\n\n",
        if success { "OK  " } else { "FAIL" }
    );
    // Keep the file small: start over once it passes 2 MB.
    if std::fs::metadata(path).is_ok_and(|m| m.len() > 2_000_000) {
        let _ = std::fs::remove_file(path);
    }
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = f.write_all(text.as_bytes());
    }
}

/// UTC in the form Swift's `ISO8601DateFormatter` writes: `2001-09-09T01:46:40Z`.
pub fn iso8601(date: SystemTime) -> String {
    let secs = date.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let (days, rem) = (secs / 86400, secs % 86400);
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    fn tmp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("pullday-shell-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_forced_gdk_backend_is_not_passed_on() {
        // Safe here: std's environment lock covers spawning, and no other test reads GDK_BACKEND.
        std::env::set_var("GDK_BACKEND", "x11");
        let seen = || {
            run(
                "printf %s \"${GDK_BACKEND-unset}\"",
                Duration::from_secs(30),
            )
            .last_line("")
        };
        assert_eq!(seen(), "x11", "the user's own choice is passed on");
        DROP_GDK_BACKEND.store(true, Ordering::Relaxed);
        assert_eq!(seen(), "unset");
        DROP_GDK_BACKEND.store(false, Ordering::Relaxed);
        std::env::remove_var("GDK_BACKEND");
    }

    #[test]
    fn a_non_posix_or_missing_shell_falls_back() {
        let fallback = if cfg!(target_os = "macos") {
            "/bin/zsh"
        } else {
            "/bin/sh"
        };
        assert_eq!(login_shell_from(Some("/bin/sh")), "/bin/sh");
        assert_eq!(login_shell_from(None), fallback);
        assert_eq!(login_shell_from(Some("")), fallback);
        assert_eq!(login_shell_from(Some("/no/such/bash")), fallback);
        // An executable that isn't a POSIX shell (fish, nu, …): its name decides.
        let dir = tmp("fish");
        let fish = dir.join("fish");
        std::fs::write(&fish, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&fish, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(login_shell_from(fish.to_str()), fallback);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn quoting_survives_the_shell() {
        let nasty = "it's $HOME `id` \"x\" ; rm -rf / \\ \n end";
        let r = run(
            &format!("printf %s {}", quote(nasty)),
            Duration::from_secs(30),
        );
        assert!(r.ok(), "{r:?}");
        assert_eq!(r.output, nasty.trim());
    }

    #[test]
    fn a_timeout_kills_the_whole_process_group() {
        let dir = tmp("timeout");
        let pidfile = dir.join("pid");
        let start = Instant::now();
        let r = run(
            &format!(
                "sleep 30 & echo $! > {}; wait",
                quote(pidfile.to_str().unwrap())
            ),
            // Long enough for a slow login profile (GitHub's runners) to finish before the sleep starts.
            Duration::from_secs(3),
        );
        assert!(
            r.timed_out && !r.ok() && r.last_line("") == "Timed out",
            "{r:?}"
        );
        assert!(
            start.elapsed() < Duration::from_secs(8),
            "took {:?}",
            start.elapsed()
        );
        let pid = std::fs::read_to_string(&pidfile)
            .unwrap()
            .trim()
            .to_string();
        // The background sleep got the signal too; give it a moment to be reaped.
        let gone = (0..20).any(|_| {
            thread::sleep(Duration::from_millis(100));
            !run(&format!("kill -0 {pid}"), Duration::from_secs(5)).ok()
        });
        assert!(gone, "sleep {pid} outlived the timeout");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_background_child_holding_the_pipe_does_not_block() {
        let start = Instant::now();
        let r = run("(sleep 20 &); echo started", Duration::from_secs(60));
        let took = start.elapsed();
        assert!(took < Duration::from_secs(5), "took {took:?}");
        assert!(r.ok() && r.output.contains("started"), "{r:?}");
    }

    #[test]
    fn a_left_behind_reader_lets_go_of_the_pipe() {
        // After `run` returns, the background child's next write ends the reader, which closes the pipe,
        // so the write after that fails (as under Swift) instead of being buffered forever. The first write comes
        // well after `run` gives up on end-of-file (2 s after the shell exits), so the two can't race.
        let dir = tmp("reader");
        let flag = dir.join("still-read");
        let cmd = format!(
            "(sleep 4; echo late; sleep 1; echo later && touch {}) & echo started",
            quote(flag.to_str().unwrap())
        );
        assert!(run(&cmd, Duration::from_secs(60))
            .output
            .contains("started"));
        thread::sleep(Duration::from_secs(6));
        assert!(
            !flag.exists(),
            "the pipe was still being read after run returned"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn lines_split_like_swift() {
        let r = ShellResult {
            status: 1,
            output: "#####\r100%\nerror: boom\rretrying \n\n".into(),
            timed_out: false,
        };
        assert_eq!(r.lines(), ["#####", "100%", "error: boom", "retrying "]);
        assert_eq!(r.last_line("none"), "retrying");
    }

    #[test]
    fn output_larger_than_the_pipe_buffer_arrives_whole() {
        let r = run(
            "head -c 300000 /dev/zero | tr '\\0' x",
            Duration::from_secs(60),
        );
        assert_eq!(r.output.len(), 300_000);
    }

    #[test]
    fn stderr_and_the_exit_status_are_kept() {
        let r = run("echo oops >&2; exit 3", Duration::from_secs(30));
        assert_eq!((r.status, r.output.as_str(), r.ok()), (3, "oops", false));
    }

    #[test]
    fn transcript_records_commands_and_the_last_failure() {
        let mut t = Transcript::default();
        t.run("echo one", Duration::from_secs(30));
        assert_eq!(t.last_failure, None);
        t.run("echo first; echo broke >&2; false", Duration::from_secs(30));
        assert_eq!(t.last_failure.as_deref(), Some("broke"));
        assert!(
            t.text()
                .contains("$ echo one\none\n→ exit 0\n\n$ echo first"),
            "{}",
            t.text()
        );
    }

    #[test]
    fn iso8601_matches_swift() {
        assert_eq!(iso8601(UNIX_EPOCH), "1970-01-01T00:00:00Z");
        assert_eq!(
            iso8601(UNIX_EPOCH + Duration::from_secs(1_000_000_000)),
            "2001-09-09T01:46:40Z"
        );
        assert_eq!(
            iso8601(UNIX_EPOCH + Duration::from_secs(1_709_208_000)),
            "2024-02-29T12:00:00Z"
        );
    }

    #[test]
    fn the_log_starts_over_past_2_mb() {
        let dir = tmp("log");
        let log = dir.join("state/pullday/pullday.log");
        log_append(&log, UNIX_EPOCH, "Check", "fine", true);
        assert_eq!(
            std::fs::read_to_string(&log).unwrap(),
            "[1970-01-01T00:00:00Z] OK   Check\nfine\n\n"
        );
        std::fs::write(&log, vec![b'x'; 2_000_001]).unwrap();
        log_append(&log, UNIX_EPOCH, "Update", "broke", false);
        assert_eq!(
            std::fs::read_to_string(&log).unwrap(),
            "[1970-01-01T00:00:00Z] FAIL Update\nbroke\n\n"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
}
