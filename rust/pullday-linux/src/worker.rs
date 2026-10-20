//! The one thread that owns `Core`: every job that touches it runs here, one at a time, never on the GTK thread.

use crate::desktop::Poller;
use crate::view;
use pullday_core::models::{Agent, SourceKind, UpdatableItem};
use pullday_core::prefs::Prefs;
use pullday_core::scanner::Scanner;
use pullday_core::shell::log_append;
use pullday_core::state::{scheduled_plan, Core, LogEntry, Notice};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::Path;
use std::sync::atomic::{AtomicU8, Ordering::SeqCst};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

pub const IDLE: u8 = 0;
pub const SCANNING: u8 = 1;
pub const UPDATING: u8 = 2;

pub enum Job {
    /// The Mac's `AppState.start()`: check, then the scheduled run if one was due.
    Startup,
    /// The 15-minute loop, the wake tick and the systemd timer: a scheduled run if due and nothing runs.
    RunIfDue,
    Refresh {
        record_run: bool,
    },
    Update(Vec<String>),
    UpdateAll,
    Skip(String),
    StopSkipping(String),
    SetIgnored(String, bool),
    /// A Settings control, already checked by `parse_setting`.
    SetSetting(Setting),
    ClearLog,
}

/// The Mac's five "Check for updates" choices, in seconds (0 = only when I ask).
pub const INTERVALS: [f64; 5] = [6.0 * 3600.0, 12.0 * 3600.0, 86400.0, 7.0 * 86400.0, 0.0];

/// What the commands read without waiting for the worker.
#[derive(Default)]
pub struct Cache {
    pub snapshot: Value,
    /// Item id → its folder, for Show in Files (the page only ever sends an id).
    pub paths: BTreeMap<String, String>,
    /// `Scanner::watched_dirs` for the sources that are on, for the folder poller.
    pub watched: Vec<String>,
}

pub struct Worker {
    tx: Sender<Job>,
    pub phase: Arc<AtomicU8>,
    pub cache: Arc<Mutex<Cache>>,
    /// Shared with the poll thread; the worker re-baselines it at the end of every update.
    pub poller: Arc<Mutex<Poller>>,
}

impl Worker {
    /// Starts the thread; `emit` gets each new snapshot and `notices` each notification.
    pub fn start(
        mut core: Core,
        emit: impl Fn(&Value) + Send + Sync + 'static,
        notices: Sender<Notice>,
    ) -> Worker {
        let (tx, rx) = channel();
        let phase = Arc::new(AtomicU8::new(IDLE));
        let cache = Arc::new(Mutex::new(Cache::default()));
        let home = core.scanner.home.clone();
        // The poller's folders, rebuilt from the prefs each publish (Core stays on the worker).
        let scanner = Arc::new(Scanner::new(home.clone()));
        let emit = Arc::new(emit);
        let publish = {
            let (phase, cache, emit) = (phase.clone(), cache.clone(), emit.clone());
            move |items: &[UpdatableItem], prefs: &Prefs, log: &[LogEntry]| {
                let detected: Vec<Agent> = Agent::ALL
                    .into_iter()
                    .filter(|a| a.is_detected(Path::new(&home)))
                    .collect();
                let snapshot =
                    view::snapshot(items, prefs, log, &detected, phase_name(phase.load(SeqCst)));
                let mut c = cache.lock().unwrap();
                c.paths = items
                    .iter()
                    .filter_map(|i| Some((i.id.clone(), i.path.clone()?)))
                    .collect();
                c.snapshot = snapshot.clone();
                c.watched = watched_dirs(&scanner, prefs);
                // Not under the lock: emit waits on the GTK thread, whose commands take the lock.
                drop(c);
                emit(&snapshot);
            }
        };
        publish(&core.items, &core.prefs, &core.log);
        core.on_change = Some(Box::new(publish.clone()));
        let poller = Arc::new(Mutex::new(Poller::new(&watched_dirs(
            &core.scanner,
            &core.prefs,
        ))));
        let (worker_phase, worker_poller) = (phase.clone(), poller.clone());
        std::thread::spawn(move || {
            for job in rx {
                for notice in run_job(&mut core, job, &worker_phase, &publish, &worker_poller) {
                    let _ = notices.send(notice);
                }
            }
        });
        Worker {
            tx,
            phase,
            cache,
            poller,
        }
    }

    /// Check now and the Update buttons are dropped while a check or update runs (the Mac's `guard !isBusy`);
    /// the rest queue behind it. True when the job was queued.
    pub fn submit(&self, mut job: Job) -> bool {
        let phase = match &mut job {
            Job::Startup | Job::Refresh { .. } => SCANNING,
            Job::Update(ids) => {
                // Each item once, in the order given (the page could send one twice).
                let mut seen = BTreeSet::new();
                ids.retain(|id| seen.insert(id.clone()));
                if ids.is_empty() {
                    return false;
                }
                UPDATING
            }
            // Nothing to update: no "Updating…" flash.
            Job::UpdateAll
                if self.cache.lock().unwrap().snapshot["updateAllIds"]
                    .as_array()
                    .is_none_or(Vec::is_empty) =>
            {
                return false
            }
            Job::UpdateAll => UPDATING,
            _ => return self.tx.send(job).is_ok(),
        };
        if self
            .phase
            .compare_exchange(IDLE, phase, SeqCst, SeqCst)
            .is_err()
        {
            return false;
        }
        self.tx.send(job).is_ok()
    }
}

/// Builds and sends a snapshot from Core's items, prefs and log.
type Publish<'a> = dyn Fn(&[UpdatableItem], &Prefs, &[LogEntry]) + 'a;

fn watched_dirs(scanner: &Scanner, prefs: &Prefs) -> Vec<String> {
    scanner.watched_dirs(&prefs.enabled_kinds())
}

/// The scheduled part of a run (`after_scheduled_check`), shown as updating when it will auto-update.
fn scheduled(core: &mut Core, phase: &AtomicU8) -> std::io::Result<Vec<Notice>> {
    let (auto, _) = scheduled_plan(&core.updates_available(), core.prefs.auto_update());
    if !auto.is_empty() {
        phase.store(UPDATING, SeqCst);
    }
    core.after_scheduled_check()
}

/// Runs one job and returns its notifications. A job that panics is logged and the worker goes on; the phase is
/// idle again either way.
fn run_job(
    core: &mut Core,
    job: Job,
    phase: &AtomicU8,
    publish: &Publish,
    poller: &Mutex<Poller>,
) -> Vec<Notice> {
    // RunIfDue waits for idle instead of being dropped by `submit`, so it claims the phase here (the Mac's
    // `guard isDue, !isBusy`); a run that isn't due, or finds a check queued, does nothing.
    if matches!(job, Job::RunIfDue)
        && !(core.is_due(SystemTime::now())
            && phase
                .compare_exchange(IDLE, SCANNING, SeqCst, SeqCst)
                .is_ok())
    {
        return Vec::new();
    }
    let busy = matches!(
        job,
        Job::Startup | Job::RunIfDue | Job::Refresh { .. } | Job::Update(_) | Job::UpdateAll
    );
    let ran = catch_unwind(AssertUnwindSafe(|| {
        if busy {
            publish(&core.items, &core.prefs, &core.log);
        }
        let none = |r: std::io::Result<()>| r.map(|()| Vec::new());
        match job {
            Job::Startup => {
                let due = core.is_due(SystemTime::now());
                core.refresh(true)?;
                if due {
                    scheduled(core, phase)
                } else {
                    Ok(Vec::new())
                }
            }
            Job::RunIfDue => {
                core.refresh(true)?;
                scheduled(core, phase)
            }
            Job::Refresh { record_run } => none(core.refresh(record_run)),
            Job::Update(ids) => core.update(&ids, false),
            Job::UpdateAll => {
                let ids: Vec<String> = core
                    .updates_available()
                    .iter()
                    .map(|i| i.id.clone())
                    .collect();
                core.update(&ids, false)
            }
            Job::Skip(id) => none(core.skip_update(&id)),
            Job::StopSkipping(id) => none(core.stop_skipping(&id)),
            Job::SetIgnored(id, on) => none(core.set_ignored(&id, on)),
            Job::SetSetting(setting) => none(apply_setting(core, setting)),
            Job::ClearLog => {
                core.clear_log();
                Ok(Vec::new())
            }
        }
    }));
    let mut notices = Vec::new();
    match ran {
        Ok(Ok(n)) => notices = n,
        Ok(Err(e)) => eprintln!("PullDay: couldn't save settings: {e}"),
        Err(panic) => {
            let why = panic
                .downcast_ref::<&str>()
                .map(|s| s.to_string())
                .or_else(|| panic.downcast_ref::<String>().cloned())
                .unwrap_or_default();
            eprintln!("PullDay: a job failed: {why}");
            log_append(
                &core.log_file,
                std::time::SystemTime::now(),
                "PullDay error",
                &why,
                false,
            );
        }
    }
    // Only the job `submit` set the phase for clears it: a Skip queued after a Check now must not.
    if busy {
        // Before idle, so the poller never sees the folders an update rewrote as a change.
        if phase.load(SeqCst) == UPDATING {
            poller
                .lock()
                .unwrap()
                .rebaseline(&watched_dirs(&core.scanner, &core.prefs));
        }
        phase.store(IDLE, SeqCst);
    }
    let _ = catch_unwind(AssertUnwindSafe(|| {
        publish(&core.items, &core.prefs, &core.log)
    }));
    notices
}

/// One Settings change, checked by `parse_setting`.
#[derive(Debug, PartialEq)]
pub enum Setting {
    CheckInterval(f64),
    AutoUpdate(bool),
    Notify(bool),
    /// One source on or off: a delta, so two toggles queued behind a check both land.
    Kind(SourceKind, bool),
    /// Item id, command; a blank command removes it.
    PostCommand(String, String),
}

/// The longest post-update command accepted, in bytes.
pub const MAX_COMMAND: usize = 4096;

/// The page is untrusted: anything but the keys and values Settings can send is rejected.
pub fn parse_setting(key: &str, value: &Value) -> Result<Setting, String> {
    let bad = || format!("rejected setting {key}");
    Ok(match key {
        "checkInterval" => Setting::CheckInterval(
            value
                .as_f64()
                .filter(|v| INTERVALS.contains(v))
                .ok_or_else(bad)?,
        ),
        "autoUpdate" => Setting::AutoUpdate(value.as_bool().ok_or_else(bad)?),
        "notify" => Setting::Notify(value.as_bool().ok_or_else(bad)?),
        "enabledKinds" => Setting::Kind(
            value["id"]
                .as_str()
                .and_then(SourceKind::from_id)
                .ok_or_else(bad)?,
            value["on"].as_bool().ok_or_else(bad)?,
        ),
        "postCommand" => {
            let (Some(id), Some(command)) = (value["id"].as_str(), value["command"].as_str())
            else {
                return Err(bad());
            };
            if command.len() > MAX_COMMAND {
                return Err(format!(
                    "A post-update command can be at most {MAX_COMMAND} bytes"
                ));
            }
            Setting::PostCommand(id.into(), command.into())
        }
        _ => return Err(bad()),
    })
}

/// Saves one Settings change; a post-update command only for an item PullDay has.
pub fn apply_setting(core: &mut Core, setting: Setting) -> std::io::Result<()> {
    let prefs = &mut core.prefs;
    match setting {
        Setting::CheckInterval(seconds) => prefs.set_check_interval(seconds),
        Setting::AutoUpdate(on) => prefs.set_auto_update(on),
        Setting::Notify(on) => prefs.set_notify(on),
        Setting::Kind(kind, on) => {
            let mut kinds = prefs.enabled_kinds();
            if on {
                kinds.insert(kind.id().into());
            } else {
                kinds.remove(kind.id());
            }
            prefs.set_enabled_kinds(&kinds)
        }
        Setting::PostCommand(id, command) => {
            if !core.items.iter().any(|i| i.id == id) {
                return Err(std::io::Error::other(format!("no item {id}")));
            }
            let mut commands = prefs.post_commands();
            if command.trim().is_empty() {
                commands.remove(&id);
            } else {
                commands.insert(id, command);
            }
            prefs.set_post_commands(&commands)
        }
    }
}

fn phase_name(phase: u8) -> &'static str {
    match phase {
        SCANNING => "scanning",
        UPDATING => "updating",
        _ => "idle",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_and_update_are_dropped_while_busy_and_the_rest_queue() {
        let (tx, rx) = channel();
        let w = Worker {
            tx,
            phase: Arc::new(AtomicU8::new(IDLE)),
            cache: Default::default(),
            poller: Arc::new(Mutex::new(Poller::new(&[]))),
        };
        assert!(!w.submit(Job::UpdateAll), "nothing available");
        w.cache.lock().unwrap().snapshot = serde_json::json!({"updateAllIds": ["uv:x"]});
        assert!(w.submit(Job::Refresh { record_run: true }));
        assert_eq!(
            w.phase.load(SeqCst),
            SCANNING,
            "busy as soon as it's queued, so a double click can't queue two"
        );
        assert!(!w.submit(Job::Refresh { record_run: true }));
        assert!(!w.submit(Job::Update(vec!["uv:x".into()])));
        assert!(!w.submit(Job::UpdateAll));
        assert!(w.submit(Job::Skip("uv:x".into())));
        assert!(w.submit(Job::StopSkipping("uv:x".into())));
        assert!(w.submit(Job::SetIgnored("uv:x".into(), true)));
        assert!(w.submit(Job::RunIfDue), "waits for idle instead");
        assert_eq!(rx.try_iter().count(), 5);

        w.phase.store(IDLE, SeqCst);
        assert!(!w.submit(Job::Update(vec![])), "nothing to update");
        assert_eq!(w.phase.load(SeqCst), IDLE);
        assert!(w.submit(Job::Update(vec!["a".into(), "b".into(), "a".into()])));
        assert!(
            matches!(rx.try_recv(), Ok(Job::Update(ids)) if ids == ["a", "b"]),
            "each id once"
        );
        w.phase.store(IDLE, SeqCst);
        assert!(w.submit(Job::UpdateAll));
        assert_eq!(w.phase.load(SeqCst), UPDATING);
        assert!(!w.submit(Job::Refresh { record_run: false }));
        assert_eq!(rx.try_iter().count(), 1);
    }

    fn no_poller() -> Mutex<Poller> {
        Mutex::new(Poller::new(&[]))
    }

    /// A home with nothing in it and only Gemini extensions on, so a check runs no tool.
    fn core(name: &str) -> (Core, std::path::PathBuf) {
        let dir =
            std::env::temp_dir().join(format!("pullday-worker-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("home")).unwrap();
        let mut prefs = Prefs::load(&dir.join("config.json"));
        prefs
            .set_enabled_kinds(&["geminiExtension".to_string()].into())
            .unwrap();
        let scanner = Scanner::new(dir.join("home").to_string_lossy());
        (Core::new(scanner, prefs, dir.join("pullday.log")), dir)
    }

    #[test]
    fn a_skip_queued_behind_a_check_leaves_the_check_busy() {
        let (mut c, dir) = core("skip");
        let phase = AtomicU8::new(SCANNING);
        run_job(
            &mut c,
            Job::Skip("uv:x".into()),
            &phase,
            &|_, _, _| {},
            &no_poller(),
        );
        assert_eq!(phase.load(SeqCst), SCANNING);
        run_job(
            &mut c,
            Job::Refresh { record_run: false },
            &phase,
            &|_, _, _| {},
            &no_poller(),
        );
        assert_eq!(phase.load(SeqCst), IDLE);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_job_that_panics_leaves_the_worker_idle_and_working() {
        let (mut c, dir) = core("panic");
        let phase = AtomicU8::new(SCANNING);
        let boom = |_: &[UpdatableItem], _: &Prefs, _: &[LogEntry]| panic!("boom");
        run_job(
            &mut c,
            Job::Refresh { record_run: false },
            &phase,
            &boom,
            &no_poller(),
        );
        assert_eq!(phase.load(SeqCst), IDLE);
        assert!(std::fs::read_to_string(dir.join("pullday.log"))
            .unwrap()
            .contains("boom"));
        phase.store(SCANNING, SeqCst);
        let ran = std::cell::Cell::new(false);
        run_job(
            &mut c,
            Job::Refresh { record_run: false },
            &phase,
            &|_, _, _| ran.set(true),
            &no_poller(),
        );
        assert!(ran.get() && phase.load(SeqCst) == IDLE, "the next job runs");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn set_setting_saves_what_settings_sends_and_rejects_the_rest() {
        use serde_json::json;
        let (mut c, dir) = core("setting");
        c.items = vec![UpdatableItem::new(SourceKind::Uv, "tool", "1.0", None)];
        let ok = |c: &mut Core, k: &str, v: Value| {
            parse_setting(k, &v).is_ok_and(|s| apply_setting(c, s).is_ok())
        };
        let saved = || Prefs::load(&dir.join("config.json"));

        for seconds in INTERVALS {
            assert!(ok(&mut c, "checkInterval", json!(seconds)), "{seconds}");
            assert_eq!(saved().check_interval(), seconds);
        }
        for bad in [json!(3600), json!(-1), json!("86400"), json!(null)] {
            assert!(!ok(&mut c, "checkInterval", bad.clone()), "{bad}");
        }
        assert_eq!(
            saved().check_interval(),
            0.0,
            "a rejected value changes nothing"
        );

        assert!(ok(&mut c, "autoUpdate", json!(true)) && saved().auto_update());
        assert!(ok(&mut c, "notify", json!(false)) && !saved().notify());
        assert!(!ok(&mut c, "notify", json!("no")));

        // Two toggles in a row (queued behind a check) both land.
        let kind = |id: &str, on| json!({"id": id, "on": on});
        assert!(ok(&mut c, "enabledKinds", kind("npm", true)));
        assert!(ok(&mut c, "enabledKinds", kind("pipx", true)));
        assert!(ok(&mut c, "enabledKinds", kind("geminiExtension", false)));
        assert_eq!(
            saved().enabled_kinds(),
            ["npm".to_string(), "pipx".into()].into()
        );
        assert!(!ok(&mut c, "enabledKinds", kind("bogus", true)));
        assert!(!ok(
            &mut c,
            "enabledKinds",
            json!({"id": "uv", "on": "yes"})
        ));
        assert!(
            !ok(&mut c, "enabledKinds", json!(["uv"])),
            "not a whole set"
        );
        assert_eq!(saved().enabled_kinds().len(), 2);

        let cmd = |id: &str, command: &str| json!({"id": id, "command": command});
        assert!(ok(&mut c, "postCommand", cmd("uv:tool", "make install")));
        assert_eq!(
            saved().post_commands().get("uv:tool").map(String::as_str),
            Some("make install")
        );
        assert!(ok(&mut c, "postCommand", cmd("uv:tool", "  \t ")));
        assert!(
            !saved().post_commands().contains_key("uv:tool"),
            "blank removes it"
        );
        assert!(
            !ok(&mut c, "postCommand", cmd("uv:missing", "rm -rf ~")),
            "not an item"
        );
        assert!(!saved().post_commands().contains_key("uv:missing"));
        assert!(!ok(&mut c, "postCommand", json!({"id": "uv:tool"})));
        assert!(ok(
            &mut c,
            "postCommand",
            cmd("uv:tool", &"x".repeat(MAX_COMMAND))
        ));
        assert!(
            parse_setting("postCommand", &cmd("uv:tool", &"x".repeat(MAX_COMMAND + 1))).is_err(),
            "too long"
        );
        assert_eq!(saved().post_commands()["uv:tool"].len(), MAX_COMMAND);

        assert!(!ok(&mut c, "lastRun", json!(0)), "not a Settings key");
        assert!(!ok(&mut c, "ignored", json!(["uv:tool"])));
        let _ = std::fs::remove_dir_all(dir);
    }
    /// One local Gemini extension one version behind its source folder: an update offer with no network.
    fn behind(c: &Core, dir: &Path) {
        let ext = Path::new(&c.scanner.home).join(".gemini/extensions/ext");
        std::fs::create_dir_all(&ext).unwrap();
        let source = dir.join("source");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(
            ext.join("gemini-extension.json"),
            r#"{"name":"ext","version":"1.0.0"}"#,
        )
        .unwrap();
        std::fs::write(
            source.join("gemini-extension.json"),
            r#"{"name":"ext","version":"1.1.0"}"#,
        )
        .unwrap();
        let record = serde_json::json!({"type": "local", "source": source});
        std::fs::write(
            ext.join(".gemini-extension-install.json"),
            record.to_string(),
        )
        .unwrap();
    }

    #[test]
    fn startup_runs_the_scheduled_part_only_when_due() {
        let (mut c, dir) = core("startup-due");
        behind(&c, &dir);
        let phase = AtomicU8::new(SCANNING);
        let notices = run_job(&mut c, Job::Startup, &phase, &|_, _, _| {}, &no_poller());
        assert_eq!(
            notices,
            [Notice {
                title: "1 update available".into(),
                body: "ext".into()
            }],
            "never run, so due"
        );
        assert!(c.prefs.last_run().is_some(), "Startup records the run");
        assert_eq!(phase.load(SeqCst), IDLE);

        phase.store(SCANNING, SeqCst);
        let notices = run_job(&mut c, Job::Startup, &phase, &|_, _, _| {}, &no_poller());
        assert!(notices.is_empty(), "just checked: not due");
        assert!(
            c.items[0].status.is_update_available(),
            "but it still checks"
        );

        assert!(run_job(&mut c, Job::RunIfDue, &phase, &|_, _, _| {}, &no_poller()).is_empty());
        c.prefs.set_last_run(SystemTime::UNIX_EPOCH).unwrap();
        phase.store(SCANNING, SeqCst);
        assert!(
            run_job(&mut c, Job::RunIfDue, &phase, &|_, _, _| {}, &no_poller()).is_empty(),
            "a check is running or queued"
        );
        phase.store(IDLE, SeqCst);
        assert_eq!(
            run_job(&mut c, Job::RunIfDue, &phase, &|_, _, _| {}, &no_poller()).len(),
            1
        );
        assert_eq!(phase.load(SeqCst), IDLE);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_change_made_during_an_update_never_rescans_after_it() {
        let (mut c, dir) = core("rebaseline");
        let root = Path::new(&c.scanner.home).join(".claude/skills");
        std::fs::create_dir_all(&root).unwrap();
        let dirs = watched_dirs(&c.scanner, &c.prefs);
        assert!(dirs.contains(&root.to_string_lossy().into_owned()));
        let poller = Mutex::new(Poller::new(&dirs));
        let t0 = std::time::Instant::now();
        let poll = |secs| {
            let mut ran = false;
            poller.lock().unwrap().poll(
                &dirs,
                IDLE,
                t0 + std::time::Duration::from_secs(secs),
                || {
                    ran = true;
                    true
                },
            );
            ran
        };
        assert!(!poll(5));
        std::thread::sleep(std::time::Duration::from_millis(50));
        let phase = AtomicU8::new(UPDATING);
        // What a plugin update does to its folder, made while the update job runs.
        std::fs::create_dir(root.join("rewritten")).unwrap();
        run_job(
            &mut c,
            Job::Update(vec!["uv:x".into()]),
            &phase,
            &|_, _, _| {},
            &poller,
        );
        assert_eq!(phase.load(SeqCst), IDLE);
        assert!(!poll(10) && !poll(15) && !poll(20), "the Updated rows stay");

        std::thread::sleep(std::time::Duration::from_millis(50));
        std::fs::create_dir(root.join("added later")).unwrap();
        assert!(!poll(25));
        assert!(poll(30), "a change after the update still counts");
        let _ = std::fs::remove_dir_all(dir);
    }
}
