import Foundation

@MainActor
final class Prefs: ObservableObject {
    static let shared = Prefs()

    private let defaults: UserDefaults

    private enum Key {
        static let interval = "checkInterval"
        static let autoUpdate = "autoUpdate"
        static let notify = "notify"
        static let kinds = "enabledKinds"
        static let knownKinds = "knownKinds"
        static let ignored = "ignored"
        static let skipped = "skippedUpdates"
        static let postCommands = "postCommands"
        static let lastRun = "lastRun"
    }

    /// Seconds between automatic checks; 0 means manual only.
    @Published var checkInterval: TimeInterval { didSet { defaults.set(checkInterval, forKey: Key.interval) } }
    @Published var autoUpdate: Bool { didSet { defaults.set(autoUpdate, forKey: Key.autoUpdate) } }
    @Published var notify: Bool { didSet { defaults.set(notify, forKey: Key.notify) } }
    @Published var enabledKinds: Set<String> { didSet { defaults.set(Array(enabledKinds), forKey: Key.kinds) } }
    @Published var ignored: Set<String> { didSet { defaults.set(Array(ignored), forKey: Key.ignored) } }
    /// Item id → the update offer the user skipped.
    @Published var skippedUpdates: [String: String] { didSet { defaults.set(skippedUpdates, forKey: Key.skipped) } }
    @Published var postCommands: [String: String] { didSet { defaults.set(postCommands, forKey: Key.postCommands) } }
    @Published var lastRun: Date? { didSet { defaults.set(lastRun, forKey: Key.lastRun) } }

    /// The kinds shipped before `knownKinds` existed. Never `allCases`: then nothing would ever look new.
    static let legacyKinds = ["plugin", "gitSkill", "uv", "pipx", "npm", "claudeCode"]

    /// The app was called SkillSync; settings saved before the rename live under this domain.
    static let oldDomain = "dev.skillsync.app"

    /// Copies the settings saved under `old` into `defaults` on the first launch, which is before anything has
    /// written `knownKinds`. The old domain is left as it is.
    static func copySettings(fromDomain old: String, into defaults: UserDefaults) {
        guard defaults.object(forKey: Key.knownKinds) == nil,
              let saved = UserDefaults.standard.persistentDomain(forName: old) else { return }
        for (key, value) in saved { defaults.set(value, forKey: key) }
    }

    init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
        if defaults === UserDefaults.standard { Prefs.copySettings(fromDomain: Prefs.oldDomain, into: defaults) }
        defaults.register(defaults: [
            Key.interval: 24.0 * 3600,
            Key.autoUpdate: false,   // new installs only check and notify until the user opts in
            Key.notify: true,
            Key.kinds: SourceKind.allCases.filter(\.enabledByDefault).map(\.rawValue),
            Key.ignored: [String](),
            Key.skipped: [String: String](),
            // graphify's `uv tool upgrade` updates the CLI; `graphify install`
            // re-copies the updated skill files into ~/.claude/skills.
            Key.postCommands: ["uv:graphifyy": "graphify install", "pipx:graphifyy": "graphify install"],
        ])
        checkInterval = defaults.double(forKey: Key.interval)
        autoUpdate = defaults.bool(forKey: Key.autoUpdate)
        notify = defaults.bool(forKey: Key.notify)
        // Turn on kinds added since the last launch once, so a user who changed their sources still gets
        // them; a kind they later turn off stays off. Written directly because didSet doesn't run in init.
        var kinds = Set(defaults.stringArray(forKey: Key.kinds) ?? [])
        let known = Set(defaults.stringArray(forKey: Key.knownKinds) ?? Prefs.legacyKinds)
        let added = SourceKind.allCases.filter { !known.contains($0.rawValue) && $0.enabledByDefault }
        if !added.isEmpty {
            kinds.formUnion(added.map(\.rawValue))
            defaults.set(Array(kinds), forKey: Key.kinds)
        }
        defaults.set(SourceKind.allCases.map(\.rawValue), forKey: Key.knownKinds)
        enabledKinds = kinds
        ignored = Set(defaults.stringArray(forKey: Key.ignored) ?? [])
        skippedUpdates = (defaults.dictionary(forKey: Key.skipped) as? [String: String]) ?? [:]
        postCommands = (defaults.dictionary(forKey: Key.postCommands) as? [String: String]) ?? [:]
        lastRun = defaults.object(forKey: Key.lastRun) as? Date
    }
}
