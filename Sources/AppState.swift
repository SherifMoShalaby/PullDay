import AppKit
import Foundation

enum Phase { case idle, scanning, updating }
enum SettingsTab: Hashable { case general, sources, items }

@MainActor
final class AppState: ObservableObject {
    static let shared = AppState()

    @Published private(set) var items: [UpdatableItem] = []
    @Published private(set) var phase: Phase = .idle
    @Published private(set) var log: [LogEntry] = []
    @Published var settingsTab: SettingsTab = .general

    let prefs: Prefs
    private var timer: Timer?
    private var watchers: [DispatchSourceFileSystemObject] = []
    private var rescanTask: Task<Void, Never>?
    private var started = false

    private init() {
        prefs = Prefs.shared
    }

    var isBusy: Bool { phase != .idle }
    var visibleItems: [UpdatableItem] { items.filter { !prefs.ignored.contains($0.id) } }
    var updatesAvailable: [UpdatableItem] { visibleItems.filter { $0.status.isUpdateAvailable } }

    // MARK: - Scheduling

    func start() {
        guard !started else { return }
        started = true

        // Wake up every 15 minutes and run if the chosen interval has passed.
        // This survives sleep better than one long timer.
        timer = Timer.scheduledTimer(withTimeInterval: 15 * 60, repeats: true) { [weak self] _ in
            Task { @MainActor in await self?.runIfDue() }
        }
        NSWorkspace.shared.notificationCenter.addObserver(
            forName: NSWorkspace.didWakeNotification, object: nil, queue: .main
        ) { [weak self] _ in
            Task { @MainActor in
                try? await Task.sleep(nanoseconds: 60_000_000_000) // let the network come back
                await self?.runIfDue()
            }
        }

        watchFolders()

        Task { @MainActor in
            let due = isDue
            await refresh()
            if due { await afterScheduledCheck() }
        }
    }

    /// Rescan soon after a skill or marketplace is added, removed or renamed, instead of
    /// waiting for the schedule. Only the top-level folders are watched, so pulls inside a repo don't fire.
    /// Re-armed after every scan, so a root created since (e.g. ~/.agents/skills) gets watched.
    private func watchFolders() {
        watchers.forEach { $0.cancel() }   // the cancel handler closes each fd
        watchers.removeAll()
        // Agent folders only while their source is on: Codex upgrades churn through a .staging folder.
        let optional = [(SourceKind.codexPlugin, Scanner.codexMarketplacesDir), (.geminiExtension, Scanner.geminiExtensionsDir)]
            .filter { prefs.enabledKinds.contains($0.0.rawValue) }.map(\.1)
        for path in Scanner.skillRoots + [Scanner.marketplacesDir] + optional {
            let fd = open(path, O_EVTONLY)
            guard fd >= 0 else { continue }
            let source = DispatchSource.makeFileSystemObjectSource(fileDescriptor: fd, eventMask: [.write, .rename, .delete], queue: .main)
            source.setEventHandler { [weak self] in
                Task { @MainActor in
                    // A plugin update replaces its marketplace folder; rescanning then would wipe the
                    // "Updated" results, and update() already leaves every item's status current.
                    guard let self, self.phase != .updating else { return }
                    self.scheduleRescan()
                }
            }
            source.setCancelHandler { close(fd) }
            source.resume()
            watchers.append(source)
        }
    }

    private func scheduleRescan() {
        rescanTask?.cancel()
        rescanTask = Task { @MainActor in
            try? await Task.sleep(nanoseconds: 5_000_000_000) // let a burst of changes settle
            // refresh() returns early while busy, so wait for the running scan to finish instead of losing this one.
            while isBusy, !Task.isCancelled { try? await Task.sleep(nanoseconds: 1_000_000_000) }
            guard !Task.isCancelled else { return }
            await refresh(recordRun: false)
        }
    }

    private var isDue: Bool {
        guard prefs.checkInterval > 0 else { return false }
        guard let last = prefs.lastRun else { return true }
        return Date().timeIntervalSince(last) >= prefs.checkInterval
    }

    func runIfDue() async {
        guard isDue, !isBusy else { return }
        await refresh()
        await afterScheduledCheck()
    }

    private func afterScheduledCheck() async {
        let available = updatesAvailable
        guard !available.isEmpty else { return }
        let auto = prefs.autoUpdate ? available.filter { !$0.manualOnly } : []
        if !auto.isEmpty {
            await update(auto.map(\.id), notifyWhenDone: true)
        }
        let rest = available.filter { !(prefs.autoUpdate && !$0.manualOnly) }
        if !rest.isEmpty, prefs.notify {
            let names = rest.map(\.name).joined(separator: ", ")
            Notifier.send(title: "\(rest.count) update\(rest.count == 1 ? "" : "s") available",
                          body: names)
        }
    }

    // MARK: - Checking

    /// `recordRun: false` rescans without moving `lastRun`, so it doesn't postpone a scheduled run.
    func refresh(recordRun: Bool = true) async {
        guard !isBusy else { return }
        phase = .scanning
        let kinds = SourceKind.allCases.filter { prefs.enabledKinds.contains($0.rawValue) }

        var found: [UpdatableItem] = []
        await withTaskGroup(of: [UpdatableItem].self) { group in
            for kind in kinds { group.addTask { await Scanner.discover(kind) } }
            for await list in group { found += list }
        }
        let order = Dictionary(uniqueKeysWithValues: SourceKind.allCases.enumerated().map { ($0.element, $0.offset) })
        found.sort {
            if $0.kind != $1.kind { return order[$0.kind, default: 0] < order[$1.kind, default: 0] }
            return $0.name.localizedCaseInsensitiveCompare($1.name) == .orderedAscending
        }

        let ignored = prefs.ignored
        items = found.map { item in
            var item = item
            item.status = ignored.contains(item.id) ? .unknown("Ignored") : .checking
            return item
        }

        let toCheck = items.filter { !ignored.contains($0.id) }
        await withTaskGroup(of: [String: ItemStatus].self) { group in
            for kind in kinds {
                let subset = toCheck.filter { $0.kind == kind }
                if subset.isEmpty { continue }
                group.addTask { await Scanner.check(kind, items: subset) }
            }
            for await statuses in group {
                for (id, status) in statuses {
                    if let index = items.firstIndex(where: { $0.id == id }) { items[index].status = status }
                }
            }
        }
        for index in items.indices where items[index].status == .checking {
            items[index].status = .unknown(nil)
        }
        for index in items.indices {
            let id = items[index].id
            guard let offer = prefs.skippedUpdates[id] else { continue }
            let resolved = items[index].status.resolvingSkip(offer)
            items[index].status = resolved.status
            if !resolved.keepSkip { prefs.skippedUpdates[id] = nil }
        }

        if recordRun { prefs.lastRun = Date() }
        phase = .idle
        watchFolders()

        let checked = items.filter { !ignored.contains($0.id) }
        let failures = checked.filter { $0.status.isFailure }
        let available = checked.filter { $0.status.isUpdateAvailable }
        var lines = checked.map { "\($0.name) (\($0.kind.title)): \(describe($0.status))" }
        if lines.isEmpty { lines = ["Nothing found. Check the Sources tab in Settings."] }
        appendLog(title: "Checked \(checked.count) item\(checked.count == 1 ? "" : "s"): "
                    + "\(available.count) update\(available.count == 1 ? "" : "s") available"
                    + (failures.isEmpty ? "" : ", \(failures.count) couldn't be checked"),
                  output: lines.joined(separator: "\n"),
                  success: failures.isEmpty)
    }

    // MARK: - Updating

    func update(_ ids: [String], notifyWhenDone: Bool = false) async {
        guard !isBusy, !ids.isEmpty else { return }
        phase = .updating
        var updated: [UpdatableItem] = []
        var failed: [String] = []

        for id in ids {
            guard let index = items.firstIndex(where: { $0.id == id }) else { continue }
            items[index].status = .updating
            let item = items[index]
            let result = await Scanner.update(item, postCommand: prefs.postCommands[id])
            appendLog(title: "Update \(item.name) (\(item.kind.title))", output: result.output, success: result.ok)
            if let j = items.firstIndex(where: { $0.id == id }) {
                items[j].status = result.ok ? .updated(result.summary) : .failed(result.summary)
            }
            if result.ok { updated.append(item) } else { failed.append(item.name) }
        }
        phase = .idle

        guard notifyWhenDone, prefs.notify else { return }
        if !updated.isEmpty {
            var body = updated.map(\.name).joined(separator: ", ")
            let restart: [SourceKind: String] = [.plugin: "Claude Code", .codexPlugin: "Codex", .geminiExtension: "Gemini CLI"]
            let agents = Set(updated.compactMap { restart[$0.kind] }).sorted()
            if !agents.isEmpty { body += ". Restart \(agents.joined(separator: " and ")) to load the new versions." }
            Notifier.send(title: "Updated \(updated.count) item\(updated.count == 1 ? "" : "s")", body: body)
        }
        if !failed.isEmpty {
            Notifier.send(title: "\(failed.count) update\(failed.count == 1 ? "" : "s") failed",
                          body: failed.joined(separator: ", ") + ". Open the PullDay log for details.")
        }
    }

    // MARK: - Misc

    func setIgnored(_ id: String, _ ignore: Bool) {
        if ignore { prefs.ignored.insert(id) } else { prefs.ignored.remove(id) }
        if let index = items.firstIndex(where: { $0.id == id }), ignore {
            items[index].status = .unknown("Ignored")
        }
    }

    func skipUpdate(_ id: String) {
        guard let index = items.firstIndex(where: { $0.id == id }),
              case .updateAvailable(let offer?) = items[index].status else { return }
        prefs.skippedUpdates[id] = offer
        items[index].status = .skipped(offer)
    }

    func stopSkipping(_ id: String) {
        guard let offer = prefs.skippedUpdates.removeValue(forKey: id) else { return }
        if let index = items.firstIndex(where: { $0.id == id }), case .skipped = items[index].status {
            items[index].status = .updateAvailable(offer)
        }
    }

    func clearLog() { log.removeAll() }

    private func appendLog(title: String, output: String, success: Bool) {
        let entry = LogEntry(date: Date(), title: title, output: output, success: success)
        log.insert(entry, at: 0)
        if log.count > 200 { log.removeLast(log.count - 200) }
        LogFile.append(entry)
    }

    private func describe(_ status: ItemStatus) -> String {
        switch status {
        case .unknown(let reason): return reason ?? "unknown"
        case .checking: return "checking"
        case .upToDate: return "up to date"
        case .updateAvailable(let info): return "update available" + (info.map { " (\($0.prefix { $0 != "\n" }))" } ?? "")
        case .pinned(let message): return message
        case .skipped(let offer): return "skipped (\(offer.prefix { $0 != "\n" }))"
        case .updating: return "updating"
        case .updated(let summary): return summary
        case .failed(let message): return "error: \(message)"
        }
    }
}
