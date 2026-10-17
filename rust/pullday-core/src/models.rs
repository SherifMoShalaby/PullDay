//! Port of `Sources/Models.swift`. Names, ids and agent sets must match it: `Tests/models.golden`
//! is checked by both the Swift self-test and `cargo test`.

use std::collections::BTreeSet;
use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum SourceKind {
    Plugin,
    GitSkill,
    Uv,
    Pipx,
    Npm,
    ClaudeCode,
    CodexPlugin,
    CodexCli,
    GeminiExtension,
    AntigravityPlugin,
    AntigravityCli,
}

impl SourceKind {
    pub const ALL: [SourceKind; 11] = [
        Self::Plugin,
        Self::GitSkill,
        Self::Uv,
        Self::Pipx,
        Self::Npm,
        Self::ClaudeCode,
        Self::CodexPlugin,
        Self::CodexCli,
        Self::GeminiExtension,
        Self::AntigravityPlugin,
        Self::AntigravityCli,
    ];

    /// The Swift raw value: item ids and saved settings use it.
    pub fn id(self) -> &'static str {
        match self {
            Self::Plugin => "plugin",
            Self::GitSkill => "gitSkill",
            Self::Uv => "uv",
            Self::Pipx => "pipx",
            Self::Npm => "npm",
            Self::ClaudeCode => "claudeCode",
            Self::CodexPlugin => "codexPlugin",
            Self::CodexCli => "codexCLI",
            Self::GeminiExtension => "geminiExtension",
            Self::AntigravityPlugin => "antigravityPlugin",
            Self::AntigravityCli => "antigravityCLI",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.id() == id)
    }

    pub fn title(self) -> &'static str {
        match self {
            Self::Plugin => "Claude Code plugins",
            Self::GitSkill => "Git-cloned skills",
            Self::Uv => "uv tools",
            Self::Pipx => "pipx tools",
            Self::Npm => "npm global packages",
            Self::ClaudeCode => "Claude Code",
            Self::CodexPlugin => "Codex plugin marketplaces",
            Self::CodexCli => "Codex CLI",
            Self::GeminiExtension => "Gemini CLI extensions",
            Self::AntigravityPlugin => "Antigravity plugins",
            Self::AntigravityCli => "Antigravity CLI",
        }
    }

    /// An SF Symbols name, as in the Swift app; the Linux UI maps it to its own icon.
    pub fn symbol(self) -> &'static str {
        match self {
            Self::Plugin => "puzzlepiece.extension",
            Self::GitSkill => "arrow.triangle.branch",
            Self::Uv => "shippingbox",
            Self::Pipx => "cube.box",
            Self::Npm => "cube",
            Self::ClaudeCode => "terminal",
            Self::CodexPlugin => "puzzlepiece.extension",
            Self::CodexCli => "terminal",
            Self::GeminiExtension => "puzzlepiece.extension",
            Self::AntigravityPlugin => "puzzlepiece.extension",
            Self::AntigravityCli => "terminal",
        }
    }

    pub fn explanation(self) -> &'static str {
        match self {
            Self::Plugin => "Marketplaces in ~/.claude/plugins/marketplaces, updated with `claude plugin marketplace update` and `claude plugin update`.",
            Self::GitSkill => "Git repos in each agent's skills folder (~/.claude/skills, ~/.agents/skills, ~/.gemini/skills, ~/.config/opencode/skills, ~/.cursor/skills; symlinks followed), updated with `git pull --ff-only`.",
            Self::Uv => "Tools from `uv tool install`, such as graphify. Updated with `uv tool upgrade`.",
            Self::Pipx => "Tools from `pipx install`. Updated with `pipx upgrade`.",
            Self::Npm => "Packages from `npm install -g`. Off by default because it lists every global package.",
            Self::ClaudeCode => "The `claude` command in your terminal. A Homebrew install is updated with `brew upgrade --cask claude-code`. The Claude desktop app updates its own copy.",
            Self::CodexPlugin => "Git marketplaces in ~/.codex/.tmp/marketplaces, updated with `codex plugin marketplace upgrade`, which also refreshes their installed plugins.",
            Self::CodexCli => "The `codex` command. Updated with `codex update`, which handles Homebrew, npm and the install script.",
            Self::GeminiExtension => "Extensions in ~/.gemini/extensions, updated with `gemini extensions update`. Linked extensions are skipped, and an update that needs your consent has to be run in a terminal.",
            Self::AntigravityPlugin => "Plugins installed with `agy plugin install` from a git repo, checked against that repo and updated by installing them again with `agy`, which keeps their enabled state.",
            Self::AntigravityCli => "The `agy` command, Google's replacement for Gemini CLI. Updated with `agy update`.",
        }
    }

    pub fn enabled_by_default(self) -> bool {
        self != Self::Npm
    }

    /// The agents that load items of this kind, before discovery refines it (git skills, graphify).
    pub fn agents(self) -> BTreeSet<Agent> {
        use Agent::*;
        match self {
            Self::Plugin | Self::ClaudeCode => [Claude].into(),
            Self::CodexPlugin | Self::CodexCli => [Codex].into(),
            Self::GeminiExtension => [Gemini].into(),
            Self::AntigravityPlugin | Self::AntigravityCli => [Antigravity].into(),
            Self::GitSkill | Self::Uv | Self::Pipx | Self::Npm => BTreeSet::new(),
        }
    }
}

/// A coding agent that loads skills or plugins PullDay keeps up to date. Ordered as in Swift's `allCases`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Agent {
    Claude,
    Codex,
    Gemini,
    Antigravity,
    Opencode,
    Cursor,
}

impl Agent {
    pub const ALL: [Agent; 6] = [
        Self::Claude,
        Self::Codex,
        Self::Gemini,
        Self::Antigravity,
        Self::Opencode,
        Self::Cursor,
    ];

    pub fn id(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Gemini => "gemini",
            Self::Antigravity => "antigravity",
            Self::Opencode => "opencode",
            Self::Cursor => "cursor",
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Self::Claude => "Claude Code",
            Self::Codex => "Codex",
            Self::Gemini => "Gemini CLI",
            Self::Antigravity => "Antigravity",
            Self::Opencode => "OpenCode",
            Self::Cursor => "Cursor",
        }
    }

    /// The agent's config folder, relative to home; it existing means the agent is installed.
    pub fn home_dir(self) -> &'static str {
        match self {
            Self::Claude => ".claude",
            Self::Codex => ".codex",
            Self::Gemini => ".gemini",
            Self::Antigravity => ".gemini/config", // created by `agy`; Gemini CLI doesn't use it
            Self::Opencode => ".config/opencode",
            Self::Cursor => ".cursor",
        }
    }

    pub fn is_detected(self, home: &Path) -> bool {
        home.join(self.home_dir()).exists()
    }

    /// Each user skill root (relative to home) and the agents that load skills from it.
    pub fn skill_root_readers() -> Vec<(&'static str, BTreeSet<Agent>)> {
        use Agent::*;
        vec![
            (".claude/skills", [Claude, Opencode].into()),
            (".agents/skills", [Codex, Gemini, Opencode, Cursor].into()),
            (".gemini/skills", [Gemini].into()),
            (".config/opencode/skills", [Opencode].into()),
            (".cursor/skills", [Cursor].into()),
        ]
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ItemStatus {
    Unknown(Option<String>),
    Checking,
    UpToDate,
    UpdateAvailable(Option<String>),
    /// Installed with an exact version pin, so upgrading won't move it.
    Pinned(String),
    /// An offered update the user chose to skip; the message is the offer that was skipped.
    Skipped(String),
    Updating,
    Updated(String),
    Failed(String),
}

impl ItemStatus {
    pub fn is_update_available(&self) -> bool {
        matches!(self, Self::UpdateAvailable(_))
    }

    pub fn is_busy(&self) -> bool {
        matches!(self, Self::Checking | Self::Updating)
    }

    pub fn is_failure(&self) -> bool {
        matches!(self, Self::Failed(_))
    }

    /// Applies a skipped offer to a fresh check result. The skip holds while the same offer comes back;
    /// a different offer (newer version, more commits) or "up to date" ends it. Other results keep it,
    /// so a failed check doesn't forget the skip. Returns the status and whether to keep the skip.
    pub fn resolving_skip(&self, skipped_offer: &str) -> (ItemStatus, bool) {
        match self {
            Self::UpdateAvailable(Some(info)) if info == skipped_offer => {
                (Self::Skipped(skipped_offer.to_string()), true)
            }
            Self::UpdateAvailable(_) | Self::UpToDate => (self.clone(), false),
            _ => (self.clone(), true),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UpdatableItem {
    pub id: String,
    pub kind: SourceKind,
    pub name: String,
    pub detail: String,
    pub path: Option<String>,
    /// For plugin marketplaces: installed plugins as "name@marketplace".
    pub plugins: Vec<String>,
    pub status: ItemStatus,
    /// The agents that load this item; empty for tools no agent loads directly (uv, pipx, npm).
    pub agents: BTreeSet<Agent>,
    /// Never updated by the schedule, only by an explicit Update (e.g. a project repo behind a skill symlink).
    pub manual_only: bool,
}

impl UpdatableItem {
    /// The id is `"<kind>:<name>"` unless given (git skills pass `"gitSkill:<repo path>"`).
    pub fn new(kind: SourceKind, name: &str, detail: &str, id: Option<String>) -> Self {
        UpdatableItem {
            id: id.unwrap_or_else(|| format!("{}:{}", kind.id(), name)),
            kind,
            name: name.to_string(),
            detail: detail.to_string(),
            path: None,
            plugins: Vec::new(),
            status: ItemStatus::Unknown(None),
            // graphify (uv/pipx) installs its skill into ~/.claude/skills, so it belongs to Claude Code.
            agents: if name == "graphifyy" {
                [Agent::Claude].into()
            } else {
                kind.agents()
            },
            manual_only: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UpdateResult {
    pub ok: bool,
    pub summary: String,
    pub output: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_ids_round_trip() {
        for k in SourceKind::ALL {
            assert_eq!(SourceKind::from_id(k.id()), Some(k));
        }
        assert_eq!(SourceKind::from_id("codexCli"), None);
    }

    #[test]
    fn a_skip_holds_only_for_the_same_offer() {
        let skip = "2 new commits";
        let same = ItemStatus::UpdateAvailable(Some(skip.into()));
        assert_eq!(
            same.resolving_skip(skip),
            (ItemStatus::Skipped(skip.into()), true)
        );
        let newer = ItemStatus::UpdateAvailable(Some("3 new commits".into()));
        assert_eq!(newer.resolving_skip(skip), (newer.clone(), false));
        let unnamed = ItemStatus::UpdateAvailable(None);
        assert_eq!(unnamed.resolving_skip(skip), (unnamed.clone(), false));
        assert_eq!(
            ItemStatus::UpToDate.resolving_skip(skip),
            (ItemStatus::UpToDate, false)
        );
        let failed = ItemStatus::Failed("offline".into());
        assert_eq!(failed.resolving_skip(skip), (failed.clone(), true));
    }

    #[test]
    fn is_detected_checks_the_home_dir() {
        let home = std::env::temp_dir().join(format!("pullday-core-{}", std::process::id()));
        std::fs::create_dir_all(home.join(".gemini/config")).unwrap();
        assert!(Agent::Antigravity.is_detected(&home) && Agent::Gemini.is_detected(&home));
        assert!(!Agent::Claude.is_detected(&home));
        std::fs::remove_dir_all(&home).unwrap();
    }
}
