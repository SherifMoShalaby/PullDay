//! Port of `Sources/Scanner.swift`: discover, check and update per source kind. The git-backed sources live here
//! (Claude Code plugin marketplaces as git clones and `.gcs-sha` snapshots, git-cloned skills, Codex marketplaces,
//! Gemini CLI extensions, Antigravity plugins); the package and CLI sources are in `tools`.

use crate::models::{Agent, ItemStatus, SourceKind, UpdatableItem, UpdateResult};
use crate::shell::{self, quote, ShellResult, Transcript};
use crate::tools;
use serde_json::{Map, Value};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Every git call goes through this: no repo hooks or fsmonitor, and no local-path or `file://` remotes.
pub const GIT: &str =
    "git -c core.hooksPath=/dev/null -c core.fsmonitor=false -c protocol.file.allow=never";
pub const GITHUB_API: &str = "https://api.github.com";

pub type Statuses = HashMap<String, ItemStatus>;
type Json = Map<String, Value>;

fn run(command: &str, seconds: u64) -> ShellResult {
    shell::run(command, Duration::from_secs(seconds))
}

fn unknown(why: &str) -> ItemStatus {
    ItemStatus::Unknown(Some(why.into()))
}

pub struct Scanner {
    pub home: String,
}

impl Scanner {
    pub fn new(home: impl Into<String>) -> Self {
        Scanner { home: home.into() }
    }

    fn path(&self, relative: &str) -> String {
        format!("{}/{relative}", self.home)
    }

    /// The folders whose entries the app watches for a quick rescan (Swift `AppState.watchFolders()`): every skill
    /// root, the Claude marketplaces, and the Codex / Gemini / Antigravity folders only while their kind is on
    /// (Codex upgrades churn through a .staging folder).
    pub fn watched_dirs(&self, enabled_kinds: &BTreeSet<String>) -> Vec<String> {
        let optional = [
            (SourceKind::CodexPlugin, ".codex/.tmp/marketplaces"),
            (SourceKind::GeminiExtension, ".gemini/extensions"),
            (SourceKind::AntigravityPlugin, ".gemini/config/plugins"),
        ]
        .into_iter()
        .filter(|(kind, _)| enabled_kinds.contains(kind.id()))
        .map(|(_, dir)| dir);
        Agent::skill_root_readers()
            .into_iter()
            .map(|(root, _)| root)
            .chain([".claude/plugins/marketplaces"])
            .chain(optional)
            .map(|dir| self.path(dir))
            .collect()
    }

    // Discover

    pub fn discover(&self, kind: SourceKind) -> Vec<UpdatableItem> {
        match kind {
            SourceKind::Plugin => self.discover_plugins(),
            SourceKind::GitSkill => self.discover_git_skills(),
            SourceKind::CodexPlugin => discover_codex_plugins(
                &self.path(".codex/.tmp/marketplaces"),
                &self.path(".codex/plugins/cache"),
            ),
            SourceKind::GeminiExtension => {
                discover_gemini_extensions(&self.path(".gemini/extensions"))
            }
            SourceKind::AntigravityPlugin => discover_antigravity_plugins(),
            SourceKind::Uv => tools::discover_uv(),
            SourceKind::Pipx => tools::discover_pipx(),
            SourceKind::Npm => tools::discover_npm(),
            SourceKind::ClaudeCode => tools::discover_claude_code(),
            SourceKind::CodexCli => tools::discover_codex_cli(),
            SourceKind::AntigravityCli => tools::discover_antigravity_cli(),
        }
    }

    pub fn discover_plugins(&self) -> Vec<UpdatableItem> {
        let dir = self.path(".claude/plugins/marketplaces");
        let Some(entries) = list_dir(&dir) else {
            return Vec::new();
        };
        let installed = self.installed_plugins();
        entries
            .into_iter()
            .filter_map(|name| {
                let path = format!("{dir}/{name}");
                if name.starts_with('.') || !is_dir(&path) {
                    return None;
                }
                let plugins = installed.get(&name).cloned().unwrap_or_default();
                let names: Vec<&str> = plugins
                    .iter()
                    .map(|key| key.rsplit_once('@').map_or(key.as_str(), |(n, _)| n))
                    .collect();
                let detail = if names.is_empty() {
                    "Marketplace (no plugins installed)".into()
                } else {
                    names.join(", ")
                };
                let mut item = UpdatableItem::new(SourceKind::Plugin, &name, &detail, None);
                item.path = Some(path);
                item.plugins = plugins;
                Some(item)
            })
            .collect()
    }

    /// Reads ~/.claude/plugins/installed_plugins.json → {marketplace: ["plugin@marketplace", …]}.
    pub fn installed_plugins(&self) -> HashMap<String, Vec<String>> {
        let Some(object) = read_json(&self.path(".claude/plugins/installed_plugins.json")) else {
            return HashMap::new();
        };
        let container = object
            .get("plugins")
            .and_then(Value::as_object)
            .unwrap_or(&object);
        let mut result: HashMap<String, Vec<String>> = HashMap::new();
        for key in container.keys() {
            if let Some((_, marketplace)) = key.rsplit_once('@').filter(|(_, m)| !m.is_empty()) {
                result
                    .entry(marketplace.into())
                    .or_default()
                    .push(key.clone());
            }
        }
        result.values_mut().for_each(|v| v.sort());
        result
    }

    /// One item per repo; its agents are the readers of every root it was found in.
    pub fn discover_git_skills(&self) -> Vec<UpdatableItem> {
        let roots: Vec<(String, BTreeSet<Agent>)> = Agent::skill_root_readers()
            .into_iter()
            .map(|(root, agents)| (self.path(root), agents))
            .collect();
        let root_paths: Vec<String> = roots.iter().map(|(p, _)| p.clone()).collect();
        // The agents' own folders (each root's parent), as written and resolved, so a symlinked home still counts.
        // The parent is resolved, not the root: a symlinked skills folder must not widen it to its target's parent.
        let agent_dirs: Vec<String> = root_paths
            .iter()
            .flat_map(|r| [parent(r), resolve(&parent(r))])
            .collect();
        let mut items: Vec<UpdatableItem> = Vec::new();
        let mut index_by_repo: HashMap<String, usize> = HashMap::new();
        for (root, agents) in &roots {
            let Some(entries) = list_dir(root) else {
                continue;
            };
            for entry in entries.iter().filter(|e| !e.starts_with('.')) {
                let resolved = resolve(&format!("{root}/{entry}"));
                if !is_dir(&resolved) {
                    continue;
                }
                let Some(repo) = git_root(&resolved, &root_paths) else {
                    continue;
                };
                if let Some(&i) = index_by_repo.get(&repo) {
                    items[i].agents.extend(agents.iter().copied());
                    continue;
                }
                let name = Path::new(&repo)
                    .file_name()
                    .map_or(repo.clone(), |n| n.to_string_lossy().into());
                // A symlink into a repo outside every agent's folder is likely the user's own project.
                let external = !agent_dirs
                    .iter()
                    .any(|d| repo.starts_with(&format!("{d}/")));
                let detail = self.tilde(&repo)
                    + if external {
                        " · manual updates only"
                    } else {
                        ""
                    };
                let mut item = UpdatableItem::new(
                    SourceKind::GitSkill,
                    &name,
                    &detail,
                    Some(format!("gitSkill:{repo}")),
                );
                item.path = Some(repo.clone());
                item.manual_only = external;
                item.agents = agents.clone();
                index_by_repo.insert(repo, items.len());
                items.push(item);
            }
        }
        items
    }

    // Check

    pub fn check(&self, kind: SourceKind, items: &[UpdatableItem]) -> Statuses {
        match kind {
            SourceKind::Plugin | SourceKind::GitSkill => {
                each_item(items, |i| self.git_status(i.path.as_deref()))
            }
            SourceKind::CodexPlugin => each_item(items, |i| {
                codex_marketplace_status(i.path.as_deref().unwrap_or(""))
            }),
            SourceKind::GeminiExtension => each_item(items, |i| {
                gemini_extension_status(i.path.as_deref().unwrap_or(""))
            }),
            SourceKind::AntigravityPlugin => each_item(items, |i| {
                self.antigravity_plugin_status(i.path.as_deref().unwrap_or(""))
            }),
            SourceKind::Uv => {
                tools::check_graphify_skill(tools::check_uv(items), items, &self.home)
            }
            SourceKind::Pipx => {
                tools::check_graphify_skill(tools::check_pipx(items), items, &self.home)
            }
            SourceKind::Npm => tools::check_npm(items),
            SourceKind::ClaudeCode => tools::check_claude_code(items, &self.home),
            SourceKind::CodexCli => tools::check_codex_cli(items),
            SourceKind::AntigravityCli => tools::check_agy(items),
        }
    }

    pub fn git_status(&self, path: Option<&str>) -> ItemStatus {
        let Some(path) = path.filter(|p| Path::new(&format!("{p}/.git")).exists()) else {
            if let Some(status) = path.and_then(|p| self.snapshot_status(p)) {
                return status;
            }
            return unknown("Not a git checkout");
        };
        let q = quote(path);
        let fetch = run(&format!("{GIT} -C {q} fetch --quiet"), 90);
        if !fetch.ok() {
            return ItemStatus::Failed(fetch.last_line("git fetch failed"));
        }
        let count = run(
            &format!("{GIT} -C {q} rev-list --count 'HEAD..@{{u}}'"),
            300,
        );
        let Some(n) = count
            .ok()
            .then(|| count.output.parse::<u64>().ok())
            .flatten()
        else {
            return ItemStatus::Failed(count.last_line("No upstream branch"));
        };
        if n == 0 {
            return ItemStatus::UpToDate;
        }
        // First line is the summary; the commit subjects after it show what the pull would bring in.
        let mut info = format!("{n} new commit{}", if n == 1 { "" } else { "s" });
        let log = run(
            &format!("{GIT} -C {q} log --oneline --no-decorate -n 5 'HEAD..@{{u}}'"),
            300,
        );
        if log.ok() && !log.output.is_empty() {
            info += &format!("\n{}", log.output);
        }
        ItemStatus::UpdateAvailable(Some(info))
    }

    /// Official marketplaces are installed as snapshots, not git clones: `.gcs-sha` holds the
    /// source commit, and known_marketplaces.json names the GitHub repo it came from.
    pub fn snapshot_status(&self, path: &str) -> Option<ItemStatus> {
        let sha = std::fs::read_to_string(format!("{path}/.gcs-sha"))
            .ok()?
            .trim()
            .to_string();
        if sha.is_empty() {
            return None;
        }
        let name = Path::new(path).file_name()?.to_string_lossy().to_string();
        let known = read_json(&self.path(".claude/plugins/known_marketplaces.json"));
        let source = known
            .as_ref()
            .and_then(|all| all.get(&name)?.get("source")?.as_object());
        let github = source.filter(|s| str_of(s, "source") == Some("github"));
        let Some(repo) = github.and_then(|s| str_of(s, "repo")) else {
            return Some(unknown("Snapshot install; source repo unknown"));
        };
        let git_ref = github.and_then(|s| str_of(s, "ref")).unwrap_or("HEAD");
        Some(remote_head_status(
            &format!("https://github.com/{repo}.git"),
            git_ref,
            &sha,
            |s| format!("New version on GitHub ({s})"),
        ))
    }

    /// A plugin installed from a git repo is a checkout of it, so the usual git check applies. Others (a marketplace,
    /// a local folder) can't be checked from here.
    pub fn antigravity_plugin_status(&self, path: &str) -> ItemStatus {
        if !Path::new(&format!("{path}/.git")).exists() {
            return unknown("Not installed from a git repo");
        }
        self.git_status(Some(path))
    }

    // Update

    pub fn update(&self, item: &UpdatableItem, post_command: Option<&str>) -> UpdateResult {
        let mut t = Transcript::default();
        let long = Duration::from_secs(600);
        let q = quote(&item.name);
        let mut ok = false;

        match item.kind {
            // The name comes from a folder or a manifest; one like `--all` would be read as an option.
            _ if item.name.starts_with('-') => t.note(&format!(
                "PullDay won't pass the name {} to a command.",
                item.name
            )),
            SourceKind::Plugin => {
                if run("command -v claude", 300).ok() {
                    ok = t
                        .run(&format!("claude plugin marketplace update {q}"), long)
                        .ok();
                    if ok {
                        for plugin in &item.plugins {
                            if !t
                                .run(&format!("claude plugin update {}", quote(plugin)), long)
                                .ok()
                            {
                                ok = false;
                            }
                        }
                    }
                } else if let Some(path) = &item.path {
                    t.note("`claude` wasn't found on PATH, so only the marketplace checkout was pulled.");
                    ok = t
                        .run(&format!("{GIT} -C {} pull --ff-only", quote(path)), long)
                        .ok();
                } else {
                    t.note("`claude` wasn't found on PATH.");
                }
            }
            SourceKind::GitSkill => {
                if let Some(path) = &item.path {
                    ok = t
                        .run(&format!("{GIT} -C {} pull --ff-only", quote(path)), long)
                        .ok();
                }
            }
            SourceKind::CodexPlugin => {
                ok = t
                    .run(&format!("codex plugin marketplace upgrade {q}"), long)
                    .ok();
            }
            SourceKind::GeminiExtension => {
                // Checked here, not read from item.status: the caller has already marked the item updating.
                let before = item.path.as_deref().map(gemini_extension_status);
                ok = t
                    .run(
                        &format!("gemini extensions update {q}"),
                        Duration::from_secs(300),
                    )
                    .ok();
                // `gemini extensions update` exits 0 even when it fails or a consent prompt ([Y/n]) got no answer
                // (stdin is closed), so only a re-check shows whether anything was installed.
                if let (true, Some(path), Some(ItemStatus::UpdateAvailable(offer))) =
                    (ok, &item.path, before)
                {
                    match gemini_extension_status(path) {
                        ItemStatus::UpToDate => {}
                        ItemStatus::UpdateAvailable(now) if now != offer => {}
                        ItemStatus::UpdateAvailable(_) => {
                            t.note(&format!(
                                "Gemini CLI didn't install it; it may need your consent. Run `gemini extensions update {}` in a terminal.",
                                item.name
                            ));
                            ok = false;
                        }
                        _ => {
                            t.note("Couldn't confirm the update; check `gemini extensions list`.");
                            ok = false;
                        }
                    }
                }
            }
            SourceKind::AntigravityPlugin => {
                // Installing again over the existing checkout updates it in place and keeps its enabled state;
                // uninstalling first would lose the plugin if the install then failed.
                let install_url = item.path.as_deref().and_then(|path| {
                    let origin = run(
                        &format!("{GIT} -C {} remote get-url origin", quote(path)),
                        30,
                    );
                    origin
                        .ok()
                        .then(|| antigravity_install_url(&origin.output))
                        .flatten()
                });
                match (&item.path, install_url) {
                    (Some(path), Some(url)) => {
                        ok = t
                            .run(
                                &format!("agy plugin install {}", quote(&url)),
                                Duration::from_secs(300),
                            )
                            .ok();
                        if ok && self.antigravity_plugin_status(path).is_update_available() {
                            t.note(&format!(
                                "agy finished, but {} is still behind its repo.",
                                item.name
                            ));
                            ok = false;
                        }
                    }
                    _ => t.note(&format!(
                        "{} wasn't installed from a git repo PullDay can reinstall from.",
                        item.name
                    )),
                }
            }
            _ => ok = tools::update(item, &mut t, &self.home),
        }

        if let Some(command) = post_command.map(str::trim).filter(|c| ok && !c.is_empty()) {
            if !t.run(command, long).ok() {
                return UpdateResult {
                    ok: false,
                    summary: "Updated, but the post-update command failed".into(),
                    output: t.text(),
                };
            }
        }

        let summary = if ok {
            match item.kind {
                SourceKind::Plugin => "Updated. Restart Claude Code to load it",
                SourceKind::CodexPlugin => "Updated. Restart Codex to load it",
                SourceKind::GeminiExtension => "Updated. Restart Gemini CLI to load it",
                SourceKind::AntigravityPlugin => "Updated. Restart Antigravity to load it",
                SourceKind::ClaudeCode | SourceKind::CodexCli | SourceKind::AntigravityCli => {
                    "Updated. New terminal sessions use it"
                }
                _ => "Updated",
            }
            .to_string()
        } else {
            t.last_failure
                .clone()
                .unwrap_or_else(|| "Update failed".into())
        };
        UpdateResult {
            ok,
            summary,
            output: t.text(),
        }
    }

    /// Paths found through symlinks are resolved, so the resolved home counts too (macOS's /var → /private/var,
    /// Silverblue's /home → /var/home).
    pub fn tilde(&self, path: &str) -> String {
        let real = resolve(&self.home);
        [self.home.as_str(), real.as_str()]
            .iter()
            .find_map(|home| path.strip_prefix(home))
            .map_or(path.into(), |rest| format!("~{rest}"))
    }
}

/// Checks every item at the same time.
pub fn each_item(
    items: &[UpdatableItem],
    status: impl Fn(&UpdatableItem) -> ItemStatus + Sync,
) -> Statuses {
    let status = &status;
    std::thread::scope(|s| {
        let running: Vec<_> = items
            .iter()
            .map(|item| s.spawn(move || (item.id.clone(), status(item))))
            .collect();
        running
            .into_iter()
            .map(|h| h.join().expect("a check panicked"))
            .collect()
    })
}

/// Walks up from a skill folder to the git repo that contains it, never treating a skill root
/// or any of its ancestors (home, ~/.claude, ~/.config, …) as the repo.
pub fn git_root(path: &str, roots: &[String]) -> Option<String> {
    let mut blocked: HashSet<PathBuf> = HashSet::from(["/".into()]);
    // Resolved roots too: entries are resolved, so a stow-style symlinked root must block its real ancestors.
    for root in roots
        .iter()
        .flat_map(|r| [PathBuf::from(r), PathBuf::from(resolve(r))])
    {
        let mut p: PathBuf = root.components().collect();
        while p != Path::new("/") {
            blocked.insert(p.clone());
            if !p.pop() {
                break;
            }
        }
    }
    let mut current: PathBuf = Path::new(path).components().collect();
    for _ in 0..8 {
        if blocked.contains(&current) {
            return None;
        }
        if current.join(".git").exists() {
            return Some(current.to_string_lossy().into());
        }
        current.pop();
    }
    None
}

/// Compares a local commit with what `ref` (branch, tag or HEAD) points to upstream.
/// `offer` builds the message from the new short SHA, which makes each upstream version a new offer
/// (see Skip This Update).
pub fn remote_head_status(
    url: &str,
    git_ref: &str,
    local_sha: &str,
    offer: impl Fn(&str) -> String,
) -> ItemStatus {
    // A ref pinned to a commit can't move, so there's nothing to look up.
    if is_commit_sha(git_ref) {
        return if git_ref == local_sha {
            ItemStatus::UpToDate
        } else {
            ItemStatus::UpdateAvailable(Some(format!("Pinned commit {}", &git_ref[..7])))
        };
    }
    // A plain pattern doesn't list the peeled `^{}` line, so ask for it too.
    let patterns = if git_ref == "HEAD" {
        "HEAD".into()
    } else {
        format!("{} {}", quote(git_ref), quote(&format!("{git_ref}^{{}}")))
    };
    // `--` ends git's options, so a URL from an install record or a remote is never read as `--upload-pack=…`
    // (protocol.file.allow=never in GIT already blocks the local form of that; this closes the rest).
    let remote = run(&format!("{GIT} ls-remote -- {} {patterns}", quote(url)), 60);
    if !remote.ok() {
        return ItemStatus::Failed(remote.last_line("git ls-remote failed"));
    }
    let Some(head) = pick_remote_sha(&remote.output, git_ref) else {
        return ItemStatus::Failed(format!("git ls-remote found no {git_ref}"));
    };
    if head == local_sha {
        ItemStatus::UpToDate
    } else {
        ItemStatus::UpdateAvailable(Some(offer(&head.chars().take(7).collect::<String>())))
    }
}

/// Picks `ref`'s commit from `git ls-remote <url> <ref>` output, which also lists any ref merely ending in
/// the name (refs/heads/x/main). Order follows git's own: a tag (peeled `^{}` first, so an annotated tag
/// gives its commit), then a branch.
pub fn pick_remote_sha(output: &str, git_ref: &str) -> Option<String> {
    let shas: HashMap<&str, &str> = output
        .lines()
        .filter_map(
            |line| match line.split_whitespace().collect::<Vec<_>>()[..] {
                [sha, name] => Some((name, sha)),
                _ => None,
            },
        )
        .collect();
    let candidates = if git_ref == "HEAD" {
        vec!["HEAD".to_string()]
    } else {
        // The last two: a fully qualified ref.
        vec![
            format!("refs/tags/{git_ref}^{{}}"),
            format!("refs/tags/{git_ref}"),
            format!("refs/heads/{git_ref}"),
            format!("{git_ref}^{{}}"),
            git_ref.to_string(),
        ]
    };
    candidates
        .iter()
        .find_map(|c| shas.get(c.as_str()).map(|s| s.to_string()))
}

pub fn is_commit_sha(git_ref: &str) -> bool {
    git_ref.len() == 40 && git_ref.chars().all(|c| c.is_ascii_hexdigit())
}

/// Codex keeps each marketplace as a snapshot whose `.codex-marketplace-install.json` records where it came from.
pub fn discover_codex_plugins(dir: &str, cache_dir: &str) -> Vec<UpdatableItem> {
    let Some(entries) = list_dir(dir) else {
        return Vec::new();
    };
    entries
        .into_iter()
        .filter_map(|name| {
            let path = format!("{dir}/{name}");
            if name.starts_with('.') || !is_dir(&path) {
                return None; // skips .staging
            }
            let plugins: Vec<String> = list_dir(&format!("{cache_dir}/{name}"))
                .unwrap_or_default()
                .into_iter()
                .filter(|p| !p.starts_with('.'))
                .collect();
            let detail = if plugins.is_empty() {
                "Marketplace (no plugins installed)".into()
            } else {
                plugins.join(", ")
            };
            let mut item = UpdatableItem::new(SourceKind::CodexPlugin, &name, &detail, None);
            item.path = Some(path);
            Some(item)
        })
        .collect()
}

/// The install record is `{source_type, source, ref_name, sparse_paths, revision}`; only git sources can move.
pub fn codex_marketplace_status(path: &str) -> ItemStatus {
    let Some(record) = read_json(&format!("{path}/.codex-marketplace-install.json")) else {
        return unknown("No install record");
    };
    let source = str_of(&record, "source").filter(|s| !s.is_empty());
    let Some(source) = source.filter(|_| str_of(&record, "source_type") == Some("git")) else {
        return unknown("Local marketplace; nothing to update");
    };
    let Some(revision) = str_of(&record, "revision").filter(|r| !r.is_empty()) else {
        return unknown("Installed revision unknown");
    };
    let git_ref = str_of(&record, "ref_name")
        .filter(|r| !r.is_empty())
        .unwrap_or("HEAD");
    remote_head_status(&codex_source_url(source), git_ref, revision, |s| {
        format!("New version ({s})")
    })
}

/// `owner/repo` (or `github.com/owner/repo`) is GitHub shorthand; anything else (https, ssh, git@) is a URL as written.
pub fn codex_source_url(source: &str) -> String {
    if source.starts_with("github.com/") {
        return format!("https://{source}");
    }
    let parts: Vec<&str> = source.split('/').collect();
    let shorthand = parts.len() == 2
        && !source.contains(':')
        && !source.starts_with('.')
        && parts.iter().all(|p| !p.is_empty());
    if shorthand {
        format!("https://github.com/{source}.git")
    } else {
        source.into()
    }
}

/// Each extension folder has `gemini-extension.json` (name, version, migratedTo) and, when installed with
/// `gemini extensions install`, `.gemini-extension-install.json` (source, type, ref, releaseTag, allowPreRelease).
pub fn discover_gemini_extensions(dir: &str) -> Vec<UpdatableItem> {
    let Some(entries) = list_dir(dir) else {
        return Vec::new();
    };
    entries
        .into_iter()
        .filter_map(|entry| {
            let path = format!("{dir}/{entry}");
            if entry.starts_with('.') || !is_dir(&path) {
                return None;
            }
            // Gemini CLI can't load (or update) an extension without a name, so neither do we.
            let config = read_json(&format!("{path}/gemini-extension.json"))?;
            let name = str_of(&config, "name").filter(|n| !n.is_empty())?;
            let version =
                str_of(&config, "version").map_or("Version unknown".into(), |v| format!("v{v}"));
            let record = read_json(&format!("{path}/.gemini-extension-install.json"));
            let kind = record
                .as_ref()
                .and_then(|r| str_of(r, "type"))
                .map_or(String::new(), |t| format!(" · {t}"));
            let mut item = UpdatableItem::new(
                SourceKind::GeminiExtension,
                name,
                &format!("{version}{kind}"),
                None,
            );
            item.path = Some(path);
            Some(item)
        })
        .collect()
}

/// Mirrors Gemini CLI's own check (`checkForExtensionUpdate`), except that an annotated tag ref is compared
/// by its commit.
pub fn gemini_extension_status(path: &str) -> ItemStatus {
    let record = read_json(&format!("{path}/.gemini-extension-install.json"));
    let Some((record, kind)) = record.as_ref().and_then(|r| Some((r, str_of(r, "type")?))) else {
        return unknown("No install record");
    };
    let config = read_json(&format!("{path}/gemini-extension.json"));
    let source = str_of(record, "source").unwrap_or("");
    match kind {
        "link" => unknown("Linked folder; nothing to update"),
        "local" => {
            let installed = config.as_ref().and_then(|c| str_of(c, "version"));
            let source_config = read_json(&format!("{source}/gemini-extension.json"));
            match (
                source_config.as_ref().and_then(|c| str_of(c, "version")),
                installed,
            ) {
                (Some(latest), Some(installed)) if latest == installed => ItemStatus::UpToDate,
                (Some(latest), Some(installed)) => {
                    ItemStatus::UpdateAvailable(Some(format!("v{installed} → v{latest}")))
                }
                _ => unknown("Can't read the source folder's version"),
            }
        }
        "git" | "github-release" => {
            let git_ref = str_of(record, "ref").filter(|r| !r.is_empty());
            let allow_pre = record.get("allowPreRelease").and_then(Value::as_bool) == Some(true);
            // Like Gemini CLI, a move only counts once the new source can be checked; until then, check the old one.
            if let Some(moved) = config
                .as_ref()
                .and_then(|c| str_of(c, "migratedTo"))
                .filter(|m| !m.is_empty())
            {
                let moved_status = ItemStatus::UpdateAvailable(Some(format!("Moved to {moved}")));
                if kind == "git" {
                    return moved_status;
                }
                if let Some((owner, repo)) = github_repo(moved) {
                    if github_release_tag(&owner, &repo, git_ref, allow_pre, GITHUB_API).is_some() {
                        return moved_status;
                    }
                }
            }
            if kind == "github-release" {
                let installed = str_of(record, "releaseTag");
                let latest = installed
                    .and(github_repo(source))
                    .and_then(|(owner, repo)| {
                        github_release_tag(&owner, &repo, git_ref, allow_pre, GITHUB_API)
                    });
                // None includes GitHub's 60-an-hour API limit.
                return match (installed, latest) {
                    (Some(installed), Some(latest)) if latest == installed => ItemStatus::UpToDate,
                    (Some(installed), Some(latest)) => {
                        ItemStatus::UpdateAvailable(Some(format!("{installed} → {latest}")))
                    }
                    _ => unknown("Couldn't check"),
                };
            }
            let q = quote(path);
            let head = run(&format!("{GIT} -C {q} rev-parse HEAD"), 30);
            if !head.ok() || head.output.is_empty() {
                return unknown("Not a git checkout");
            }
            let origin = run(&format!("{GIT} -C {q} remote get-url origin"), 30);
            let url = if origin.ok() && !origin.output.is_empty() {
                origin.output.as_str()
            } else {
                source
            };
            remote_head_status(url, git_ref.unwrap_or("HEAD"), &head.output, |s| {
                format!("New version ({s})")
            })
        }
        other => unknown(&format!("Unknown install type {other}")),
    }
}

/// `owner/repo`, `https://github.com/owner/repo(.git)` or `git@github.com:owner/repo(.git)`; None for other hosts.
pub fn github_repo(source: &str) -> Option<(String, String)> {
    let mut s = source;
    if let Some(rest) = s.strip_prefix("git@github.com:") {
        s = rest;
    } else if s.starts_with("git@") {
        return None;
    }
    for prefix in ["https://github.com/", "http://github.com/", "github.com/"] {
        if let Some(rest) = s.strip_prefix(prefix) {
            s = rest;
        }
    }
    let parts: Vec<&str> = s.split('/').filter(|p| !p.is_empty()).collect();
    if s.contains(':') || parts.len() < 2 {
        return None;
    }
    Some((
        parts[0].into(),
        parts[1].strip_suffix(".git").unwrap_or(parts[1]).into(),
    ))
}

/// Same release lookup as Gemini CLI: a pinned tag, else "latest", else (or with pre-releases) the newest release.
pub fn github_release_tag(
    owner: &str,
    repo: &str,
    git_ref: Option<&str>,
    allow_pre: bool,
    api: &str,
) -> Option<String> {
    let fetch = |path: &str| -> Option<Value> {
        let url = quote(&format!("{api}/repos/{owner}/{repo}/{path}"));
        let result = run(
            &format!("curl -fsS --max-time 20 -H 'Accept: application/vnd.github+json' {url}"),
            30,
        );
        result
            .ok()
            .then(|| serde_json::from_str(&result.output).ok())
            .flatten()
    };
    let tag = |v: &Value| v.get("tag_name")?.as_str().map(String::from);
    if let Some(git_ref) = git_ref {
        return fetch(&format!("releases/tags/{git_ref}"))
            .as_ref()
            .and_then(tag);
    }
    if !allow_pre {
        if let Some(latest) = fetch("releases/latest").as_ref().and_then(tag) {
            return Some(latest);
        }
    }
    fetch("releases?per_page=1")?.get(0).and_then(tag)
}

pub fn discover_antigravity_plugins() -> Vec<UpdatableItem> {
    if !run("command -v agy", 30).ok() {
        return Vec::new();
    }
    antigravity_plugins(&run("agy plugin list --json", 60).output)
}

/// Parses `agy plugin list --json`: `[{"name", "enabled", "version", "source", "path", "skills"}]`. Output is
/// stdout and stderr together, so the JSON is cut out of any log lines around it.
pub fn antigravity_plugins(output: &str) -> Vec<UpdatableItem> {
    let list =
        extract_json(output, '[', ']').and_then(|j| serde_json::from_str::<Vec<Value>>(j).ok());
    let mut items: Vec<UpdatableItem> = list
        .unwrap_or_default()
        .iter()
        .filter_map(|plugin| {
            let name = plugin.get("name")?.as_str().filter(|n| !n.is_empty())?;
            let path = plugin.get("path")?.as_str()?;
            let version = plugin
                .get("version")
                .and_then(Value::as_str)
                .map_or("Version unknown".into(), |v| format!("v{v}"));
            let disabled = if plugin.get("enabled").and_then(Value::as_bool) == Some(false) {
                " · disabled"
            } else {
                ""
            };
            let mut item = UpdatableItem::new(
                SourceKind::AntigravityPlugin,
                name,
                &format!("{version}{disabled}"),
                None,
            );
            item.path = Some(path.into());
            Some(item)
        })
        .collect();
    items.sort_by(|a, b| a.name.cmp(&b.name));
    items
}

/// Where `agy plugin install` can fetch the plugin again: the checkout's origin, if it's a network URL.
/// Anything else (a local path, an option-looking string) is never passed to `agy`.
pub fn antigravity_install_url(origin: &str) -> Option<String> {
    let url = origin.trim();
    let network = ["https://", "ssh://", "git@"]
        .iter()
        .any(|p| url.starts_with(p));
    (network && !url.contains(' ')).then(|| url.into())
}

// Helpers

/// A folder's entry names, sorted; None if it can't be read.
fn list_dir(path: &str) -> Option<Vec<String>> {
    let mut names: Vec<String> = std::fs::read_dir(path)
        .ok()?
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .collect();
    names.sort();
    Some(names)
}

fn is_dir(path: &str) -> bool {
    Path::new(path).is_dir()
}

fn parent(path: &str) -> String {
    Path::new(path)
        .parent()
        .map_or(path.into(), |p| p.to_string_lossy().into())
}

/// Resolves symlinks in as much of the path as exists, like NSString's resolvingSymlinksInPath.
fn resolve(path: &str) -> String {
    let mut existing = PathBuf::from(path);
    let mut rest = Vec::new();
    loop {
        if let Ok(mut real) = std::fs::canonicalize(&existing) {
            real.extend(rest.iter().rev());
            return real.to_string_lossy().into();
        }
        match existing.file_name().map(|n| n.to_os_string()) {
            Some(name) if existing.pop() => rest.push(name),
            _ => return path.into(),
        }
    }
}

/// A JSON file's top-level object.
fn read_json(path: &str) -> Option<Json> {
    match serde_json::from_slice(&std::fs::read(path).ok()?).ok()? {
        Value::Object(map) => Some(map),
        _ => None,
    }
}

fn str_of<'a>(object: &'a Json, key: &str) -> Option<&'a str> {
    object.get(key)?.as_str()
}

/// Pulls the JSON out of output that may have warnings around it.
fn extract_json(text: &str, open: char, close: char) -> Option<&str> {
    let (start, end) = (text.find(open)?, text.rfind(close)?);
    (start < end).then(|| &text[start..=end])
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::symlink;

    struct Tmp(PathBuf);
    impl Tmp {
        fn new(name: &str) -> Tmp {
            let dir =
                std::env::temp_dir().join(format!("pullday-scanner-{}-{name}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            Tmp(dir)
        }
        fn p(&self, rel: &str) -> String {
            self.0.join(rel).to_string_lossy().into()
        }
        fn mkdir(&self, rel: &str) {
            fs::create_dir_all(self.0.join(rel)).unwrap();
        }
        fn write(&self, rel: &str, text: &str) {
            let path = self.0.join(rel);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
        }
        fn link(&self, rel: &str, to: &str) {
            symlink(self.0.join(to), self.0.join(rel)).unwrap();
        }
    }

    #[test]
    fn watched_dirs_match_the_swift_watch_folders_list() {
        let s = Scanner::new("/h");
        let kinds = |ids: &[&str]| ids.iter().map(|k| k.to_string()).collect();
        // Swift: Scanner.skillRoots + [marketplacesDir] + the optional folders whose kind is on, in this order.
        let base = [
            "/h/.claude/skills",
            "/h/.agents/skills",
            "/h/.gemini/skills",
            "/h/.config/opencode/skills",
            "/h/.cursor/skills",
            "/h/.claude/plugins/marketplaces",
        ];
        assert_eq!(s.watched_dirs(&kinds(&["plugin", "gitSkill", "uv"])), base);
        let all = s.watched_dirs(&kinds(&[
            "antigravityPlugin",
            "geminiExtension",
            "codexPlugin",
        ]));
        assert_eq!(all[..6], base);
        assert_eq!(
            all[6..],
            [
                "/h/.codex/.tmp/marketplaces",
                "/h/.gemini/extensions",
                "/h/.gemini/config/plugins"
            ]
        );
        assert_eq!(
            s.watched_dirs(&kinds(&["geminiExtension"]))[6..],
            ["/h/.gemini/extensions"]
        );
    }
    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn remote_refs() {
        let ls = "1111111111111111111111111111111111111111\trefs/heads/feature/main\n\
                  2222222222222222222222222222222222222222\trefs/heads/main\n\
                  3333333333333333333333333333333333333333\trefs/tags/v1\n\
                  4444444444444444444444444444444444444444\trefs/tags/v1^{}\n\
                  5555555555555555555555555555555555555555\tHEAD";
        let pick = |r| pick_remote_sha(ls, r).map(|s| s[..4].to_string());
        assert_eq!(
            pick("main").as_deref(),
            Some("2222"),
            "a branch, not a branch merely ending in its name"
        );
        assert_eq!(
            pick("v1").as_deref(),
            Some("4444"),
            "an annotated tag gives its commit (peeled ^{{}})"
        );
        assert_eq!(
            pick("refs/tags/v1").as_deref(),
            Some("4444"),
            "a fully qualified annotated tag"
        );
        assert_eq!(
            pick("refs/heads/main").as_deref(),
            Some("2222"),
            "a fully qualified branch"
        );
        assert_eq!(pick("HEAD").as_deref(), Some("5555"));
        assert_eq!(pick("nope"), None);
        assert!(is_commit_sha(&"a".repeat(40)) && !is_commit_sha("main"));
    }

    #[test]
    fn source_urls() {
        assert_eq!(
            codex_source_url("openai/plugins"),
            "https://github.com/openai/plugins.git"
        );
        assert_eq!(codex_source_url("github.com/a/b"), "https://github.com/a/b");
        assert_eq!(
            codex_source_url("git@github.com:a/b.git"),
            "git@github.com:a/b.git"
        );
        assert_eq!(codex_source_url("./local/x"), "./local/x");
        let repos = [
            ("o/r", Some("o/r")),
            ("https://github.com/o/r.git", Some("o/r")),
            ("git@github.com:o/r.git", Some("o/r")),
            ("github.com/o/r", Some("o/r")),
            ("git@gitlab.com:o/r", None),
            ("https://gitlab.com/o/r", None),
        ];
        for (source, expected) in repos {
            assert_eq!(
                github_repo(source)
                    .map(|(o, r)| format!("{o}/{r}"))
                    .as_deref(),
                expected,
                "{source}"
            );
        }
    }

    /// Consumer sets, manual-only for outside repos, and never treating a skill root or an ancestor as the repo.
    #[test]
    fn git_skill_discovery() {
        let t = Tmp::new("skills");
        t.mkdir("home/.claude/skills/claudeonly/.git");
        t.mkdir("home/repos/both/.git");
        t.link("home/.claude/skills/both", "home/repos/both");
        t.mkdir("home/.agents/skills");
        t.link("home/.agents/skills/both", "home/repos/both");
        t.mkdir("home/.gemini/.git"); // a dotfiles repo at an agent's home
        t.mkdir("home/.gemini/skills/plain");
        t.mkdir("home/.config/.git"); // ~/.config as a repo
        t.mkdir("home/.config/opencode/skills/plain");
        t.mkdir("home/dotfiles/.git"); // stow-style: ~/.cursor links into a dotfiles repo
        t.mkdir("home/dotfiles/cursor/skills/plain");
        t.link("home/.cursor", "home/dotfiles/cursor");

        let items = Scanner::new(t.p("home")).discover_git_skills();
        let by_name: HashMap<&str, &UpdatableItem> =
            items.iter().map(|i| (i.name.as_str(), i)).collect();
        let mut names: Vec<_> = by_name.keys().copied().collect();
        names.sort();
        assert_eq!(
            names,
            ["both", "claudeonly"],
            "only real skill repos are found"
        );
        use Agent::*;
        assert_eq!(by_name["claudeonly"].agents, [Claude, Opencode].into());
        assert!(
            !by_name["claudeonly"].manual_only,
            "a repo inside ~/.claude is auto-updatable"
        );
        assert_eq!(
            by_name["both"].agents,
            [Claude, Codex, Gemini, Opencode, Cursor].into()
        );
        assert!(
            by_name["both"].manual_only,
            "a repo outside every agent folder is manual updates only"
        );
        assert_eq!(
            by_name["claudeonly"].id,
            format!(
                "gitSkill:{}/.claude/skills/claudeonly",
                resolve(&t.p("home"))
            )
        );
        assert_eq!(by_name["claudeonly"].detail, "~/.claude/skills/claudeonly");
    }

    #[test]
    fn a_project_behind_a_symlinked_skills_folder_stays_manual() {
        let t = Tmp::new("linked-root");
        t.mkdir("home/.claude");
        t.mkdir("home/my-skills");
        t.link("home/.claude/skills", "home/my-skills");
        t.mkdir("home/Workspace/project/.git");
        t.link("home/my-skills/project", "home/Workspace/project");
        t.mkdir("home/.claude/vendored/.git");
        t.link("home/my-skills/vendored", "home/.claude/vendored");
        let items = Scanner::new(t.p("home")).discover_git_skills();
        let manual: Vec<(&str, bool)> = items
            .iter()
            .map(|i| (i.name.as_str(), i.manual_only))
            .collect();
        assert_eq!(manual, [("project", true), ("vendored", false)]);
    }

    #[test]
    fn plugin_marketplaces() {
        let t = Tmp::new("plugins");
        t.mkdir("home/.claude/plugins/marketplaces/official");
        t.mkdir("home/.claude/plugins/marketplaces/empty");
        t.mkdir("home/.claude/plugins/marketplaces/.hidden");
        t.write("home/.claude/plugins/marketplaces/file", "");
        t.write(
            "home/.claude/plugins/installed_plugins.json",
            r#"{"version": 2, "plugins": {"b@official": {}, "a@official": {}, "x@": {}, "noat": {}}}"#,
        );
        let items = Scanner::new(t.p("home")).discover_plugins();
        assert_eq!(
            items.iter().map(|i| i.name.as_str()).collect::<Vec<_>>(),
            ["empty", "official"]
        );
        assert_eq!(items[0].detail, "Marketplace (no plugins installed)");
        assert_eq!(
            (items[1].detail.as_str(), &items[1].plugins[..]),
            ("a, b", &["a@official".into(), "b@official".into()][..])
        );
        assert_eq!(items[1].id, "plugin:official");
    }

    #[test]
    fn snapshot_marketplaces() {
        let t = Tmp::new("snapshot");
        let sha = "abcdef0123456789abcdef0123456789abcdef01";
        t.write(
            "home/.claude/plugins/marketplaces/official/.gcs-sha",
            &format!("{sha}\n"),
        );
        let scanner = Scanner::new(t.p("home"));
        let path = t.p("home/.claude/plugins/marketplaces/official");
        assert_eq!(
            scanner.git_status(Some(&path)),
            unknown("Snapshot install; source repo unknown")
        );
        // A ref pinned to the installed commit needs no network.
        t.write(
            "home/.claude/plugins/known_marketplaces.json",
            &format!(r#"{{"official": {{"source": {{"source": "github", "repo": "o/r", "ref": "{sha}"}}}}}}"#),
        );
        assert_eq!(scanner.git_status(Some(&path)), ItemStatus::UpToDate);
        assert_eq!(
            scanner.git_status(Some(&t.p("home"))),
            unknown("Not a git checkout")
        );
        assert_eq!(scanner.git_status(None), unknown("Not a git checkout"));
    }

    #[test]
    fn git_never_fetches_from_a_local_path() {
        let t = Tmp::new("hardening");
        let repo = t.p("skill");
        let setup = format!(
            "git init -q {r} && git -C {r} remote add origin {o}",
            r = quote(&repo),
            o = quote(&t.p("elsewhere"))
        );
        assert!(run(&setup, 30).ok());
        match Scanner::new(t.p("home")).git_status(Some(&repo)) {
            ItemStatus::Failed(why) => {
                assert!(why.contains("transport 'file' not allowed"), "{why}")
            }
            other => panic!("a local-path remote was fetched: {other:?}"),
        }
    }

    #[test]
    fn codex_records() {
        let t = Tmp::new("codex");
        t.write(
            "mk/local/.codex-marketplace-install.json",
            r#"{"source_type":"local","source":"/x","revision":"abc"}"#,
        );
        t.write(
            "mk/norev/.codex-marketplace-install.json",
            r#"{"source_type":"git","source":"o/r","ref_name":"main"}"#,
        );
        t.mkdir("mk/norecord");
        t.mkdir("mk/.staging");
        t.mkdir("cache/local/superpowers");
        let items = discover_codex_plugins(&t.p("mk"), &t.p("cache"));
        assert_eq!(
            items.iter().map(|i| i.name.as_str()).collect::<Vec<_>>(),
            ["local", "norecord", "norev"]
        );
        assert_eq!(
            items[0].detail, "superpowers",
            "installed plugins come from the cache folder"
        );
        let statuses = Scanner::new(t.p("home")).check(SourceKind::CodexPlugin, &items);
        assert_eq!(
            statuses["codexPlugin:local"],
            unknown("Local marketplace; nothing to update")
        );
        assert_eq!(
            statuses["codexPlugin:norev"],
            unknown("Installed revision unknown")
        );
        assert_eq!(
            statuses["codexPlugin:norecord"],
            unknown("No install record")
        );
    }

    #[test]
    fn gemini_records() {
        let t = Tmp::new("gemini");
        let ext = |name: &str, config: &str, record: Option<&str>| {
            t.write(&format!("ext/{name}/gemini-extension.json"), config);
            if let Some(r) = record {
                t.write(&format!("ext/{name}/.gemini-extension-install.json"), r);
            }
        };
        t.write(
            "src/gemini-extension.json",
            r#"{"name":"src","version":"2.0.0"}"#,
        );
        let local = format!(r#"{{"source":"{}","type":"local"}}"#, t.p("src"));
        ext(
            "current",
            r#"{"name":"current","version":"2.0.0"}"#,
            Some(&local),
        );
        ext("old", r#"{"name":"old","version":"1.0.0"}"#, Some(&local));
        ext(
            "linked",
            r#"{"name":"linked"}"#,
            Some(r#"{"source":"/x","type":"link"}"#),
        );
        ext(
            "moved",
            r#"{"name":"moved","migratedTo":"https://github.com/n/r"}"#,
            Some(r#"{"source":"o/r","type":"git"}"#),
        );
        ext("odd", r#"{"name":"odd"}"#, Some(r#"{"type":"tarball"}"#));
        ext("bare", r#"{"name":"bare","version":"1"}"#, None);
        ext("nameless", r#"{"version":"1"}"#, None);
        let items = discover_gemini_extensions(&t.p("ext"));
        let names: Vec<&str> = items.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(
            names,
            ["bare", "current", "linked", "moved", "odd", "old"],
            "an extension without a name is skipped"
        );
        assert_eq!(
            (items[0].detail.as_str(), items[1].detail.as_str()),
            ("v1", "v2.0.0 · local")
        );
        let s = |name: &str| gemini_extension_status(&t.p(&format!("ext/{name}")));
        assert_eq!(s("current"), ItemStatus::UpToDate);
        assert_eq!(
            s("old"),
            ItemStatus::UpdateAvailable(Some("v1.0.0 → v2.0.0".into()))
        );
        assert_eq!(s("linked"), unknown("Linked folder; nothing to update"));
        assert_eq!(
            s("moved"),
            ItemStatus::UpdateAvailable(Some("Moved to https://github.com/n/r".into()))
        );
        assert_eq!(s("odd"), unknown("Unknown install type tarball"));
        assert_eq!(s("bare"), unknown("No install record"));
    }

    /// `agy plugin list --json` as Antigravity CLI 1.4.0 prints it (captured from a real install), plus the Go
    /// logger's stderr noise.
    #[test]
    fn antigravity() {
        let list = r#"ERROR: logging before google.Init: I1016 21:58:38.362951       1 cli.go:27] starting
[
  {"name": "ponytail", "enabled": true, "version": "5.3.1", "source": "antigravity",
   "path": "/home/u/.gemini/config/plugins/ponytail", "skills": ["ponytail", "ponytail-review"]},
  {"name": "alpha", "enabled": false, "source": "antigravity", "path": "/home/u/.gemini/config/plugins/alpha"}
]"#;
        let items = antigravity_plugins(list);
        assert_eq!(
            items.iter().map(|i| i.name.as_str()).collect::<Vec<_>>(),
            ["alpha", "ponytail"]
        );
        assert_eq!(items[1].detail, "v5.3.1");
        assert_eq!(
            items[1].path.as_deref(),
            Some("/home/u/.gemini/config/plugins/ponytail")
        );
        assert_eq!(items[0].detail, "Version unknown · disabled");
        assert!(items
            .iter()
            .all(|i| i.agents == [Agent::Antigravity].into()
                && i.id.starts_with("antigravityPlugin:")));
        assert!(antigravity_plugins("agy: not logged in").is_empty());

        for url in [
            "https://github.com/o/r.git",
            "git@github.com:o/r.git",
            "ssh://git@host/o/r",
        ] {
            assert_eq!(
                antigravity_install_url(&format!("{url}\n")).as_deref(),
                Some(url)
            );
        }
        for url in [
            "/home/u/src/plugin",
            "./plugin",
            "-oProxyCommand=touch /tmp/x",
            "file:///tmp/r",
            "https://x y",
        ] {
            assert_eq!(
                antigravity_install_url(url),
                None,
                "{url} is never passed to agy"
            );
        }
        let t = Tmp::new("agy");
        assert_eq!(
            Scanner::new(t.p("home")).antigravity_plugin_status(&t.p("")),
            unknown("Not installed from a git repo")
        );
    }

    #[test]
    fn an_option_looking_name_is_never_passed_on() {
        let item = UpdatableItem::new(SourceKind::GeminiExtension, "--all", "", None);
        let result = Scanner::new("/nonexistent").update(&item, None);
        assert!(
            !result.ok
                && result.summary.contains("won't pass the name --all")
                && !result.output.contains("$ ")
        );
    }

    #[test]
    fn json_is_cut_out_of_log_noise() {
        assert_eq!(extract_json("warn [1] x", '[', ']'), Some("[1]"));
        assert_eq!(extract_json("] [", '[', ']'), None);
        assert_eq!(
            Scanner::new("/home/u").tilde("/home/u/.claude/skills/x"),
            "~/.claude/skills/x"
        );
        assert_eq!(Scanner::new("/home/u").tilde("/srv/x"), "/srv/x");
    }
}
