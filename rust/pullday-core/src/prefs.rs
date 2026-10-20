//! Port of `Sources/Prefs.swift`: the settings, kept in `$XDG_CONFIG_HOME/pullday/config.json` (default
//! `~/.config/pullday`) under the same keys the Mac app uses in UserDefaults. Like UserDefaults, a missing key reads
//! as its default and defaults are never written, so a changed default reaches everyone who never touched it.

use crate::models::SourceKind;
use crate::tools::default_post_commands;
use serde_json::{Map, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const INTERVAL: &str = "checkInterval";
const AUTO_UPDATE: &str = "autoUpdate";
const NOTIFY: &str = "notify";
const KINDS: &str = "enabledKinds";
const KNOWN_KINDS: &str = "knownKinds";
const IGNORED: &str = "ignored";
const SKIPPED: &str = "skippedUpdates";
const POST_COMMANDS: &str = "postCommands";
const LAST_RUN: &str = "lastRun";
/// Linux only: the PullDay release the new-version notification last went out for.
const APP_UPDATE_NOTIFIED: &str = "appUpdateNotified";

/// One writer at a time in this process (the worker's `Prefs` and the release check's): they share the temp file
/// name, and an unlocked read-merge-rename can lose a key or fail to rename.
static WRITE: Mutex<()> = Mutex::new(());

/// The kinds shipped before `knownKinds` existed. Never all kinds: then nothing would ever look new.
pub const LEGACY_KINDS: [&str; 6] = ["plugin", "gitSkill", "uv", "pipx", "npm", "claudeCode"];

pub struct Prefs {
    path: PathBuf,
    values: Map<String, Value>,
}

pub fn default_path() -> PathBuf {
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| crate::shell::home().join(".config"));
    config.join("pullday/config.json")
}

impl Prefs {
    /// Reads the settings (a missing or unreadable file is a fresh install) and runs the kinds migration.
    /// Never fails, like UserDefaults: settings that can't be written (a read-only ~/.config) stay in memory.
    pub fn load(path: &Path) -> Prefs {
        let values = match std::fs::read(path) {
            Ok(bytes) => match serde_json::from_slice(&bytes) {
                Ok(Value::Object(map)) => map,
                // A broken file is set aside rather than overwritten, so the user's settings can be recovered.
                _ => {
                    let _ = std::fs::rename(path, broken_path(path));
                    Map::new()
                }
            },
            Err(_) => Map::new(),
        };
        let mut prefs = Prefs {
            path: path.into(),
            values,
        };
        let _ = prefs.migrate_kinds();
        prefs
    }

    /// Turn on kinds added since the last launch once, so a user who changed their sources still gets
    /// them; a kind they later turn off stays off.
    fn migrate_kinds(&mut self) -> io::Result<()> {
        let known = self
            .strings(KNOWN_KINDS)
            .unwrap_or_else(|| LEGACY_KINDS.map(String::from).into());
        let added: Vec<String> = SourceKind::ALL
            .into_iter()
            .filter(|k| k.enabled_by_default() && !known.contains(k.id()))
            .map(|k| k.id().to_string())
            .collect();
        if !added.is_empty() {
            let mut kinds = self.enabled_kinds();
            kinds.extend(added);
            self.set(KINDS, set_value(&kinds))?;
        }
        let all: BTreeSet<String> = SourceKind::ALL.iter().map(|k| k.id().to_string()).collect();
        if self.strings(KNOWN_KINDS).as_ref() == Some(&all) {
            return Ok(()); // already current: loading writes nothing
        }
        self.set(KNOWN_KINDS, set_value(&all))
    }

    /// Seconds between automatic checks; 0 means manual only.
    pub fn check_interval(&self) -> f64 {
        self.values
            .get(INTERVAL)
            .and_then(Value::as_f64)
            .unwrap_or(24.0 * 3600.0)
    }

    /// New installs only check and notify until the user opts in.
    pub fn auto_update(&self) -> bool {
        self.values
            .get(AUTO_UPDATE)
            .and_then(Value::as_bool)
            .unwrap_or(false)
    }

    pub fn notify(&self) -> bool {
        self.values
            .get(NOTIFY)
            .and_then(Value::as_bool)
            .unwrap_or(true)
    }

    pub fn enabled_kinds(&self) -> BTreeSet<String> {
        self.strings(KINDS).unwrap_or_else(|| {
            SourceKind::ALL
                .iter()
                .filter(|k| k.enabled_by_default())
                .map(|k| k.id().to_string())
                .collect()
        })
    }

    pub fn ignored(&self) -> BTreeSet<String> {
        self.strings(IGNORED).unwrap_or_default()
    }

    /// Item id → the update offer the user skipped.
    pub fn skipped_updates(&self) -> BTreeMap<String, String> {
        self.string_map(SKIPPED).unwrap_or_default()
    }

    /// Item id → a shell command run after a successful update. graphify's `uv tool upgrade` updates the CLI;
    /// `graphify install` re-copies the updated skill files into ~/.claude/skills.
    pub fn post_commands(&self) -> BTreeMap<String, String> {
        self.string_map(POST_COMMANDS)
            .unwrap_or_else(|| default_post_commands().into_iter().collect())
    }

    pub fn last_run(&self) -> Option<SystemTime> {
        let seconds = self.values.get(LAST_RUN)?.as_f64()?;
        // Checked: a hand-edited 1e20 must not crash every launch.
        UNIX_EPOCH.checked_add(Duration::try_from_secs_f64(seconds).ok()?)
    }

    pub fn app_update_notified(&self) -> Option<String> {
        self.values
            .get(APP_UPDATE_NOTIFIED)?
            .as_str()
            .map(String::from)
    }

    pub fn set_app_update_notified(&mut self, version: &str) -> io::Result<()> {
        self.set(APP_UPDATE_NOTIFIED, version.into())
    }

    pub fn set_check_interval(&mut self, seconds: f64) -> io::Result<()> {
        self.set(INTERVAL, seconds.into())
    }

    pub fn set_auto_update(&mut self, on: bool) -> io::Result<()> {
        self.set(AUTO_UPDATE, on.into())
    }

    pub fn set_notify(&mut self, on: bool) -> io::Result<()> {
        self.set(NOTIFY, on.into())
    }

    pub fn set_enabled_kinds(&mut self, kinds: &BTreeSet<String>) -> io::Result<()> {
        self.set(KINDS, set_value(kinds))
    }

    pub fn set_ignored(&mut self, ids: &BTreeSet<String>) -> io::Result<()> {
        self.set(IGNORED, set_value(ids))
    }

    pub fn set_skipped_updates(&mut self, skipped: &BTreeMap<String, String>) -> io::Result<()> {
        self.set(SKIPPED, map_value(skipped))
    }

    pub fn set_post_commands(&mut self, commands: &BTreeMap<String, String>) -> io::Result<()> {
        self.set(POST_COMMANDS, map_value(commands))
    }

    pub fn set_last_run(&mut self, at: SystemTime) -> io::Result<()> {
        let seconds = at
            .duration_since(UNIX_EPOCH)
            .map_or(0.0, |d| d.as_secs_f64());
        self.set(LAST_RUN, seconds.into())
    }

    /// Merged into the file as it is now, as UserDefaults does per key, so a change another process (the app, or
    /// the scan example) made to another key since this one loaded isn't lost.
    fn set(&mut self, key: &str, value: Value) -> io::Result<()> {
        let _one_writer = WRITE.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(Ok(Value::Object(disk))) = std::fs::read(&self.path)
            .ok()
            .map(|b| serde_json::from_slice(&b))
        {
            self.values = disk;
        }
        self.values.insert(key.into(), value);
        self.save()
    }

    /// Written to a temporary file (one per process) and renamed over the old one, so a crash or a second
    /// process mid-write never leaves half a file.
    fn save(&self) -> io::Result<()> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let temp = self
            .path
            .with_extension(format!("json.tmp.{}", std::process::id()));
        let text = serde_json::to_string_pretty(&Value::Object(self.values.clone()))?;
        std::fs::write(&temp, text + "\n")?;
        std::fs::rename(&temp, &self.path)
    }

    fn strings(&self, key: &str) -> Option<BTreeSet<String>> {
        let list = self.values.get(key)?.as_array()?;
        Some(
            list.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect(),
        )
    }

    fn string_map(&self, key: &str) -> Option<BTreeMap<String, String>> {
        let map = self.values.get(key)?.as_object()?;
        Some(
            map.iter()
                .filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_string())))
                .collect(),
        )
    }
}

/// `config.json.broken`, or a timestamped name if an earlier broken copy is still there.
fn broken_path(path: &Path) -> PathBuf {
    let first = path.with_extension("json.broken");
    if !first.exists() {
        return first;
    }
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    path.with_extension(format!("json.broken.{now}"))
}

fn set_value(set: &BTreeSet<String>) -> Value {
    set.iter().cloned().collect::<Vec<_>>().into()
}

fn map_value(map: &BTreeMap<String, String>) -> Value {
    Value::Object(
        map.iter()
            .map(|(k, v)| (k.clone(), v.clone().into()))
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Tmp(PathBuf);
    impl Tmp {
        fn new(name: &str) -> Tmp {
            let dir =
                std::env::temp_dir().join(format!("pullday-prefs-{}-{name}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            Tmp(dir)
        }
        fn config(&self) -> PathBuf {
            self.0.join("pullday/config.json")
        }
        fn write(&self, json: &str) {
            std::fs::create_dir_all(self.0.join("pullday")).unwrap();
            std::fs::write(self.config(), json).unwrap();
        }
    }
    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn a_fresh_install_gets_the_defaults() {
        let t = Tmp::new("fresh");
        let p = Prefs::load(&t.config());
        assert_eq!(
            (p.check_interval(), p.auto_update(), p.notify()),
            (86400.0, false, true)
        );
        let all_but_npm: BTreeSet<String> = SourceKind::ALL
            .iter()
            .filter(|k| **k != SourceKind::Npm)
            .map(|k| k.id().into())
            .collect();
        assert_eq!(p.enabled_kinds(), all_but_npm);
        assert_eq!(p.post_commands()["uv:graphifyy"], "graphify install");
        assert_eq!(
            (p.ignored().len(), p.skipped_updates().len(), p.last_run()),
            (0, 0, None)
        );
        // As in Swift, the migration writes the kinds (a fresh install looks like a legacy one); the rest stay defaults.
        let saved: Value = serde_json::from_slice(&std::fs::read(t.config()).unwrap()).unwrap();
        assert_eq!(
            saved.as_object().unwrap().keys().collect::<Vec<_>>(),
            ["enabledKinds", "knownKinds"]
        );
    }

    /// New default-on kinds turn on once; one the user turns off stays off; npm stays off.
    #[test]
    fn migration() {
        let t = Tmp::new("migration");
        t.write(r#"{"enabledKinds": ["plugin"], "knownKinds": ["plugin"]}"#);
        let mut first = Prefs::load(&t.config());
        let kinds = first.enabled_kinds();
        assert!(
            kinds.contains("uv") && kinds.contains("codexCLI"),
            "new kinds are turned on"
        );
        assert!(!kinds.contains("npm"), "npm stays off");
        let without_uv: BTreeSet<String> = kinds.into_iter().filter(|k| k != "uv").collect();
        first.set_enabled_kinds(&without_uv).unwrap();
        assert!(
            !Prefs::load(&t.config()).enabled_kinds().contains("uv"),
            "a kind turned off stays off"
        );

        let legacy = Tmp::new("legacy");
        legacy.write(r#"{"enabledKinds": ["plugin"]}"#); // no knownKinds: shipped before the migration existed
        let kinds = Prefs::load(&legacy.config()).enabled_kinds();
        assert!(
            !kinds.contains("uv") && kinds.contains("codexPlugin"),
            "legacy kinds untouched, newer kinds added"
        );
        assert_eq!(
            Prefs::load(&legacy.config()).enabled_kinds(),
            kinds,
            "the migration runs once"
        );
    }

    #[test]
    fn settings_survive_a_reload() {
        let t = Tmp::new("roundtrip");
        let mut p = Prefs::load(&t.config());
        p.set_check_interval(3600.0).unwrap();
        p.set_auto_update(true).unwrap();
        p.set_notify(false).unwrap();
        p.set_ignored(&["npm:x".to_string()].into()).unwrap();
        p.set_skipped_updates(&[("uv:a".to_string(), "v1 → v2".to_string())].into())
            .unwrap();
        p.set_post_commands(&BTreeMap::new()).unwrap();
        let at = UNIX_EPOCH + Duration::from_secs(1_800_000_000);
        p.set_last_run(at).unwrap();
        let q = Prefs::load(&t.config());
        assert_eq!(
            (q.check_interval(), q.auto_update(), q.notify()),
            (3600.0, true, false)
        );
        assert!(q.ignored().contains("npm:x") && q.skipped_updates()["uv:a"] == "v1 → v2");
        assert!(
            q.post_commands().is_empty(),
            "removing the graphify default sticks"
        );
        assert_eq!(q.last_run(), Some(at));
        assert!(!t
            .config()
            .with_extension(format!("json.tmp.{}", std::process::id()))
            .exists());
    }

    #[test]
    fn two_processes_keep_each_others_changes() {
        let t = Tmp::new("merge");
        let mut app = Prefs::load(&t.config());
        let mut example = Prefs::load(&t.config());
        app.set_notify(false).unwrap();
        example.set_auto_update(true).unwrap();
        let both = Prefs::load(&t.config());
        assert!(!both.notify() && both.auto_update());
    }

    #[test]
    fn the_notified_release_is_merged_with_the_workers_writes() {
        let t = Tmp::new("notified");
        let mut worker = Prefs::load(&t.config());
        let mut notice = Prefs::load(&t.config());
        assert_eq!(notice.app_update_notified(), None);
        worker.set_auto_update(true).unwrap();
        notice.set_app_update_notified("1.0.3").unwrap();
        worker.set_notify(false).unwrap();
        let all = Prefs::load(&t.config());
        assert!(all.auto_update() && !all.notify());
        assert_eq!(all.app_update_notified().as_deref(), Some("1.0.3"));
    }

    #[test]
    fn two_writers_in_one_process_lose_nothing() {
        let t = Tmp::new("writers");
        let writer = |prefix: &'static str| {
            let path = t.config();
            std::thread::spawn(move || {
                let mut p = Prefs::load(&path);
                (0..300).try_for_each(|i| p.set(&format!("{prefix}{i}"), i.into()))
            })
        };
        let (a, b) = (writer("a"), writer("b"));
        a.join().unwrap().unwrap();
        b.join().unwrap().unwrap();
        let all = Prefs::load(&t.config());
        let missing: Vec<String> = ["a", "b"]
            .iter()
            .flat_map(|p| (0..300).map(move |i| format!("{p}{i}")))
            .filter(|k| !all.values.contains_key(k))
            .collect();
        assert!(missing.is_empty(), "lost {missing:?}");
    }

    #[test]
    fn odd_files_never_stop_a_launch() {
        let t = Tmp::new("odd");
        t.write(r#"{"lastRun": 1e20}"#);
        assert_eq!(
            Prefs::load(&t.config()).last_run(),
            None,
            "an absurd lastRun is no last run"
        );
        t.write(r#"{"lastRun": -5}"#);
        assert_eq!(Prefs::load(&t.config()).last_run(), None);
        // A config path that can't be written (its folder is a file): the defaults still load.
        std::fs::write(t.0.join("file"), "").unwrap();
        let p = Prefs::load(&t.0.join("file/pullday/config.json"));
        assert!(p.enabled_kinds().contains("uv") && !p.auto_update());
    }

    #[test]
    fn a_broken_file_is_set_aside() {
        let t = Tmp::new("broken");
        t.write("{ not json");
        let p = Prefs::load(&t.config());
        assert!(!p.auto_update());
        assert_eq!(
            std::fs::read_to_string(t.config().with_extension("json.broken")).unwrap(),
            "{ not json"
        );
        t.write("{ broken again");
        Prefs::load(&t.config());
        assert_eq!(
            std::fs::read_to_string(t.config().with_extension("json.broken")).unwrap(),
            "{ not json",
            "the first broken copy is kept"
        );
        let copies = std::fs::read_dir(t.0.join("pullday"))
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with("config.json.broken.")
            })
            .count();
        assert_eq!(copies, 1);
    }
}
