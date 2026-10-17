//! Desktop integration: single instance (a lock, then a socket).

use std::ffi::OsString;
use std::fs::{self, DirBuilder, File, OpenOptions, TryLockError};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::fs::DirBuilderExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

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

/// Serves the socket for the life of the process: `on_show` for each exact `show` line, anything else ignored.
pub fn serve(instance: Instance, on_show: impl Fn()) {
    for stream in instance.listener.incoming().flatten() {
        // A client that never writes must not block the next one.
        let _ = stream.set_read_timeout(Some(Duration::from_secs(1)));
        // Only short command lines are expected; don't buffer whatever else a client sends.
        for line in BufReader::new(stream.take(64))
            .lines()
            .map_while(Result::ok)
        {
            if line == "show" {
                on_show();
            }
        }
    }
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
        std::thread::spawn(move || serve(instance, move || tx.send(()).unwrap()));
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
}
