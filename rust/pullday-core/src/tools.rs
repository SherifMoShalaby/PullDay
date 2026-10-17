//! Port of the package and CLI sources in `Sources/Scanner.swift`: uv, pipx and npm tools, graphify's skill files,
//! and the `claude`, `codex` and `agy` commands. Parsers are pure functions; the rest runs the commands.
//!
//! Linux differs from the Mac app only for `claude`: there's no Homebrew cask, so a native or npm install is checked
//! against the release on its update channel and updated with `claude update`, and a system package (apt, dnf, apk)
//! is reported, never updated, since that needs root.

use crate::models::{ItemStatus, SourceKind, UpdatableItem};
use crate::scanner::{each_item, Statuses};
use crate::shell::{quote, run as shell_run, split_lines, ShellResult, Transcript};
use serde_json::Value;
use std::collections::HashMap;
use std::time::Duration;

fn run(command: &str, seconds: u64) -> ShellResult {
    shell_run(command, Duration::from_secs(seconds))
}

fn unknown(why: &str) -> ItemStatus {
    ItemStatus::Unknown(Some(why.into()))
}

/// Post-update commands a new install starts with: upgrading graphify's CLI doesn't refresh its skill files.
pub fn default_post_commands() -> HashMap<String, String> {
    ["uv:graphifyy", "pipx:graphifyy"]
        .map(|id| (id.to_string(), "graphify install".to_string()))
        .into()
}

// Discover

pub fn discover_uv() -> Vec<UpdatableItem> {
    let result = run("uv tool list", 60);
    if result.ok() {
        uv_tools(&result.output)
    } else {
        Vec::new()
    }
}

/// Tool lines look like "graphifyy v0.5.1"; executables are indented "- graphify".
pub fn uv_tools(output: &str) -> Vec<UpdatableItem> {
    split_lines(output)
        .into_iter()
        .filter(|line| {
            line.chars()
                .next()
                .is_some_and(|c| !c.is_whitespace() && c != '-')
        })
        .filter_map(|line| match words(line)[..] {
            [name, version, ..] if version.starts_with('v') => {
                Some(UpdatableItem::new(SourceKind::Uv, name, version, None))
            }
            _ => None,
        })
        .collect()
}

pub fn discover_pipx() -> Vec<UpdatableItem> {
    let result = run("pipx list --short", 60);
    if result.ok() {
        pipx_tools(&result.output)
    } else {
        Vec::new()
    }
}

/// `pipx list --short` lines are "name version".
pub fn pipx_tools(output: &str) -> Vec<UpdatableItem> {
    split_lines(output)
        .into_iter()
        .filter_map(|line| match words(line)[..] {
            [name, version, ..] => Some(UpdatableItem::new(
                SourceKind::Pipx,
                name,
                &format!("v{version}"),
                None,
            )),
            _ => None,
        })
        .collect()
}

pub fn discover_npm() -> Vec<UpdatableItem> {
    npm_globals(&run("npm ls -g --depth=0 --json", 60).output)
}

/// Registry installs carry no `resolved`; linked, file: and git globals do, and
/// `npm install -g <name>@latest` would replace them with a registry package of the same name.
pub fn npm_globals(output: &str) -> Vec<UpdatableItem> {
    let Some(Value::Object(object)) = json_in(output, '{', '}') else {
        return Vec::new();
    };
    let Some(deps) = object.get("dependencies").and_then(Value::as_object) else {
        return Vec::new();
    };
    let mut names: Vec<&String> = deps.keys().collect();
    names.sort();
    names
        .into_iter()
        .filter(|name| {
            !["npm", "corepack"].contains(&name.as_str()) && deps[*name].get("resolved").is_none()
        })
        .map(|name| {
            let version = deps[name]
                .get("version")
                .and_then(Value::as_str)
                .unwrap_or("");
            let detail = if version.is_empty() {
                String::new()
            } else {
                format!("v{version}")
            };
            UpdatableItem::new(SourceKind::Npm, name, &detail, None)
        })
        .collect()
}

/// One item for a CLI on PATH, its path resolved through symlinks (which tells how it was installed).
fn discover_cli(
    kind: SourceKind,
    command: &str,
    name: &str,
    version: Option<String>,
) -> Vec<UpdatableItem> {
    let which = run(&format!("command -v {command}"), 30);
    if !which.ok() || which.output.is_empty() {
        return Vec::new();
    }
    let detail = version.map_or("Version unknown".into(), |v| format!("v{v}"));
    let mut item = UpdatableItem::new(kind, name, &detail, None);
    item.path = Some(
        std::fs::canonicalize(&which.output)
            .map_or(which.output.clone(), |p| p.to_string_lossy().into()),
    );
    vec![item]
}

pub fn discover_claude_code() -> Vec<UpdatableItem> {
    discover_cli(
        SourceKind::ClaudeCode,
        "claude",
        "claude-code",
        claude_version(),
    )
}

pub fn discover_codex_cli() -> Vec<UpdatableItem> {
    discover_cli(SourceKind::CodexCli, "codex", "codex", codex_version())
}

pub fn discover_antigravity_cli() -> Vec<UpdatableItem> {
    discover_cli(SourceKind::AntigravityCli, "agy", "agy", agy_version())
}

pub fn claude_version() -> Option<String> {
    let result = run("claude --version", 30);
    result
        .ok()
        .then(|| claude_version_in(&result.output))
        .flatten()
}

/// `claude --version` prints "2.1.292 (Claude Code)".
pub fn claude_version_in(output: &str) -> Option<String> {
    output.split_whitespace().next().map(String::from)
}

pub fn codex_version() -> Option<String> {
    let result = run("codex --version", 30);
    result
        .ok()
        .then(|| codex_version_in(&result.output))
        .flatten()
}

/// `codex --version` prints "codex-cli 0.163.0". Output is stdout and stderr together, so skip any Node or
/// profile warnings.
pub fn codex_version_in(output: &str) -> Option<String> {
    let line = split_lines(output)
        .into_iter()
        .find(|l| l.starts_with("codex-cli"))?;
    line.split_whitespace().last().map(String::from)
}

pub fn agy_version() -> Option<String> {
    let result = run("agy --version", 30);
    result
        .ok()
        .then(|| version_line_in(&result.output))
        .flatten()
}

/// The first line that is a plain version: `agy --version` prints "1.4.0", but its Go logger can add
/// "ERROR: logging before google.Init" lines; the Claude release server answers with a bare version too.
pub fn version_line_in(output: &str) -> Option<String> {
    split_lines(output)
        .into_iter()
        .map(str::trim)
        .find(|l| {
            l.starts_with(|c: char| c.is_ascii_digit())
                && l.chars().all(|c| c.is_ascii_digit() || c == '.')
        })
        .map(String::from)
}

// Check

pub fn check_uv(items: &[UpdatableItem]) -> Statuses {
    let result = run("uv tool list --outdated --show-version-specifiers", 180);
    if !result.ok() {
        return same_status(
            items,
            unknown("Can't check: update uv to get `uv tool list --outdated`"),
        );
    }
    uv_statuses(&result.output, items)
}

pub fn uv_statuses(output: &str, items: &[UpdatableItem]) -> Statuses {
    let mut latest: HashMap<&str, String> = HashMap::new();
    let mut pinned: HashMap<&str, String> = HashMap::new();
    for line in split_lines(output) {
        let Some((_, after)) = line.split_once("[latest: ") else {
            continue;
        };
        let name = words(line).first().copied().unwrap_or("");
        let version = after.split(']').next().unwrap_or("");
        latest.insert(
            name,
            if version.starts_with('v') {
                version.into()
            } else {
                format!("v{version}")
            },
        );
        if let Some(pin) = exact_pin(line) {
            pinned.insert(name, pin);
        }
    }
    items
        .iter()
        .map(|item| {
            let status = match (
                latest.get(item.name.as_str()),
                pinned.get(item.name.as_str()),
            ) {
                (Some(version), Some(pin)) => {
                    ItemStatus::Pinned(format!("Pinned to {pin} · {version} available"))
                }
                (Some(version), None) => {
                    ItemStatus::UpdateAvailable(Some(format!("{} → {version}", item.detail)))
                }
                _ => ItemStatus::UpToDate,
            };
            (item.id.clone(), status)
        })
        .collect()
}

/// `uv tool upgrade` never moves past an exact pin (installed as `tool==X`). A wildcard (`==0.3.*`)
/// or a range (`==1,<2`) can still move, so those aren't treated as pins.
pub fn exact_pin(line: &str) -> Option<String> {
    let (_, after) = line.split_once("[required: ==")?;
    let version: String = after.chars().take_while(|&c| c != ']').collect();
    (!version.is_empty() && !version.contains('*') && !version.contains(','))
        .then(|| format!("=={version}"))
}

/// graphify's skill files are copied by `graphify install`, not by upgrading the CLI, so they can
/// lag behind it. Flagging that as an update makes Update run the `graphify install` post command.
// shortcut: graphify only, since it's the one tool known to ship its skill separately; generalize if another does.
pub fn check_graphify_skill(
    mut statuses: Statuses,
    items: &[UpdatableItem],
    home: &str,
) -> Statuses {
    let version_file = format!("{home}/.claude/skills/graphify/.graphify_version");
    for item in items.iter().filter(|i| i.name == "graphifyy") {
        if statuses.get(&item.id) != Some(&ItemStatus::UpToDate) {
            continue;
        }
        let Ok(skill) = std::fs::read_to_string(&version_file) else {
            continue;
        };
        let cli = run("graphify --version", 30);
        if let Some(drift) = cli
            .ok()
            .then(|| graphify_drift(skill.trim(), &cli.output))
            .flatten()
        {
            statuses.insert(item.id.clone(), ItemStatus::UpdateAvailable(Some(drift)));
        }
    }
    statuses
}

/// The message when the skill files' version differs from `graphify --version`'s last word.
pub fn graphify_drift(skill: &str, cli_output: &str) -> Option<String> {
    let last = cli_output.split_whitespace().last()?;
    let version = last.strip_prefix('v').unwrap_or(last);
    (!skill.is_empty() && version != skill)
        .then(|| format!("Skill files are v{skill}; graphify is v{version}"))
}

pub fn check_pipx(items: &[UpdatableItem]) -> Statuses {
    each_item(items, |item| {
        let result = run(
            &format!(
                "pipx runpip {} list --outdated --format json",
                quote(&item.name)
            ),
            120,
        );
        if result.ok() {
            pipx_status(&result.output, item)
        } else {
            unknown("Couldn't check")
        }
    })
}

/// `pipx runpip <tool> list --outdated --format json` lists every outdated package in the tool's venv; only the
/// tool's own row counts.
pub fn pipx_status(output: &str, item: &UpdatableItem) -> ItemStatus {
    let Some(Value::Array(rows)) = json_in(output, '[', ']') else {
        return unknown("Couldn't check");
    };
    let target = normalize_package(&item.name);
    let own = rows
        .iter()
        .find(|r| normalize_package(r.get("name").and_then(Value::as_str).unwrap_or("")) == target);
    match own.and_then(|r| r.get("latest_version")?.as_str()) {
        Some(version) => ItemStatus::UpdateAvailable(Some(format!("{} → v{version}", item.detail))),
        None => ItemStatus::UpToDate,
    }
}

pub fn check_npm(items: &[UpdatableItem]) -> Statuses {
    // `npm outdated` exits 1 when something is outdated, so don't rely on the status.
    npm_statuses(&run("npm outdated -g --json", 180).output, items)
}

pub fn npm_statuses(output: &str, items: &[UpdatableItem]) -> Statuses {
    if output.is_empty() {
        return same_status(items, ItemStatus::UpToDate);
    }
    let Some(Value::Object(object)) = json_in(output, '{', '}') else {
        return same_status(items, unknown("Couldn't check"));
    };
    items
        .iter()
        .map(|item| {
            let latest = object
                .get(&item.name)
                .and_then(|i| i.get("latest")?.as_str())
                .map(|l| format!("v{l}"));
            let status = match latest {
                Some(latest) if latest != item.detail => {
                    ItemStatus::UpdateAvailable(Some(format!("{} → {latest}", item.detail)))
                }
                _ => ItemStatus::UpToDate,
            };
            (item.id.clone(), status)
        })
        .collect()
}

pub fn is_cask_install(item: &UpdatableItem, cask: &str) -> bool {
    item.path
        .as_deref()
        .is_some_and(|p| p.contains(&format!("/Caskroom/{cask}/")))
}

/// Installed by apt, dnf or apk: outside home, not Homebrew, not an npm package.
pub fn is_system_package(item: &UpdatableItem, home: &str) -> bool {
    let path = item.path.as_deref().unwrap_or("");
    // The path is resolved, so a symlinked home (Silverblue's /home → /var/home) must be resolved too.
    let real_home =
        std::fs::canonicalize(home).map_or(home.into(), |p| p.to_string_lossy().into_owned());
    !path.is_empty()
        && !path.starts_with(&format!("{home}/"))
        && !path.starts_with(&format!("{real_home}/"))
        && !is_cask_install(item, "claude-code")
        && !path.contains("/node_modules/")
        && !path.starts_with("/opt/homebrew/")
        && !path.starts_with("/home/linuxbrew/")
}

pub fn check_claude_code(items: &[UpdatableItem], home: &str) -> Statuses {
    items
        .iter()
        .map(|item| {
            // The cask tracks the stable channel; elsewhere `claude update` follows the user's channel.
            let latest = if is_cask_install(item, "claude-code") {
                cask_latest_version("claude-code")
            } else {
                claude_latest_version(home)
            };
            (item.id.clone(), claude_status(item, latest, home))
        })
        .collect()
}

pub fn claude_status(item: &UpdatableItem, latest: Option<String>, home: &str) -> ItemStatus {
    let installed = item.detail.trim_start_matches('v');
    match latest {
        None => unknown("Couldn't check"),
        Some(latest) if installed == latest => ItemStatus::UpToDate,
        // A channel switched to stable can trail the installed build; that's not an update.
        Some(latest) if is_newer(installed, &latest) => ItemStatus::UpToDate,
        // Shown, but never counted as an update: only root can install it.
        Some(latest) if is_system_package(item, home) => ItemStatus::Pinned(format!(
            "v{latest} available; update it with your system's package manager"
        )),
        Some(latest) => ItemStatus::UpdateAvailable(Some(format!("{} → v{latest}", item.detail))),
    }
}

pub fn check_codex_cli(items: &[UpdatableItem]) -> Statuses {
    items
        .iter()
        .map(|item| {
            (
                item.id.clone(),
                version_status(item, codex_latest_version(item)),
            )
        })
        .collect()
}

pub fn check_agy(items: &[UpdatableItem]) -> Statuses {
    let latest = agy_latest_version();
    items
        .iter()
        .map(|item| (item.id.clone(), version_status(item, latest.clone())))
        .collect()
}

fn version_status(item: &UpdatableItem, latest: Option<String>) -> ItemStatus {
    match latest {
        None => unknown("Couldn't check"),
        Some(latest) if item.detail == format!("v{latest}") => ItemStatus::UpToDate,
        Some(latest) => ItemStatus::UpdateAvailable(Some(format!("{} → v{latest}", item.detail))),
    }
}

/// A cask install follows the cask; npm and the install script both ship the npm release.
pub fn codex_latest_version(item: &UpdatableItem) -> Option<String> {
    if is_cask_install(item, "codex") {
        cask_latest_version("codex")
    } else {
        npm_latest_version("@openai/codex")
    }
}

/// What `claude update` installs: the release on the channel set in ~/.claude/settings.json (`autoUpdatesChannel`,
/// latest unless it says stable), read from the server the native installer uses.
pub fn claude_latest_version(home: &str) -> Option<String> {
    let settings =
        std::fs::read_to_string(format!("{home}/.claude/settings.json")).unwrap_or_default();
    let url = format!(
        "https://downloads.claude.ai/claude-code-releases/{}",
        claude_channel(&settings)
    );
    let release = run(&format!("curl -fsS --max-time 20 {}", quote(&url)), 30);
    release
        .ok()
        .then(|| version_line_in(&release.output))
        .flatten()
}

pub fn claude_channel(settings_json: &str) -> &'static str {
    let value: Option<Value> = serde_json::from_str(settings_json).ok();
    match value
        .as_ref()
        .and_then(|v| v.get("autoUpdatesChannel")?.as_str())
    {
        Some("stable") => "stable",
        _ => "latest",
    }
}

pub fn cask_latest_version(cask: &str) -> Option<String> {
    fetch_version(&format!("https://formulae.brew.sh/api/cask/{cask}.json"))
}

pub fn npm_latest_version(package: &str) -> Option<String> {
    fetch_version(&format!("https://registry.npmjs.org/{package}/latest"))
}

/// The version `agy update` would install, from the manifest Antigravity's installer reads.
pub fn agy_latest_version() -> Option<String> {
    let os = if cfg!(target_os = "macos") {
        "darwin"
    } else {
        "linux"
    };
    let arch = if cfg!(target_arch = "aarch64") {
        "arm64"
    } else {
        "amd64"
    };
    fetch_version(&format!(
        "https://antigravity-cli-auto-updater-974169037036.us-central1.run.app/manifests/{os}_{arch}.json"
    ))
}

fn fetch_version(url: &str) -> Option<String> {
    let api = run(&format!("curl -fsS --max-time 20 {}", quote(url)), 30);
    api.ok().then(|| version_field(&api.output)).flatten()
}

/// The non-empty `version` of a JSON object (Homebrew's cask API, npm's registry, agy's manifest).
pub fn version_field(json: &str) -> Option<String> {
    let value: Value = serde_json::from_str(json).ok()?;
    value
        .get("version")?
        .as_str()
        .filter(|v| !v.is_empty())
        .map(String::from)
}

// Update

/// Runs one package or CLI update into the transcript; returns whether it worked.
pub fn update(item: &UpdatableItem, t: &mut Transcript, home: &str) -> bool {
    let long = Duration::from_secs(600);
    let q = quote(&item.name);
    match item.kind {
        SourceKind::Uv => t.run(&format!("uv tool upgrade {q}"), long).ok(),
        SourceKind::Pipx => t.run(&format!("pipx upgrade {q}"), long).ok(),
        SourceKind::Npm => t
            .run(
                &format!("npm install -g {}", quote(&format!("{}@latest", item.name))),
                long,
            )
            .ok(),
        SourceKind::ClaudeCode if is_cask_install(item, "claude-code") => {
            let before = claude_version();
            let latest = cask_latest_version("claude-code");
            // brew can exit 0 without installing anything when its own data is stale. Only complain
            // when a newer version was actually on offer; Update Now on a current install is fine.
            let ok = t.run("brew upgrade --cask claude-code", long).ok();
            guard_changed(ok, t, before, latest, claude_version)
        }
        SourceKind::ClaudeCode if is_system_package(item, home) => {
            t.note("Claude Code was installed with your system's package manager, which needs root; update it there.");
            false
        }
        SourceKind::ClaudeCode => {
            let before = claude_version();
            // Only a newer release is on offer: one a stable channel trails with isn't expected to install.
            let latest = claude_latest_version(home)
                .filter(|l| before.as_deref().is_some_and(|b| is_newer(l, b)));
            let ok = t.run("claude update", long).ok();
            guard_changed(ok, t, before, latest, claude_version)
        }
        SourceKind::CodexCli => {
            // `codex update` picks Homebrew, npm or the install script itself.
            let before = codex_version();
            let latest = codex_latest_version(item);
            let ok = t.run("codex update", long).ok();
            guard_changed(ok, t, before, latest, codex_version)
        }
        SourceKind::AntigravityCli => {
            let before = agy_version();
            let latest = agy_latest_version();
            let ok = t.run("agy update", Duration::from_secs(300)).ok();
            guard_changed(ok, t, before, latest, agy_version)
        }
        _ => false,
    }
}

/// An updater that exits 0 but leaves the old version while a newer one is out didn't update anything.
fn guard_changed(
    ok: bool,
    t: &mut Transcript,
    before: Option<String>,
    latest: Option<String>,
    version_now: impl FnOnce() -> Option<String>,
) -> bool {
    match (ok, before, latest) {
        (true, Some(before), Some(latest)) if latest != before => {
            match still_at(&before, &latest, version_now()) {
                Some(note) => {
                    t.note(&note);
                    false
                }
                None => true,
            }
        }
        (ok, ..) => ok,
    }
}

/// The note when the version after the update is still the one before it.
pub fn still_at(before: &str, latest: &str, after: Option<String>) -> Option<String> {
    (after.as_deref() == Some(before))
        .then(|| format!("Still at {before} although {latest} is out, so nothing was installed."))
}

// Helpers

/// Whether dotted version `a` is newer than `b` (2.1.302 > 2.1.296); false if either isn't a plain version.
pub fn is_newer(a: &str, b: &str) -> bool {
    let parse = |v: &str| {
        v.split('.')
            .map(str::parse::<u64>)
            .collect::<Result<Vec<_>, _>>()
            .ok()
    };
    matches!((parse(a), parse(b)), (Some(a), Some(b)) if a > b)
}

fn words(line: &str) -> Vec<&str> {
    line.split(' ').filter(|w| !w.is_empty()).collect()
}

fn same_status(items: &[UpdatableItem], status: ItemStatus) -> Statuses {
    items
        .iter()
        .map(|i| (i.id.clone(), status.clone()))
        .collect()
}

pub fn normalize_package(name: &str) -> String {
    name.to_lowercase().replace(['_', '.'], "-")
}

/// The JSON value between the first `open` and the last `close`: output may have warnings around it.
fn json_in(text: &str, open: char, close: char) -> Option<Value> {
    let (start, end) = (text.find(open)?, text.rfind(close)?);
    (start < end)
        .then(|| serde_json::from_str(&text[start..=end]).ok())
        .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(kind: SourceKind, name: &str, detail: &str) -> UpdatableItem {
        UpdatableItem::new(kind, name, detail, None)
    }

    #[test]
    fn uv_lists_tools_not_executables() {
        let items = uv_tools("graphifyy v0.5.1\n- graphify\nspecify-cli v0.4.3\n- specify\nwarning: x\n  indented v1\n");
        let got: Vec<(&str, &str)> = items
            .iter()
            .map(|i| (i.name.as_str(), i.detail.as_str()))
            .collect();
        assert_eq!(got, [("graphifyy", "v0.5.1"), ("specify-cli", "v0.4.3")]);
        assert_eq!(items[0].id, "uv:graphifyy");
        assert_eq!(
            items[0].agents,
            [crate::models::Agent::Claude].into(),
            "graphify's skill is Claude's"
        );
    }

    #[test]
    fn uv_outdated_pins_and_ranges() {
        // The specify-cli line is from a real `uv tool list --outdated --show-version-specifiers`.
        let output = "graphifyy v0.5.1 [latest: 0.6.0]\n- graphify\n\
                      pinned v1.0.0 [required: ==1.0.0] [latest: 1.2.0]\n\
                      wild v0.3.1 [required: ==0.3.*] [latest: 0.4.0]\n\
                      range v1.0.0 [required: ==1,<2] [latest: 1.1.0]\n\
                      specify-cli v0.4.3 [required:  git+https://github.com/github/spec-kit.git@v0.4.3] [latest: 1.2.1]\n";
        let items: Vec<_> = [
            "graphifyy",
            "pinned",
            "wild",
            "range",
            "specify-cli",
            "current",
        ]
        .map(|n| {
            item(
                SourceKind::Uv,
                n,
                if n == "graphifyy" { "v0.5.1" } else { "v1.0.0" },
            )
        })
        .into();
        let s = uv_statuses(output, &items);
        assert_eq!(
            s["uv:graphifyy"],
            ItemStatus::UpdateAvailable(Some("v0.5.1 → v0.6.0".into()))
        );
        assert_eq!(
            s["uv:pinned"],
            ItemStatus::Pinned("Pinned to ==1.0.0 · v1.2.0 available".into())
        );
        assert!(
            s["uv:wild"].is_update_available() && s["uv:range"].is_update_available(),
            "only exact pins are pins"
        );
        assert!(s["uv:specify-cli"].is_update_available(), "as in Swift");
        assert_eq!(s["uv:current"], ItemStatus::UpToDate);
    }

    #[test]
    fn graphify_skill_drift() {
        assert_eq!(
            graphify_drift("0.5.0", "graphify v0.5.1"),
            Some("Skill files are v0.5.0; graphify is v0.5.1".into())
        );
        assert_eq!(graphify_drift("0.5.1", "graphify 0.5.1\n"), None);
        assert_eq!(graphify_drift("", "graphify 0.5.1"), None);
        assert_eq!(
            default_post_commands()["pipx:graphifyy"],
            "graphify install"
        );
    }

    #[test]
    fn pipx_lists_and_checks_its_own_row() {
        let items = pipx_tools("black 24.1.0\nGraphify_Y 0.5.1\n\n");
        assert_eq!(
            items
                .iter()
                .map(|i| (i.id.as_str(), i.detail.as_str()))
                .collect::<Vec<_>>(),
            [("pipx:black", "v24.1.0"), ("pipx:Graphify_Y", "v0.5.1")]
        );
        let rows = r#"[{"name":"click","version":"8.0","latest_version":"8.1"},{"name":"graphify-y","version":"0.5.1","latest_version":"0.6.0"}]"#;
        assert_eq!(
            pipx_status(rows, &items[1]),
            ItemStatus::UpdateAvailable(Some("v0.5.1 → v0.6.0".into()))
        );
        assert_eq!(
            pipx_status(rows, &items[0]),
            ItemStatus::UpToDate,
            "a dependency's update isn't the tool's"
        );
        assert_eq!(
            pipx_status("pipx: error", &items[0]),
            unknown("Couldn't check")
        );
    }

    #[test]
    fn npm_skips_its_own_and_non_registry_globals() {
        // Shaped like trq-linux's real `npm ls -g --depth=0 --json`, plus a linked and a git global.
        let ls = r#"npm warn x
{"name": "lib", "dependencies": {
  "npm": {"version": "10.9.9"}, "corepack": {"version": "0.36.0"},
  "@anthropic-ai/claude-code": {"version": "2.1.81", "overridden": false},
  "mine": {"version": "1.0.0", "resolved": "file:../../src/mine"},
  "fromgit": {"version": "2.0.0", "resolved": "git+ssh://git@github.com/o/r.git#abc"},
  "noversion": {}
}}"#;
        let items = npm_globals(ls);
        assert_eq!(
            items
                .iter()
                .map(|i| (i.id.as_str(), i.detail.as_str()))
                .collect::<Vec<_>>(),
            [
                ("npm:@anthropic-ai/claude-code", "v2.1.81"),
                ("npm:noversion", "")
            ]
        );
        let s = npm_statuses(
            r#"{"@anthropic-ai/claude-code": {"current": "2.1.81", "latest": "2.1.296"}}"#,
            &items,
        );
        assert_eq!(
            s["npm:@anthropic-ai/claude-code"],
            ItemStatus::UpdateAvailable(Some("v2.1.81 → v2.1.296".into()))
        );
        assert_eq!(s["npm:noversion"], ItemStatus::UpToDate);
        assert_eq!(
            npm_statuses("", &items)["npm:noversion"],
            ItemStatus::UpToDate,
            "no output: nothing outdated"
        );
        assert_eq!(
            npm_statuses("npm error", &items)["npm:noversion"],
            unknown("Couldn't check")
        );
    }

    #[test]
    fn cli_versions_past_noise() {
        assert_eq!(
            claude_version_in("2.1.296 (Claude Code)").as_deref(),
            Some("2.1.296")
        );
        assert_eq!(
            codex_version_in("(node:1) Warning: x\ncodex-cli 0.163.0").as_deref(),
            Some("0.163.0")
        );
        assert_eq!(codex_version_in("command not found"), None);
        assert_eq!(
            version_line_in("ERROR: logging before google.Init: I1016 cli.go:27] x\n1.4.0")
                .as_deref(),
            Some("1.4.0")
        );
        assert_eq!(version_line_in("v1.4.0\n1.4"), Some("1.4".into()));
        assert_eq!(
            version_field(r#"{"version": "1.4.0", "url": "x"}"#).as_deref(),
            Some("1.4.0")
        );
        assert_eq!(version_field(r#"{"version": ""}"#), None);
        assert_eq!(
            claude_channel(r#"{"autoUpdatesChannel": "stable", "model": "x"}"#),
            "stable"
        );
        assert_eq!(
            (
                claude_channel(r#"{"autoUpdatesChannel": "latest"}"#),
                claude_channel("")
            ),
            ("latest", "latest")
        );
        assert!(
            is_newer("2.1.302", "2.1.296")
                && is_newer("2.10.0", "2.9.9")
                && is_newer("1.0.1", "1.0")
        );
        assert!(
            !is_newer("2.1.292", "2.1.296")
                && !is_newer("2.1.296", "2.1.296")
                && !is_newer("2.x", "1")
        );
    }

    #[test]
    fn how_claude_was_installed() {
        let at = |path: &str| {
            let mut i = item(SourceKind::ClaudeCode, "claude-code", "v1");
            i.path = Some(path.into());
            i
        };
        let home = "/home/u";
        assert!(
            !is_system_package(&at("/home/u/.local/share/claude/versions/2.1.296"), home),
            "native"
        );
        assert!(
            !is_system_package(
                &at("/usr/lib/node_modules/@anthropic-ai/claude-code/cli.js"),
                home
            ),
            "npm"
        );
        assert!(
            !is_system_package(&at("/home/linuxbrew/.linuxbrew/bin/claude"), home),
            "linuxbrew"
        );
        assert!(
            is_system_package(&at("/usr/bin/claude"), home),
            "apt, dnf, apk"
        );
        assert!(is_cask_install(
            &at("/opt/homebrew/Caskroom/claude-code/2.1/claude"),
            "claude-code"
        ));
    }

    #[test]
    fn claude_statuses() {
        let at = |path: &str, detail: &str| {
            let mut i = item(SourceKind::ClaudeCode, "claude-code", detail);
            i.path = Some(path.into());
            i
        };
        let native = at("/home/u/.local/share/claude/versions/2.1.296", "v2.1.296");
        let s = |i: &UpdatableItem, latest: &str| claude_status(i, Some(latest.into()), "/home/u");
        assert_eq!(
            s(&native, "2.1.302"),
            ItemStatus::UpdateAvailable(Some("v2.1.296 → v2.1.302".into()))
        );
        assert_eq!(s(&native, "2.1.296"), ItemStatus::UpToDate);
        assert_eq!(
            s(&native, "2.1.292"),
            ItemStatus::UpToDate,
            "a stable channel behind the install"
        );
        let unknown_version = at("/home/u/.local/share/claude/versions/x", "Version unknown");
        assert!(
            s(&unknown_version, "2.1.302").is_update_available(),
            "an unreadable version isn't current"
        );
        let apt = at("/usr/bin/claude", "v2.1.296");
        assert!(
            matches!(s(&apt, "2.1.302"), ItemStatus::Pinned(_)),
            "only root can update a system package"
        );
        assert_eq!(
            claude_status(&native, None, "/home/u"),
            unknown("Couldn't check")
        );
    }

    #[test]
    fn a_symlinked_home_is_still_home() {
        let tmp = std::env::temp_dir().join(format!("pullday-tools-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("var/home/u")).unwrap();
        std::os::unix::fs::symlink(tmp.join("var/home"), tmp.join("home")).unwrap();
        let home = tmp.join("home/u").to_string_lossy().into_owned();
        // Resolved, as discovery resolves it (the temp folder itself may be behind a symlink, as on macOS).
        let real = std::fs::canonicalize(tmp.join("var/home/u"))
            .unwrap()
            .join(".local/share/claude/versions/2.1.296")
            .to_string_lossy()
            .into_owned();
        let mut native = item(SourceKind::ClaudeCode, "claude-code", "v2.1.296");
        native.path = Some(real);
        assert!(!is_system_package(&native, &home));
        std::fs::remove_dir_all(&tmp).unwrap();
    }

    #[test]
    fn an_updater_that_changed_nothing_fails() {
        assert_eq!(
            still_at("1.4.0", "1.5.0", Some("1.4.0".into())).as_deref(),
            Some("Still at 1.4.0 although 1.5.0 is out, so nothing was installed.")
        );
        assert_eq!(still_at("1.4.0", "1.5.0", Some("1.5.0".into())), None);
        let mut t = Transcript::default();
        assert!(!guard_changed(
            true,
            &mut t,
            Some("1.4.0".into()),
            Some("1.5.0".into()),
            || Some("1.4.0".into())
        ));
        assert!(guard_changed(
            true,
            &mut t,
            Some("1.5.0".into()),
            Some("1.5.0".into()),
            || panic!("not asked")
        ));
        assert!(guard_changed(
            true,
            &mut t,
            None,
            Some("1.5.0".into()),
            || panic!("not asked")
        ));
        assert!(!guard_changed(
            false,
            &mut t,
            Some("1.4.0".into()),
            Some("1.5.0".into()),
            || panic!("not asked")
        ));
    }
}
