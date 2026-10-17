//! Port of the logic in `Sources/AppState.swift` and the menu's grouping, without UI types: checking, the schedule,
//! updating, skipping and the log. Notifications come back as values for the app to show. Timers, wake events and
//! folder watching are the app's job; it calls `run_if_due` from its timer.

use crate::models::{Agent, ItemStatus, SourceKind, UpdatableItem};
use crate::prefs::Prefs;
use crate::scanner::Scanner;
use crate::shell::log_append;
use std::collections::BTreeSet;
use std::io;
use std::path::PathBuf;
use std::time::SystemTime;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notice {
    pub title: String,
    pub body: String,
}

#[derive(Clone, Debug)]
pub struct LogEntry {
    pub date: SystemTime,
    pub title: String,
    pub output: String,
    pub success: bool,
}

/// One kind of item under one agent's section; `agent` None is the "Other" section.
#[derive(Clone, Debug, PartialEq)]
pub struct ItemGroup {
    pub agent: Option<Agent>,
    pub kind: SourceKind,
    pub items: Vec<UpdatableItem>,
    /// The first group of its agent's section, which carries the section header.
    pub starts_section: bool,
}

impl ItemGroup {
    pub fn id(&self) -> String {
        format!(
            "{}|{}",
            self.agent.map_or("other", |a| a.id()),
            self.kind.id()
        )
    }
}

pub struct Core {
    pub scanner: Scanner,
    pub prefs: Prefs,
    pub items: Vec<UpdatableItem>,
    /// Newest first, at most 200.
    pub log: Vec<LogEntry>,
    pub log_file: PathBuf,
}

impl Core {
    pub fn new(scanner: Scanner, prefs: Prefs, log_file: PathBuf) -> Core {
        Core {
            scanner,
            prefs,
            items: Vec::new(),
            log: Vec::new(),
            log_file,
        }
    }

    pub fn visible_items(&self) -> Vec<&UpdatableItem> {
        let ignored = self.prefs.ignored();
        self.items
            .iter()
            .filter(|i| !ignored.contains(&i.id))
            .collect()
    }

    /// In list order, which is also the order "Update all" runs them in.
    pub fn updates_available(&self) -> Vec<&UpdatableItem> {
        self.visible_items()
            .into_iter()
            .filter(|i| i.status.is_update_available())
            .collect()
    }

    // Scheduling

    pub fn is_due(&self, now: SystemTime) -> bool {
        let interval = self.prefs.check_interval();
        if interval <= 0.0 {
            return false;
        }
        match self.prefs.last_run() {
            None => true,
            Some(last) => now.duration_since(last).map_or(0.0, |d| d.as_secs_f64()) >= interval,
        }
    }

    /// What the app's timer calls; on launch the app runs `refresh` and, if a run was due, `after_scheduled_check`.
    pub fn run_if_due(&mut self, now: SystemTime) -> io::Result<Vec<Notice>> {
        if !self.is_due(now) {
            return Ok(Vec::new());
        }
        self.refresh(true)?;
        self.after_scheduled_check()
    }

    /// Auto-update what may be (never a `manual_only` item) and notify about the rest.
    pub fn after_scheduled_check(&mut self) -> io::Result<Vec<Notice>> {
        let (auto, rest) = scheduled_plan(&self.updates_available(), self.prefs.auto_update());
        let mut notices = if auto.is_empty() {
            Vec::new()
        } else {
            self.update(&auto, true)?
        };
        if !rest.is_empty() && self.prefs.notify() {
            notices.push(Notice {
                title: format!("{} update{} available", rest.len(), plural(rest.len())),
                body: rest.join(", "),
            });
        }
        Ok(notices)
    }

    // Checking

    /// `record_run: false` rescans without moving the last run, so it doesn't postpone a scheduled run.
    pub fn refresh(&mut self, record_run: bool) -> io::Result<()> {
        let enabled = self.prefs.enabled_kinds();
        let kinds: Vec<SourceKind> = SourceKind::ALL
            .into_iter()
            .filter(|k| enabled.contains(k.id()))
            .collect();
        let scanner = &self.scanner;
        let mut found: Vec<UpdatableItem> = std::thread::scope(|s| {
            let running: Vec<_> = kinds
                .iter()
                .map(|&k| s.spawn(move || scanner.discover(k)))
                .collect();
            running
                .into_iter()
                // A source that panics finds nothing (and its checks stay unknown); the rest of the scan goes on.
                .flat_map(|h| h.join().unwrap_or_default())
                .collect()
        });
        found.sort_by_key(|i| (i.kind, i.name.to_lowercase()));

        let ignored = self.prefs.ignored();
        for item in &mut found {
            item.status = if ignored.contains(&item.id) {
                ItemStatus::Unknown(Some("Ignored".into()))
            } else {
                ItemStatus::Checking
            };
        }
        let statuses: Vec<_> = std::thread::scope(|s| {
            let running: Vec<_> = kinds
                .iter()
                .map(|&kind| {
                    let subset: Vec<UpdatableItem> = found
                        .iter()
                        .filter(|i| i.kind == kind && !ignored.contains(&i.id))
                        .cloned()
                        .collect();
                    s.spawn(move || {
                        if subset.is_empty() {
                            Default::default()
                        } else {
                            scanner.check(kind, &subset)
                        }
                    })
                })
                .collect();
            running
                .into_iter()
                .map(|h| h.join().unwrap_or_default())
                .collect()
        });
        for map in statuses {
            for item in &mut found {
                if let Some(status) = map.get(&item.id) {
                    item.status = status.clone();
                }
            }
        }
        let mut skipped = self.prefs.skipped_updates();
        let skips_before = skipped.len();
        for item in &mut found {
            if item.status == ItemStatus::Checking {
                item.status = ItemStatus::Unknown(None);
            }
            if let Some(offer) = skipped.get(&item.id).cloned() {
                let (status, keep) = item.status.resolving_skip(&offer);
                item.status = status;
                if !keep {
                    skipped.remove(&item.id);
                }
            }
        }
        self.items = found;
        if skipped.len() != skips_before {
            self.prefs.set_skipped_updates(&skipped)?;
        }
        if record_run {
            self.prefs.set_last_run(SystemTime::now())?;
        }

        let checked: Vec<&UpdatableItem> = self
            .items
            .iter()
            .filter(|i| !ignored.contains(&i.id))
            .collect();
        let failures = checked.iter().filter(|i| i.status.is_failure()).count();
        let available = checked
            .iter()
            .filter(|i| i.status.is_update_available())
            .count();
        let mut lines: Vec<String> = checked
            .iter()
            .map(|i| format!("{} ({}): {}", i.name, i.kind.title(), describe(&i.status)))
            .collect();
        if lines.is_empty() {
            lines.push("Nothing found. Check the Sources tab in Settings.".into());
        }
        let title = format!(
            "Checked {} item{}: {available} update{} available{}",
            checked.len(),
            plural(checked.len()),
            plural(available),
            if failures == 0 {
                String::new()
            } else {
                format!(", {failures} couldn't be checked")
            }
        );
        self.append_log(&title, &lines.join("\n"), failures == 0);
        Ok(())
    }

    // Updating

    /// Updates run one at a time, in the order given.
    pub fn update(&mut self, ids: &[String], notify_when_done: bool) -> io::Result<Vec<Notice>> {
        let post_commands = self.prefs.post_commands();
        let mut updated: Vec<UpdatableItem> = Vec::new();
        let mut failed: Vec<String> = Vec::new();
        for id in ids {
            let Some(index) = self.items.iter().position(|i| &i.id == id) else {
                continue;
            };
            self.items[index].status = ItemStatus::Updating;
            let item = self.items[index].clone();
            let result = self
                .scanner
                .update(&item, post_commands.get(id).map(String::as_str));
            self.append_log(
                &format!("Update {} ({})", item.name, item.kind.title()),
                &result.output,
                result.ok,
            );
            self.items[index].status = if result.ok {
                ItemStatus::Updated(result.summary)
            } else {
                ItemStatus::Failed(result.summary)
            };
            if result.ok {
                updated.push(item)
            } else {
                failed.push(item.name)
            }
        }

        let mut notices = Vec::new();
        if !notify_when_done || !self.prefs.notify() {
            return Ok(notices);
        }
        if !updated.is_empty() {
            let mut body = updated
                .iter()
                .map(|i| i.name.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            let agents: BTreeSet<&str> = updated
                .iter()
                .filter_map(|i| match i.kind {
                    SourceKind::Plugin => Some("Claude Code"),
                    SourceKind::CodexPlugin => Some("Codex"),
                    SourceKind::AntigravityPlugin => Some("Antigravity"),
                    SourceKind::GeminiExtension => Some("Gemini CLI"),
                    _ => None,
                })
                .collect();
            if !agents.is_empty() {
                let list: Vec<&str> = agents.into_iter().collect();
                body += &format!(". Restart {} to load the new versions.", list.join(" and "));
            }
            notices.push(Notice {
                title: format!("Updated {} item{}", updated.len(), plural(updated.len())),
                body,
            });
        }
        if !failed.is_empty() {
            notices.push(Notice {
                title: format!("{} update{} failed", failed.len(), plural(failed.len())),
                body: failed.join(", ") + ". Open the PullDay log for details.",
            });
        }
        Ok(notices)
    }

    // Skipping and ignoring

    pub fn set_ignored(&mut self, id: &str, ignore: bool) -> io::Result<()> {
        let mut ignored = self.prefs.ignored();
        if ignore {
            ignored.insert(id.into())
        } else {
            ignored.remove(id)
        };
        self.prefs.set_ignored(&ignored)?;
        if let Some(item) = self
            .items
            .iter_mut()
            .find(|i| i.id == id)
            .filter(|_| ignore)
        {
            item.status = ItemStatus::Unknown(Some("Ignored".into()));
        }
        Ok(())
    }

    /// Skip This Update: only an offer with a message can be skipped, since the message is what's compared.
    pub fn skip_update(&mut self, id: &str) -> io::Result<()> {
        let Some(item) = self.items.iter_mut().find(|i| i.id == id) else {
            return Ok(());
        };
        let ItemStatus::UpdateAvailable(Some(offer)) = item.status.clone() else {
            return Ok(());
        };
        item.status = ItemStatus::Skipped(offer.clone());
        let mut skipped = self.prefs.skipped_updates();
        skipped.insert(id.into(), offer);
        self.prefs.set_skipped_updates(&skipped)
    }

    pub fn stop_skipping(&mut self, id: &str) -> io::Result<()> {
        let mut skipped = self.prefs.skipped_updates();
        let Some(offer) = skipped.remove(id) else {
            return Ok(());
        };
        self.prefs.set_skipped_updates(&skipped)?;
        if let Some(item) = self
            .items
            .iter_mut()
            .find(|i| i.id == id && matches!(i.status, ItemStatus::Skipped(_)))
        {
            item.status = ItemStatus::UpdateAvailable(Some(offer));
        }
        Ok(())
    }

    pub fn clear_log(&mut self) {
        self.log.clear();
    }

    fn append_log(&mut self, title: &str, output: &str, success: bool) {
        let entry = LogEntry {
            date: SystemTime::now(),
            title: title.into(),
            output: output.into(),
            success,
        };
        log_append(&self.log_file, entry.date, title, output, success);
        self.log.insert(0, entry);
        self.log.truncate(200);
    }
}

/// Which updates the schedule installs (ids) and which it only announces (names).
pub fn scheduled_plan(
    available: &[&UpdatableItem],
    auto_update: bool,
) -> (Vec<String>, Vec<String>) {
    let (auto, rest): (Vec<&&UpdatableItem>, Vec<&&UpdatableItem>) = available
        .iter()
        .partition(|i| auto_update && !i.manual_only);
    (
        auto.iter().map(|i| i.id.clone()).collect(),
        rest.iter().map(|i| i.name.clone()).collect(),
    )
}

/// The menu's sections: an item is listed under every installed agent that loads it, so a shared skill shows up
/// more than once. Items no installed agent loads (uv/pipx/npm tools, skills for missing agents) go under "Other".
pub fn groups(visible: &[&UpdatableItem], detected: &[Agent]) -> Vec<ItemGroup> {
    let mut result = Vec::new();
    for agent in detected.iter().copied().map(Some).chain([None]) {
        let mut first = true;
        for kind in SourceKind::ALL {
            let items: Vec<UpdatableItem> = visible
                .iter()
                .filter(|i| {
                    i.kind == kind
                        && match agent {
                            Some(a) => i.agents.contains(&a),
                            None => detected.iter().all(|d| !i.agents.contains(d)),
                        }
                })
                .map(|i| (*i).clone())
                .collect();
            if items.is_empty() {
                continue;
            }
            result.push(ItemGroup {
                agent,
                kind,
                items,
                starts_section: first,
            });
            first = false;
        }
    }
    result
}

/// What a section's "Update all (n)" updates, in the order shown.
pub fn pending(groups: &[ItemGroup], agent: Option<Agent>) -> Vec<String> {
    groups
        .iter()
        .filter(|g| g.agent == agent)
        .flat_map(|g| &g.items)
        .filter(|i| i.status.is_update_available())
        .map(|i| i.id.clone())
        .collect()
}

pub fn describe(status: &ItemStatus) -> String {
    let first_line = |s: &str| s.split('\n').next().unwrap_or("").to_string();
    match status {
        ItemStatus::Unknown(reason) => reason.clone().unwrap_or_else(|| "unknown".into()),
        ItemStatus::Checking => "checking".into(),
        ItemStatus::UpToDate => "up to date".into(),
        ItemStatus::UpdateAvailable(info) => {
            "update available".to_string()
                + &info
                    .as_deref()
                    .map_or(String::new(), |i| format!(" ({})", first_line(i)))
        }
        ItemStatus::Pinned(message) => message.clone(),
        ItemStatus::Skipped(offer) => format!("skipped ({})", first_line(offer)),
        ItemStatus::Updating => "updating".into(),
        ItemStatus::Updated(summary) => summary.clone(),
        ItemStatus::Failed(message) => format!("error: {message}"),
    }
}

fn plural(n: usize) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::Duration;

    struct Tmp(PathBuf);
    impl Tmp {
        fn new(name: &str) -> Tmp {
            let dir =
                std::env::temp_dir().join(format!("pullday-state-{}-{name}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            Tmp(dir)
        }
        fn p(&self, rel: &str) -> String {
            self.0.join(rel).to_string_lossy().into()
        }
        fn write(&self, rel: &str, text: &str) {
            let path = self.0.join(rel);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
        }
    }
    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// A home with two local Gemini extensions, whose checks need no network; only that kind is enabled.
    fn core(t: &Tmp) -> Core {
        t.write(
            "src/gemini-extension.json",
            r#"{"name":"src","version":"2.0.0"}"#,
        );
        let record = format!(r#"{{"source":"{}","type":"local"}}"#, t.p("src"));
        for (name, version) in [("old", "1.0.0"), ("current", "2.0.0")] {
            t.write(
                &format!("home/.gemini/extensions/{name}/gemini-extension.json"),
                &format!(r#"{{"name":"{name}","version":"{version}"}}"#),
            );
            t.write(
                &format!("home/.gemini/extensions/{name}/.gemini-extension-install.json"),
                &record,
            );
        }
        let mut prefs = Prefs::load(&t.0.join("config.json"));
        prefs
            .set_enabled_kinds(&["geminiExtension".to_string()].into())
            .unwrap();
        Core::new(Scanner::new(t.p("home")), prefs, t.0.join("pullday.log"))
    }

    fn status<'a>(c: &'a Core, id: &str) -> &'a ItemStatus {
        &c.items.iter().find(|i| i.id == id).unwrap().status
    }

    #[test]
    fn refresh_checks_and_logs() {
        let t = Tmp::new("refresh");
        let mut c = core(&t);
        c.refresh(true).unwrap();
        assert_eq!(
            c.items.iter().map(|i| i.name.as_str()).collect::<Vec<_>>(),
            ["current", "old"]
        );
        assert_eq!(status(&c, "geminiExtension:current"), &ItemStatus::UpToDate);
        assert_eq!(
            status(&c, "geminiExtension:old"),
            &ItemStatus::UpdateAvailable(Some("v1.0.0 → v2.0.0".into()))
        );
        assert_eq!(c.log[0].title, "Checked 2 items: 1 update available");
        assert!(fs::read_to_string(t.0.join("pullday.log"))
            .unwrap()
            .contains("old (Gemini CLI extensions): update available (v1.0.0 → v2.0.0)"));
        assert!(
            c.prefs.last_run().is_some() && !c.is_due(SystemTime::now()),
            "a check moves the last run"
        );

        c.set_ignored("geminiExtension:old", true).unwrap();
        c.refresh(false).unwrap();
        assert_eq!(
            status(&c, "geminiExtension:old"),
            &ItemStatus::Unknown(Some("Ignored".into()))
        );
        assert!(
            c.updates_available().is_empty()
                && c.log[0].title == "Checked 1 item: 0 updates available"
        );
    }

    /// The skip holds while the same offer comes back, ends with a newer one, and Stop Skipping restores the offer.
    #[test]
    fn skipping() {
        let t = Tmp::new("skip");
        let mut c = core(&t);
        c.refresh(true).unwrap();
        c.skip_update("geminiExtension:current").unwrap();
        assert!(
            c.prefs.skipped_updates().is_empty(),
            "nothing to skip on an up-to-date item"
        );
        c.skip_update("geminiExtension:old").unwrap();
        let offer = "v1.0.0 → v2.0.0".to_string();
        assert_eq!(
            status(&c, "geminiExtension:old"),
            &ItemStatus::Skipped(offer.clone())
        );
        c.refresh(true).unwrap();
        assert_eq!(
            status(&c, "geminiExtension:old"),
            &ItemStatus::Skipped(offer.clone()),
            "the same offer stays skipped"
        );
        assert!(
            c.updates_available().is_empty(),
            "a skipped update isn't counted"
        );

        c.stop_skipping("geminiExtension:old").unwrap();
        assert_eq!(
            status(&c, "geminiExtension:old"),
            &ItemStatus::UpdateAvailable(Some(offer.clone()))
        );
        assert!(c.prefs.skipped_updates().is_empty());

        c.skip_update("geminiExtension:old").unwrap();
        t.write(
            "src/gemini-extension.json",
            r#"{"name":"src","version":"3.0.0"}"#,
        );
        c.refresh(true).unwrap();
        assert_eq!(
            status(&c, "geminiExtension:old"),
            &ItemStatus::UpdateAvailable(Some("v1.0.0 → v3.0.0".into()))
        );
        assert!(
            c.prefs.skipped_updates().is_empty(),
            "a newer offer ends the skip"
        );
    }

    #[test]
    fn the_schedule() {
        let t = Tmp::new("schedule");
        let mut c = core(&t);
        let now = SystemTime::now();
        assert!(c.is_due(now), "never run: due");
        c.prefs
            .set_last_run(now - Duration::from_secs(3600))
            .unwrap();
        assert!(!c.is_due(now), "an hour ago, with a daily interval");
        c.prefs
            .set_last_run(now - Duration::from_secs(86401))
            .unwrap();
        assert!(c.is_due(now));
        c.prefs.set_check_interval(0.0).unwrap();
        assert!(!c.is_due(now), "0 is manual only");
        assert!(
            c.run_if_due(now).unwrap().is_empty() && c.items.is_empty(),
            "nothing runs when not due"
        );

        // Auto-update off (the default): the due run checks and only notifies.
        c.prefs.set_check_interval(86400.0).unwrap();
        let notices = c.run_if_due(now).unwrap();
        assert_eq!(
            notices,
            [Notice {
                title: "1 update available".into(),
                body: "old".into()
            }]
        );
        c.prefs.set_notify(false).unwrap();
        c.prefs
            .set_last_run(now - Duration::from_secs(86401))
            .unwrap();
        assert!(
            c.run_if_due(now).unwrap().is_empty(),
            "no notifications when they're off"
        );
    }

    #[test]
    fn auto_update_never_touches_manual_only_items() {
        let item = |name: &str, manual: bool| {
            let mut i = UpdatableItem::new(
                SourceKind::GitSkill,
                name,
                "",
                Some(format!("gitSkill:/r/{name}")),
            );
            i.manual_only = manual;
            i.status = ItemStatus::UpdateAvailable(Some("1 new commit".into()));
            i
        };
        let (skill, project) = (item("skill", false), item("project", true));
        let available = [&skill, &project];
        assert_eq!(
            scheduled_plan(&available, true),
            (vec!["gitSkill:/r/skill".into()], vec!["project".into()])
        );
        assert_eq!(
            scheduled_plan(&available, false),
            (vec![], vec!["skill".into(), "project".into()])
        );
    }

    #[test]
    fn updates_report_and_notify() {
        let t = Tmp::new("update");
        let mut c = core(&t);
        // A plugin folder that isn't a git checkout: the update fails without running anything.
        let mut old = UpdatableItem::new(SourceKind::AntigravityPlugin, "old", "v1", None);
        old.path = Some(t.p("home"));
        c.items.push(old);
        // An unknown id is skipped.
        let ids = ["antigravityPlugin:old".to_string(), "nope".to_string()];
        let notices = c.update(&ids, true).unwrap();
        assert!(matches!(
            status(&c, "antigravityPlugin:old"),
            ItemStatus::Failed(_)
        ));
        assert_eq!(notices.len(), 1);
        assert_eq!(notices[0].title, "1 update failed");
        assert_eq!(notices[0].body, "old. Open the PullDay log for details.");
        assert!(c.log[0].title == "Update old (Antigravity plugins)" && !c.log[0].success);
        assert!(
            c.update(&ids, false).unwrap().is_empty(),
            "no notices unless asked"
        );
    }

    #[test]
    fn menu_sections_and_update_all() {
        use Agent::*;
        let mk = |kind, name: &str, agents: &[Agent], available: bool| {
            let mut i = UpdatableItem::new(kind, name, "", None);
            i.agents = agents.iter().copied().collect();
            i.status = if available {
                ItemStatus::UpdateAvailable(Some("x".into()))
            } else {
                ItemStatus::UpToDate
            };
            i
        };
        let shared = mk(SourceKind::GitSkill, "shared", &[Claude, Codex], true);
        let plugin = mk(SourceKind::Plugin, "mk", &[Claude], true);
        let cursor_only = mk(SourceKind::GitSkill, "c", &[Cursor], true);
        let tool = mk(SourceKind::Uv, "tool", &[], false);
        let visible = [&shared, &plugin, &cursor_only, &tool];
        let g = groups(&visible, &[Claude, Codex]);
        let ids: Vec<String> = g.iter().map(ItemGroup::id).collect();
        assert_eq!(
            ids,
            [
                "claude|plugin",
                "claude|gitSkill",
                "codex|gitSkill",
                "other|gitSkill",
                "other|uv"
            ]
        );
        assert_eq!(
            g.iter().map(|g| g.starts_section).collect::<Vec<_>>(),
            [true, false, true, true, false]
        );
        assert_eq!(
            pending(&g, Some(Claude)),
            ["plugin:mk", "gitSkill:shared"],
            "in the order shown"
        );
        assert_eq!(
            pending(&g, None),
            ["gitSkill:c"],
            "a skill for a missing agent is under Other"
        );
    }

    #[test]
    fn statuses_read_as_in_the_log() {
        assert_eq!(
            describe(&ItemStatus::UpdateAvailable(Some(
                "2 new commits\nabc fix".into()
            ))),
            "update available (2 new commits)"
        );
        assert_eq!(
            describe(&ItemStatus::Skipped("v1 → v2\nmore".into())),
            "skipped (v1 → v2)"
        );
        assert_eq!(describe(&ItemStatus::Unknown(None)), "unknown");
        assert_eq!(describe(&ItemStatus::Failed("boom".into())), "error: boom");
    }
}
