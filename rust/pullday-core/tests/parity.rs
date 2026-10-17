//! The Rust models must describe the same kinds, agents, skill roots, item ids and agent sets as the
//! Swift app. `Tests/models.golden` is the shared expectation; the Swift self-test checks it too.

use pullday_core::models::{Agent, SourceKind, UpdatableItem};
use std::collections::BTreeSet;

fn names(agents: &BTreeSet<Agent>) -> String {
    agents.iter().map(|a| a.id()).collect::<Vec<_>>().join(",")
}

fn dump() -> String {
    let mut lines = Vec::new();
    for k in SourceKind::ALL {
        let enabled = k.enabled_by_default().to_string();
        lines.push(
            [
                "kind",
                k.id(),
                k.title(),
                k.symbol(),
                k.explanation(),
                &enabled,
                &names(&k.agents()),
            ]
            .join("\t"),
        );
    }
    for a in Agent::ALL {
        lines.push(["agent", a.id(), a.title(), a.home_dir()].join("\t"));
    }
    for (root, agents) in Agent::skill_root_readers() {
        lines.push(["root", root, &names(&agents)].join("\t"));
    }
    let mut items: Vec<UpdatableItem> = SourceKind::ALL
        .into_iter()
        .flat_map(|k| ["x", "graphifyy"].map(|n| UpdatableItem::new(k, n, "", None)))
        .collect();
    items.push(UpdatableItem::new(
        SourceKind::GitSkill,
        "x",
        "",
        Some("gitSkill:/home/u/.claude/skills/x".into()),
    ));
    for i in items {
        lines.push(["item", &i.id, &names(&i.agents)].join("\t"));
    }
    lines.join("\n") + "\n"
}

#[test]
fn models_match_swift() {
    let golden = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../Tests/models.golden"
    ))
    .unwrap();
    assert_eq!(dump(), golden);
}

/// The fixture home of the Swift self-test's `scannerGolden`, scanned the same way: both must print
/// `Tests/scanner.golden` (ids, details, agents, manual-only and statuses).
#[test]
fn scanner_matches_swift() {
    use pullday_core::models::ItemStatus;
    use pullday_core::scanner::{
        antigravity_plugins, discover_codex_plugins, discover_gemini_extensions,
        gemini_extension_status, Scanner,
    };
    use std::os::unix::fs::symlink;

    let base = std::env::temp_dir().join(format!("pullday-parity-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let b = base.to_string_lossy().to_string();
    let dirs = [
        "home/.claude/skills/claudeonly/.git",
        "home/repos/both/.git",
        "home/.agents/skills",
        "home/.gemini/.git",
        "home/.gemini/skills/plain",
        "home/.config/.git",
        "home/.config/opencode/skills/plain",
        "home/dotfiles/.git",
        "home/dotfiles/cursor/skills/plain",
        "codex/mk/norecord",
        "codex/mk/.staging",
        "codex/cache/local/superpowers",
    ];
    let links = [
        ("home/.claude/skills/both", "home/repos/both"),
        ("home/.agents/skills/both", "home/repos/both"),
        ("home/.cursor", "home/dotfiles/cursor"),
    ];
    let local = format!(r#"{{"source":"{b}/gemini/src","type":"local"}}"#);
    let files = [
        (
            "codex/mk/local/.codex-marketplace-install.json",
            r#"{"source_type":"local","source":"/x","revision":"abc"}"#,
        ),
        (
            "codex/mk/norev/.codex-marketplace-install.json",
            r#"{"source_type":"git","source":"o/r","ref_name":"main"}"#,
        ),
        (
            "gemini/src/gemini-extension.json",
            r#"{"name":"src","version":"2.0.0"}"#,
        ),
        (
            "gemini/ext/current/gemini-extension.json",
            r#"{"name":"current","version":"2.0.0"}"#,
        ),
        ("gemini/ext/current/.gemini-extension-install.json", &local),
        (
            "gemini/ext/old/gemini-extension.json",
            r#"{"name":"old","version":"1.0.0"}"#,
        ),
        ("gemini/ext/old/.gemini-extension-install.json", &local),
        (
            "gemini/ext/linked/gemini-extension.json",
            r#"{"name":"linked"}"#,
        ),
        (
            "gemini/ext/linked/.gemini-extension-install.json",
            r#"{"source":"/x","type":"link"}"#,
        ),
        (
            "gemini/ext/moved/gemini-extension.json",
            r#"{"name":"moved","migratedTo":"https://github.com/n/r"}"#,
        ),
        (
            "gemini/ext/moved/.gemini-extension-install.json",
            r#"{"source":"o/r","type":"git"}"#,
        ),
        (
            "gemini/ext/gitplain/gemini-extension.json",
            r#"{"name":"gitplain","version":"1.0.0"}"#,
        ),
        (
            "gemini/ext/gitplain/.gemini-extension-install.json",
            r#"{"source":"o/r","type":"git"}"#,
        ),
        ("gemini/ext/odd/gemini-extension.json", r#"{"name":"odd"}"#),
        (
            "gemini/ext/odd/.gemini-extension-install.json",
            r#"{"type":"tarball"}"#,
        ),
        (
            "gemini/ext/bare/gemini-extension.json",
            r#"{"name":"bare","version":"1"}"#,
        ),
        (
            "gemini/ext/nameless/gemini-extension.json",
            r#"{"version":"1"}"#,
        ),
    ];
    for dir in dirs {
        std::fs::create_dir_all(base.join(dir)).unwrap();
    }
    for (path, target) in links {
        symlink(base.join(target), base.join(path)).unwrap();
    }
    for (path, text) in files {
        std::fs::create_dir_all(base.join(path).parent().unwrap()).unwrap();
        std::fs::write(base.join(path), text).unwrap();
    }
    let agy_list = r#"[{"name": "ponytail", "enabled": true, "version": "5.3.1", "path": "/home/u/.gemini/config/plugins/ponytail"}, {"name": "alpha", "enabled": false, "path": "/home/u/.gemini/config/plugins/alpha"}]"#;

    fn text(status: Option<&ItemStatus>) -> String {
        let Some(status) = status else {
            return "-".into();
        };
        match status {
            ItemStatus::Unknown(why) => why
                .as_ref()
                .map_or("unknown".into(), |w| format!("unknown: {w}")),
            ItemStatus::Checking => "checking".into(),
            ItemStatus::UpToDate => "upToDate".into(),
            ItemStatus::UpdateAvailable(offer) => {
                offer.as_ref().map_or("updateAvailable".into(), |o| {
                    format!("updateAvailable: {o}")
                })
            }
            ItemStatus::Pinned(s) => format!("pinned: {s}"),
            ItemStatus::Skipped(s) => format!("skipped: {s}"),
            ItemStatus::Updating => "updating".into(),
            ItemStatus::Updated(s) => format!("updated: {s}"),
            ItemStatus::Failed(s) => format!("failed: {s}"),
        }
    }
    let row = |item: &UpdatableItem, status: Option<&ItemStatus>| {
        let agents = item
            .agents
            .iter()
            .map(|a| a.id())
            .collect::<Vec<_>>()
            .join(",");
        [
            "item",
            &item.id,
            &item.detail,
            &agents,
            &item.manual_only.to_string(),
            &text(status),
        ]
        .join("\t")
    };
    // Swift's tilde() is relative to the real home, so its git skill details are full paths; here the scanner's
    // home is the fixture's. Both become "~" below.
    // git must not walk up out of the fixture (a temp folder inside a repo would give gitplain that repo's HEAD).
    // Safe here: the other test in this binary reads no environment.
    std::env::set_var("GIT_CEILING_DIRECTORIES", &base);
    let scanner = Scanner::new(format!("{b}/home"));
    let mut lines: Vec<String> = scanner
        .discover_git_skills()
        .iter()
        .map(|i| row(i, None))
        .collect();
    let codex = discover_codex_plugins(&format!("{b}/codex/mk"), &format!("{b}/codex/cache"));
    let statuses = scanner.check(SourceKind::CodexPlugin, &codex);
    lines.extend(codex.iter().map(|i| row(i, statuses.get(&i.id))));
    for item in discover_gemini_extensions(&format!("{b}/gemini/ext")) {
        lines.push(row(
            &item,
            Some(&gemini_extension_status(item.path.as_deref().unwrap_or(""))),
        ));
    }
    for item in antigravity_plugins(agy_list) {
        lines.push(row(
            &item,
            Some(&scanner.antigravity_plugin_status(item.path.as_deref().unwrap_or(""))),
        ));
    }
    // The normalising below would hide a broken tilde(), so check the git skill details are ~ forms first.
    assert!(
        lines
            .iter()
            .take(2)
            .all(|l| l.split('\t').nth(2).is_some_and(|d| d.starts_with("~/"))),
        "{lines:?}"
    );
    let resolved = std::fs::canonicalize(&base)
        .unwrap()
        .to_string_lossy()
        .to_string();
    let dump: String = lines
        .iter()
        .map(|l| {
            l.replace(&resolved, "<tmp>")
                .replace(&b, "<tmp>")
                .replace("<tmp>/home", "~")
                + "\n"
        })
        .collect();
    std::fs::remove_dir_all(&base).unwrap();
    let golden = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../Tests/scanner.golden"
    ))
    .unwrap();
    assert_eq!(dump, golden);
}
