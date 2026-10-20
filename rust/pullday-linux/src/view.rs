//! Pure: app state -> what the tray menu and windows show.

use pullday_core::models::{Agent, ItemStatus, SourceKind, UpdatableItem};
use pullday_core::prefs::Prefs;
use pullday_core::state::{groups, pending, updates_available, visible, LogEntry};
use serde_json::{json, Value};
use std::time::UNIX_EPOCH;

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

/// The windows' state. `phase` is "idle", "scanning" or "updating"; `detected` are the installed agents.
// shortcut: the whole log (≤ 200 entries) rides along on every snapshot to every window; send it to the Log
// window only if the emits ever show up as slow.
pub fn snapshot(
    items: &[UpdatableItem],
    prefs: &Prefs,
    log: &[LogEntry],
    detected: &[Agent],
    phase: &str,
) -> Value {
    let groups = groups(&visible(items, prefs), detected);
    let (ignored, post_commands, enabled) = (
        prefs.ignored(),
        prefs.post_commands(),
        prefs.enabled_kinds(),
    );
    let mut sections: Vec<Value> = Vec::new();
    for g in &groups {
        if g.starts_section {
            sections.push(json!({
                "id": g.agent.map_or("other", |a| a.id()),
                "title": g.agent.map_or("Other", |a| a.title()),
                "pending": pending(&groups, g.agent),
                "groups": [],
            }));
        }
        let Some(Value::Array(list)) = sections.last_mut().map(|s| &mut s["groups"]) else {
            continue;
        };
        list.push(json!({
            "id": g.id(),
            "title": g.kind.title(),
            "rows": g.items.iter().map(row).collect::<Vec<_>>(),
        }));
    }
    json!({
        "phase": phase,
        "lastRun": prefs.last_run().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_secs()),
        "updateAllIds": updates_available(items, prefs).iter().map(|i| &i.id).collect::<Vec<_>>(),
        "sections": sections,
        // Settings → Items: every item, ignored ones too.
        "items": items.iter().map(|i| {
            let agents = Agent::ALL.into_iter().filter(|a| i.agents.contains(a)).map(Agent::title);
            json!({
                "id": i.id,
                "name": i.name,
                "symbol": i.kind.symbol(),
                "meta": std::iter::once(i.kind.title()).chain(agents).collect::<Vec<_>>().join(" · "),
                "ignored": ignored.contains(&i.id),
                "postCommand": post_commands.get(&i.id),
            })
        }).collect::<Vec<_>>(),
        "prefs": {
            "checkInterval": prefs.check_interval(),
            "autoUpdate": prefs.auto_update(),
            "notify": prefs.notify(),
        },
        "agents": Agent::ALL.into_iter().map(|a| json!({
            "title": a.title(),
            "roots": Agent::skill_root_readers().into_iter()
                .filter(|(_, readers)| readers.contains(&a))
                .map(|(root, _)| format!("~/{root}"))
                .collect::<Vec<_>>().join(", "),
            "found": detected.contains(&a),
        })).collect::<Vec<_>>(),
        "kinds": SourceKind::ALL.into_iter().map(|k| json!({
            "id": k.id(),
            "title": k.title(),
            "explanation": k.explanation(),
            "symbol": k.symbol(),
            "enabled": enabled.contains(k.id()),
        })).collect::<Vec<_>>(),
        "log": log.iter().map(|e| json!({
            "date": e.date.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs()),
            "title": e.title,
            "output": e.output,
            "success": e.success,
        })).collect::<Vec<_>>(),
    })
}

/// One row, as `ItemRow` in MenuView.swift draws it.
pub fn row(item: &UpdatableItem) -> Value {
    use ItemStatus::*;
    let first_line = |s: &str| s.split('\n').next().unwrap_or("").to_string();
    let detail = &item.detail;
    let subtitle = match &item.status {
        UpdateAvailable(info) => info
            .as_deref()
            .map_or("Update available".into(), first_line),
        Failed(message) => message.clone(),
        Updated(summary) => summary.clone(),
        Checking => "Checking…".into(),
        Updating => "Updating…".into(),
        Unknown(reason) => reason
            .as_ref()
            .map_or(detail.clone(), |r| format!("{detail} · {r}")),
        Pinned(message) => format!("{detail} · {message}"),
        Skipped(offer) => format!("{detail} · Skipped: {}", first_line(offer)),
        UpToDate => detail.clone(),
    };
    // The tooltip shows the whole message, e.g. the commit list under "2 new commits".
    let tooltip = match &item.status {
        UpdateAvailable(Some(info)) => info.clone(),
        _ => subtitle.clone(),
    };
    let tone = match item.status {
        Failed(_) => "warning",
        UpdateAvailable(_) => "accent",
        Updated(_) => "success",
        _ => "secondary",
    };
    let trailing = match item.status {
        Checking | Updating => "spinner",
        UpdateAvailable(_) => "update",
        UpToDate | Updated(_) => "ok",
        Failed(_) => "failed",
        Skipped(_) => "skipped",
        Pinned(_) => "pinned",
        Unknown(_) => "unknown",
    };
    json!({
        "id": item.id,
        "name": item.name,
        "symbol": item.kind.symbol(),
        "subtitle": subtitle,
        "tooltip": tooltip,
        "tone": tone,
        "trailing": trailing,
        "canSkip": matches!(item.status, UpdateAvailable(Some(_))),
        "canStopSkipping": matches!(item.status, Skipped(_)),
        "hasPath": item.path.is_some(),
    })
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
        assert!(busy[0].2 && busy[4].2);
        assert_eq!(menu[3], ("settings", "Settings…".into(), true));
    }

    fn item(status: ItemStatus) -> UpdatableItem {
        let mut i = UpdatableItem::new(SourceKind::Uv, "tool", "1.0", None);
        i.status = status;
        i
    }

    #[test]
    fn rows_read_as_on_the_mac() {
        use ItemStatus::*;
        let s = |x: &str| x.to_string();
        // status, subtitle, tone, trailing
        let cases = [
            (Unknown(None), "1.0", "secondary", "unknown"),
            (
                Unknown(Some(s("Ignored"))),
                "1.0 · Ignored",
                "secondary",
                "unknown",
            ),
            (Checking, "Checking…", "secondary", "spinner"),
            (UpToDate, "1.0", "secondary", "ok"),
            (
                UpdateAvailable(None),
                "Update available",
                "accent",
                "update",
            ),
            (
                UpdateAvailable(Some(s("2 new commits\nabc fix"))),
                "2 new commits",
                "accent",
                "update",
            ),
            (
                Pinned(s("pinned to 1.0")),
                "1.0 · pinned to 1.0",
                "secondary",
                "pinned",
            ),
            (
                Skipped(s("v1 → v2\nmore")),
                "1.0 · Skipped: v1 → v2",
                "secondary",
                "skipped",
            ),
            (Updating, "Updating…", "secondary", "spinner"),
            (
                Updated(s("Updated to 2.0")),
                "Updated to 2.0",
                "success",
                "ok",
            ),
            (Failed(s("offline")), "offline", "warning", "failed"),
        ];
        for (status, subtitle, tone, trailing) in cases {
            let r = row(&item(status.clone()));
            assert_eq!(
                (
                    r["subtitle"].as_str(),
                    r["tone"].as_str(),
                    r["trailing"].as_str()
                ),
                (Some(subtitle), Some(tone), Some(trailing)),
                "{status:?}"
            );
            let offer = matches!(status, UpdateAvailable(Some(_)));
            assert_eq!(
                r["canSkip"], offer,
                "only an offer with a message: {status:?}"
            );
            assert_eq!(r["canStopSkipping"], matches!(status, Skipped(_)));
            if !offer {
                assert_eq!(r["tooltip"], r["subtitle"], "{status:?}");
            }
        }
        let r = row(&item(UpdateAvailable(Some(s(
            "2 new commits\nabc fix\ndef",
        )))));
        assert_eq!(r["tooltip"], "2 new commits\nabc fix\ndef");
        assert_eq!(r["hasPath"], false);
        let mut with_path = item(UpToDate);
        with_path.path = Some("/x".into());
        assert_eq!(row(&with_path)["hasPath"], true);
    }

    #[test]
    fn sections_pending_and_update_all() {
        use Agent::*;
        let dir = std::env::temp_dir().join(format!("pullday-view-{}", std::process::id()));
        let mut prefs = Prefs::load(&dir.join("config.json"));
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
        let items = [
            mk(SourceKind::Plugin, "mk", &[Claude], true),
            mk(SourceKind::GitSkill, "shared", &[Claude, Codex], true),
            mk(SourceKind::GitSkill, "hidden", &[Codex], true),
            mk(SourceKind::Uv, "tool", &[], false),
        ];
        prefs
            .set_ignored(&["gitSkill:hidden".to_string()].into())
            .unwrap();
        let log = [LogEntry {
            date: UNIX_EPOCH + std::time::Duration::from_secs(60),
            title: "Checked".into(),
            output: String::new(),
            success: false,
        }];
        let snap = snapshot(&items, &prefs, &log, &[Claude, Codex], "scanning");
        let _ = std::fs::remove_dir_all(&dir);

        // Settings and Log.
        assert_eq!(
            snap["items"].as_array().unwrap().len(),
            4,
            "ignored ones too"
        );
        assert_eq!(
            snap["items"][1]["meta"],
            "Git-cloned skills · Claude Code · Codex"
        );
        assert_eq!(snap["items"][2]["ignored"], true);
        assert_eq!(snap["items"][0]["ignored"], false);
        assert_eq!(snap["items"][3]["meta"], "uv tools");
        assert_eq!(snap["items"][3]["postCommand"], Value::Null);
        assert_eq!(snap["prefs"]["checkInterval"], 86400.0);
        assert_eq!(snap["agents"][1]["roots"], "~/.agents/skills");
        assert_eq!(snap["agents"][1]["found"], true);
        assert_eq!(snap["agents"][2]["found"], false);
        let kinds = snap["kinds"].as_array().unwrap();
        assert_eq!(kinds.len(), SourceKind::ALL.len());
        let npm = kinds.iter().find(|k| k["id"] == "npm").unwrap();
        assert_eq!(npm["enabled"], false, "off by default");
        assert!(npm["explanation"]
            .as_str()
            .unwrap()
            .contains("`npm install -g`"));
        assert_eq!(
            snap["log"],
            json!([{"date": 60, "title": "Checked", "output": "", "success": false}])
        );

        assert_eq!(snap["phase"], "scanning");
        assert_eq!(
            snap["updateAllIds"],
            json!(["plugin:mk", "gitSkill:shared"]),
            "a shared skill once, an ignored one never"
        );
        let shape: Vec<(String, Vec<String>, Vec<String>)> = snap["sections"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| {
                let strings = |v: &Value| -> Vec<String> {
                    v.as_array()
                        .unwrap()
                        .iter()
                        .map(|x| x.as_str().unwrap().to_string())
                        .collect()
                };
                let groups: Vec<String> = s["groups"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|g| {
                        format!(
                            "{}={}",
                            g["title"].as_str().unwrap(),
                            g["rows"].as_array().unwrap().len()
                        )
                    })
                    .collect();
                (
                    s["title"].as_str().unwrap().to_string(),
                    strings(&s["pending"]),
                    groups,
                )
            })
            .collect();
        let v = |x: &[&str]| x.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            shape,
            [
                (
                    "Claude Code".to_string(),
                    v(&["plugin:mk", "gitSkill:shared"]),
                    v(&["Claude Code plugins=1", "Git-cloned skills=1"])
                ),
                (
                    "Codex".to_string(),
                    v(&["gitSkill:shared"]),
                    v(&["Git-cloned skills=1"])
                ),
                ("Other".to_string(), v(&[]), v(&["uv tools=1"])),
            ]
        );
    }
}
