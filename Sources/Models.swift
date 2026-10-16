import Foundation

enum SourceKind: String, CaseIterable, Identifiable, Sendable {
    case plugin, gitSkill, uv, pipx, npm, claudeCode, codexPlugin, codexCLI, geminiExtension

    var id: String { rawValue }

    var title: String {
        switch self {
        case .plugin: return "Claude Code plugins"
        case .gitSkill: return "Git-cloned skills"
        case .uv: return "uv tools"
        case .pipx: return "pipx tools"
        case .npm: return "npm global packages"
        case .claudeCode: return "Claude Code"
        case .codexPlugin: return "Codex plugin marketplaces"
        case .codexCLI: return "Codex CLI"
        case .geminiExtension: return "Gemini CLI extensions"
        }
    }

    var symbol: String {
        switch self {
        case .plugin: return "puzzlepiece.extension"
        case .gitSkill: return "arrow.triangle.branch"
        case .uv: return "shippingbox"
        case .pipx: return "cube.box"
        case .npm: return "cube"
        case .claudeCode: return "terminal"
        case .codexPlugin: return "puzzlepiece.extension"
        case .codexCLI: return "terminal"
        case .geminiExtension: return "puzzlepiece.extension"
        }
    }

    var explanation: String {
        switch self {
        case .plugin:
            return "Marketplaces in ~/.claude/plugins/marketplaces, updated with `claude plugin marketplace update` and `claude plugin update`."
        case .gitSkill:
            return "Git repos in each agent's skills folder (~/.claude/skills, ~/.agents/skills, ~/.gemini/skills, ~/.config/opencode/skills, ~/.cursor/skills; symlinks followed), updated with `git pull --ff-only`."
        case .uv:
            return "Tools from `uv tool install`, such as graphify. Updated with `uv tool upgrade`."
        case .pipx:
            return "Tools from `pipx install`. Updated with `pipx upgrade`."
        case .npm:
            return "Packages from `npm install -g`. Off by default because it lists every global package."
        case .claudeCode:
            return "The `claude` command in your terminal. A Homebrew install is updated with `brew upgrade --cask claude-code`. The Claude desktop app updates its own copy."
        case .codexPlugin:
            return "Git marketplaces in ~/.codex/.tmp/marketplaces, updated with `codex plugin marketplace upgrade`, which also refreshes their installed plugins."
        case .codexCLI:
            return "The `codex` command. Updated with `codex update`, which handles Homebrew, npm and the install script."
        case .geminiExtension:
            return "Extensions in ~/.gemini/extensions, updated with `gemini extensions update`. Linked extensions are skipped, and an update that needs your consent has to be run in a terminal."
        }
    }

    var enabledByDefault: Bool { self != .npm }

    /// The agents that load items of this kind, before discovery refines it (git skills, graphify).
    var agents: Set<Agent> {
        switch self {
        case .plugin, .claudeCode: return [.claude]
        case .codexPlugin, .codexCLI: return [.codex]
        case .geminiExtension: return [.gemini]
        case .gitSkill, .uv, .pipx, .npm: return []
        }
    }
}

/// A coding agent that loads skills or plugins PullDay keeps up to date.
enum Agent: String, CaseIterable, Identifiable, Sendable {
    case claude, codex, gemini, opencode, cursor

    var id: String { rawValue }

    var title: String {
        switch self {
        case .claude: return "Claude Code"
        case .codex: return "Codex"
        case .gemini: return "Gemini CLI"
        case .opencode: return "OpenCode"
        case .cursor: return "Cursor"
        }
    }

    /// The agent's config folder, relative to home; it existing means the agent is installed.
    var homeDir: String {
        switch self {
        case .claude: return ".claude"
        case .codex: return ".codex"
        case .gemini: return ".gemini"
        case .opencode: return ".config/opencode"
        case .cursor: return ".cursor"
        }
    }

    var isDetected: Bool {
        FileManager.default.fileExists(atPath: Shell.home + "/" + homeDir)
    }

    /// Each user skill root (relative to home) and the agents that load skills from it.
    static let skillRootReaders: [(root: String, agents: Set<Agent>)] = [
        (".claude/skills", [.claude, .opencode]),
        (".agents/skills", [.codex, .gemini, .opencode, .cursor]),
        (".gemini/skills", [.gemini]),
        (".config/opencode/skills", [.opencode]),
        (".cursor/skills", [.cursor]),
    ]
}

enum ItemStatus: Equatable, Sendable {
    case unknown(String?)
    case checking
    case upToDate
    case updateAvailable(String?)
    /// Installed with an exact version pin, so upgrading won't move it.
    case pinned(String)
    /// An offered update the user chose to skip; the message is the offer that was skipped.
    case skipped(String)
    case updating
    case updated(String)
    case failed(String)

    var isUpdateAvailable: Bool {
        if case .updateAvailable = self { return true }
        return false
    }

    var isBusy: Bool { self == .checking || self == .updating }

    /// Applies a skipped offer to a fresh check result. The skip holds while the same offer comes back;
    /// a different offer (newer version, more commits) or "up to date" ends it. Other results keep it,
    /// so a failed check doesn't forget the skip.
    func resolvingSkip(_ skippedOffer: String) -> (status: ItemStatus, keepSkip: Bool) {
        switch self {
        case .updateAvailable(let info) where info == skippedOffer: return (.skipped(skippedOffer), true)
        case .updateAvailable, .upToDate: return (self, false)
        default: return (self, true)
        }
    }

    var isFailure: Bool {
        if case .failed = self { return true }
        return false
    }
}

struct UpdatableItem: Identifiable, Equatable, Sendable {
    let id: String
    let kind: SourceKind
    let name: String
    var detail: String
    var path: String?
    /// For plugin marketplaces: installed plugins as "name@marketplace".
    var plugins: [String]
    var status: ItemStatus
    /// The agents that load this item; empty for tools no agent loads directly (uv, pipx, npm).
    var agents: Set<Agent>
    /// Never updated by the schedule, only by an explicit Update (e.g. a project repo behind a skill symlink).
    var manualOnly = false

    init(kind: SourceKind, name: String, detail: String, path: String? = nil,
         plugins: [String] = [], id: String? = nil) {
        self.id = id ?? "\(kind.rawValue):\(name)"
        self.kind = kind
        self.name = name
        self.detail = detail
        self.path = path
        self.plugins = plugins
        // graphify (uv/pipx) installs its skill into ~/.claude/skills, so it belongs to Claude Code.
        self.agents = name == "graphifyy" ? [.claude] : kind.agents
        self.status = .unknown(nil)
    }
}

struct UpdateResult: Sendable {
    let ok: Bool
    let summary: String
    let output: String
}

struct LogEntry: Identifiable, Sendable {
    let id = UUID()
    let date: Date
    let title: String
    let output: String
    let success: Bool
}
