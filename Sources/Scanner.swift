import Foundation

/// Finds installed skills/tools, checks them for updates, and updates them.
enum Scanner {
    static let home = Shell.home
    static var marketplacesDir: String { home + "/.claude/plugins/marketplaces" }
    static var installedPluginsFile: String { home + "/.claude/plugins/installed_plugins.json" }
    static var knownMarketplacesFile: String { home + "/.claude/plugins/known_marketplaces.json" }
    static var codexMarketplacesDir: String { home + "/.codex/.tmp/marketplaces" }
    static var codexPluginCacheDir: String { home + "/.codex/plugins/cache" }
    static var geminiExtensionsDir: String { home + "/.gemini/extensions" }
    static var antigravityPluginsDir: String { home + "/.gemini/config/plugins" }
    static var skillRoots: [String] { Agent.skillRootReaders.map { home + "/" + $0.root } }

    // MARK: - Discover

    static func discover(_ kind: SourceKind) async -> [UpdatableItem] {
        switch kind {
        case .plugin: return discoverPlugins()
        case .gitSkill: return discoverGitSkills()
        case .uv: return await discoverUV()
        case .pipx: return await discoverPipx()
        case .npm: return await discoverNPM()
        case .claudeCode: return await discoverClaudeCode()
        case .codexPlugin: return discoverCodexPlugins()
        case .codexCLI: return await discoverCodexCLI()
        case .geminiExtension: return discoverGeminiExtensions()
        case .antigravityPlugin: return await discoverAntigravityPlugins()
        case .antigravityCLI: return await discoverAntigravityCLI()
        }
    }

    static func discoverPlugins() -> [UpdatableItem] {
        guard let entries = try? FileManager.default.contentsOfDirectory(atPath: marketplacesDir) else { return [] }
        let installed = installedPlugins()
        return entries.sorted().compactMap { name -> UpdatableItem? in
            let path = marketplacesDir + "/" + name
            guard !name.hasPrefix("."), isDirectory(path) else { return nil }
            let plugins = installed[name] ?? []
            let names = plugins.map { key -> String in
                guard let at = key.lastIndex(of: "@") else { return key }
                return String(key[..<at])
            }
            let detail = names.isEmpty ? "Marketplace (no plugins installed)" : names.joined(separator: ", ")
            return UpdatableItem(kind: .plugin, name: name, detail: detail, path: path, plugins: plugins)
        }
    }

    /// Reads ~/.claude/plugins/installed_plugins.json → [marketplace: ["plugin@marketplace", …]].
    static func installedPlugins() -> [String: [String]] {
        guard let data = FileManager.default.contents(atPath: installedPluginsFile),
              let object = try? JSONSerialization.jsonObject(with: data) as? [String: Any] else { return [:] }
        let container = (object["plugins"] as? [String: Any]) ?? object
        var result: [String: [String]] = [:]
        for key in container.keys {
            guard let at = key.lastIndex(of: "@") else { continue }
            let marketplace = String(key[key.index(after: at)...])
            guard !marketplace.isEmpty else { continue }
            result[marketplace, default: []].append(key)
        }
        return result.mapValues { $0.sorted() }
    }

    /// One item per repo; its agents are the readers of every root it was found in.
    static func discoverGitSkills(home: String = home) -> [UpdatableItem] {
        let fm = FileManager.default
        let roots = Agent.skillRootReaders.map { (path: home + "/" + $0.root, agents: $0.agents) }
        let rootPaths = roots.map(\.path)
        var items: [UpdatableItem] = []
        var indexByRepo: [String: Int] = [:]
        for root in roots {
            guard let entries = try? fm.contentsOfDirectory(atPath: root.path) else { continue }
            for entry in entries.sorted() where !entry.hasPrefix(".") {
                let full = (root.path as NSString).appendingPathComponent(entry)
                let resolved = (full as NSString).resolvingSymlinksInPath
                guard isDirectory(resolved), let repo = gitRoot(of: resolved, roots: rootPaths) else { continue }
                if let i = indexByRepo[repo] { items[i].agents.formUnion(root.agents); continue }
                let name = (repo as NSString).lastPathComponent
                // A symlink into a repo outside every agent's folder is likely the user's own project.
                let external = !rootPaths.contains { repo.hasPrefix(($0 as NSString).deletingLastPathComponent + "/") }
                var item = UpdatableItem(kind: .gitSkill, name: name,
                                         detail: tilde(repo) + (external ? " · manual updates only" : ""),
                                         path: repo, id: "gitSkill:" + repo)
                item.manualOnly = external
                item.agents = root.agents
                indexByRepo[repo] = items.count
                items.append(item)
            }
        }
        return items
    }

    /// Walks up from a skill folder to the git repo that contains it, never treating a skill root
    /// or any of its ancestors (home, ~/.claude, ~/.config, …) as the repo.
    static func gitRoot(of path: String, roots: [String] = skillRoots) -> String? {
        var blocked: Set<String> = ["/"]
        // Resolved roots too: entries are resolved, so a stow-style symlinked root must block its real ancestors.
        for root in roots + roots.map({ ($0 as NSString).resolvingSymlinksInPath }) {
            var url = URL(fileURLWithPath: root).standardizedFileURL
            while url.path != "/" { blocked.insert(url.path); url.deleteLastPathComponent() }
        }
        var current = URL(fileURLWithPath: path).standardizedFileURL
        for _ in 0..<8 {
            let p = current.path
            if blocked.contains(p) { return nil }
            if FileManager.default.fileExists(atPath: current.appendingPathComponent(".git").path) { return p }
            current = current.deletingLastPathComponent().standardizedFileURL
        }
        return nil
    }

    static func discoverUV() async -> [UpdatableItem] {
        let result = await Shell.run("uv tool list", timeout: 60)
        guard result.ok else { return [] }
        return result.lines.compactMap { line -> UpdatableItem? in
            // Tool lines look like "graphifyy v0.5.1"; executables are indented "- graphify".
            guard let first = line.first, !first.isWhitespace, first != "-" else { return nil }
            let parts = line.split(separator: " ")
            guard parts.count >= 2, parts[1].hasPrefix("v") else { return nil }
            return UpdatableItem(kind: .uv, name: String(parts[0]), detail: String(parts[1]))
        }
    }

    static func discoverPipx() async -> [UpdatableItem] {
        let result = await Shell.run("pipx list --short", timeout: 60)
        guard result.ok else { return [] }
        return result.lines.compactMap { line -> UpdatableItem? in
            let parts = line.split(separator: " ")
            guard parts.count >= 2 else { return nil }
            return UpdatableItem(kind: .pipx, name: String(parts[0]), detail: "v\(parts[1])")
        }
    }

    /// Every git call: never run hooks or fsmonitor from a repo's own config, never follow file:// remotes.
    static let git = "git -c core.hooksPath=/dev/null -c core.fsmonitor=false -c protocol.file.allow=never"

    static func discoverNPM() async -> [UpdatableItem] {
        let result = await Shell.run("npm ls -g --depth=0 --json", timeout: 60)
        guard let data = extractJSON(result.output, open: "{", close: "}"),
              let object = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              let deps = object["dependencies"] as? [String: Any] else { return [] }
        let skip: Set<String> = ["npm", "corepack"]
        // Registry installs carry no `resolved`; linked, file: and git globals do, and
        // `npm install -g <name>@latest` would replace them with a registry package of the same name.
        return deps.keys.sorted().filter { !skip.contains($0) && (deps[$0] as? [String: Any])?["resolved"] == nil }.map { name -> UpdatableItem in
            let version = (deps[name] as? [String: Any])?["version"] as? String ?? ""
            return UpdatableItem(kind: .npm, name: name, detail: version.isEmpty ? "" : "v\(version)")
        }
    }

    static func discoverClaudeCode() async -> [UpdatableItem] {
        let which = await Shell.run("command -v claude", timeout: 30)
        guard which.ok, !which.output.isEmpty else { return [] }
        let version = await claudeVersion()
        return [UpdatableItem(kind: .claudeCode, name: "claude-code", detail: version.map { "v" + $0 } ?? "Version unknown",
                              path: (which.output as NSString).resolvingSymlinksInPath)]
    }

    /// `claude --version` prints "2.1.292 (Claude Code)".
    static func claudeVersion() async -> String? {
        let result = await Shell.run("claude --version", timeout: 30)
        guard result.ok, let first = result.output.split(whereSeparator: \.isWhitespace).first else { return nil }
        return String(first)
    }

    /// Codex keeps each marketplace as a snapshot whose `.codex-marketplace-install.json` records where it came from.
    static func discoverCodexPlugins(dir: String = codexMarketplacesDir, cacheDir: String = codexPluginCacheDir) -> [UpdatableItem] {
        guard let entries = try? FileManager.default.contentsOfDirectory(atPath: dir) else { return [] }
        return entries.sorted().compactMap { name -> UpdatableItem? in
            let path = dir + "/" + name
            guard !name.hasPrefix("."), isDirectory(path) else { return nil }   // skips .staging
            let plugins = ((try? FileManager.default.contentsOfDirectory(atPath: cacheDir + "/" + name)) ?? [])
                .filter { !$0.hasPrefix(".") }.sorted()
            let detail = plugins.isEmpty ? "Marketplace (no plugins installed)" : plugins.joined(separator: ", ")
            return UpdatableItem(kind: .codexPlugin, name: name, detail: detail, path: path)
        }
    }

    static func discoverCodexCLI() async -> [UpdatableItem] {
        let which = await Shell.run("command -v codex", timeout: 30)
        guard which.ok, !which.output.isEmpty else { return [] }
        let version = await codexVersion()
        return [UpdatableItem(kind: .codexCLI, name: "codex", detail: version.map { "v" + $0 } ?? "Version unknown",
                              path: (which.output as NSString).resolvingSymlinksInPath)]
    }

    /// `codex --version` prints "codex-cli 0.163.0".
    static func codexVersion() async -> String? {
        let result = await Shell.run("codex --version", timeout: 30)
        // Output is stdout and stderr together, so skip any Node or profile warnings.
        guard result.ok, let line = result.lines.first(where: { $0.hasPrefix("codex-cli") }),
              let last = line.split(whereSeparator: \.isWhitespace).last else { return nil }
        return String(last)
    }

    static func discoverAntigravityCLI() async -> [UpdatableItem] {
        let which = await Shell.run("command -v agy", timeout: 30)
        guard which.ok, !which.output.isEmpty else { return [] }
        let version = await agyVersion()
        return [UpdatableItem(kind: .antigravityCLI, name: "agy", detail: version.map { "v" + $0 } ?? "Version unknown",
                              path: (which.output as NSString).resolvingSymlinksInPath)]
    }

    /// `agy --version` prints "1.4.0". Its Go logger can add "ERROR: logging before google.Init" lines, so take
    /// the first line that is a version.
    static func agyVersion() async -> String? {
        let result = await Shell.run("agy --version", timeout: 30)
        guard result.ok else { return nil }
        return result.lines.map { $0.trimmingCharacters(in: .whitespaces) }
            .first { $0.first?.isNumber == true && $0.allSatisfy { $0.isNumber || $0 == "." } }
    }

    /// The version `agy update` would install, from the manifest Antigravity's installer reads.
    static func agyLatestVersion() async -> String? {
        #if arch(arm64)
        let platform = "darwin_arm64"
        #else
        let platform = "darwin_amd64"
        #endif
        let url = "https://antigravity-cli-auto-updater-974169037036.us-central1.run.app/manifests/\(platform).json"
        let api = await Shell.run("curl -fsS --max-time 20 \(Shell.quote(url))", timeout: 30)
        guard api.ok, let object = try? JSONSerialization.jsonObject(with: Data(api.output.utf8)) as? [String: Any],
              let latest = object["version"] as? String, !latest.isEmpty else { return nil }
        return latest
    }

    static func discoverAntigravityPlugins() async -> [UpdatableItem] {
        guard await Shell.run("command -v agy", timeout: 30).ok else { return [] }
        return antigravityPlugins(fromList: await Shell.run("agy plugin list --json", timeout: 60).output)
    }

    /// Parses `agy plugin list --json`: `[{"name", "enabled", "version", "source", "path", "skills"}]`. Output is
    /// stdout and stderr together, so the JSON is cut out of any log lines around it.
    static func antigravityPlugins(fromList output: String) -> [UpdatableItem] {
        guard let data = extractJSON(output, open: "[", close: "]"),
              let list = try? JSONSerialization.jsonObject(with: data) as? [[String: Any]] else { return [] }
        return list.compactMap { plugin -> UpdatableItem? in
            guard let name = plugin["name"] as? String, !name.isEmpty, let path = plugin["path"] as? String else { return nil }
            let version = (plugin["version"] as? String).map { "v" + $0 } ?? "Version unknown"
            let disabled = plugin["enabled"] as? Bool == false ? " · disabled" : ""
            return UpdatableItem(kind: .antigravityPlugin, name: name, detail: version + disabled, path: path)
        }.sorted { $0.name < $1.name }
    }

    /// A plugin installed from a git repo is a checkout of it, so the usual git check applies. Others (a marketplace,
    /// a local folder) can't be checked from here.
    static func antigravityPluginStatus(_ path: String) async -> ItemStatus {
        guard FileManager.default.fileExists(atPath: path + "/.git") else { return .unknown("Not installed from a git repo") }
        return await gitStatus(path)
    }

    /// Where `agy plugin install` can fetch the plugin again: the checkout's origin, if it's a network URL.
    /// Anything else (a local path, an option-looking string) is never passed to `agy`.
    static func antigravityInstallURL(_ origin: String) -> String? {
        let url = origin.trimmingCharacters(in: .whitespacesAndNewlines)
        return ["https://", "ssh://", "git@"].contains(where: url.hasPrefix) && !url.contains(" ") ? url : nil
    }

    static func readJSON(_ path: String) -> [String: Any]? {
        FileManager.default.contents(atPath: path).flatMap { try? JSONSerialization.jsonObject(with: $0) as? [String: Any] }
    }

    /// Each extension folder has `gemini-extension.json` (name, version, migratedTo) and, when installed with
    /// `gemini extensions install`, `.gemini-extension-install.json` (source, type, ref, releaseTag, allowPreRelease).
    static func discoverGeminiExtensions(dir: String = geminiExtensionsDir) -> [UpdatableItem] {
        guard let entries = try? FileManager.default.contentsOfDirectory(atPath: dir) else { return [] }
        return entries.sorted().compactMap { entry -> UpdatableItem? in
            let path = dir + "/" + entry
            guard !entry.hasPrefix("."), isDirectory(path) else { return nil }
            // Gemini CLI can't load (or update) an extension without a name, so neither do we.
            guard let config = readJSON(path + "/gemini-extension.json"),
                  let name = config["name"] as? String, !name.isEmpty else { return nil }
            let version = (config["version"] as? String).map { "v" + $0 } ?? "Version unknown"
            let type = readJSON(path + "/.gemini-extension-install.json")?["type"] as? String
            return UpdatableItem(kind: .geminiExtension, name: name,
                                 detail: version + (type.map { " · \($0)" } ?? ""), path: path)
        }
    }

    /// Mirrors Gemini CLI's own check (`checkForExtensionUpdate`), except that an annotated tag ref is compared
    /// by its commit.
    static func geminiExtensionStatus(_ path: String) async -> ItemStatus {
        guard let record = readJSON(path + "/.gemini-extension-install.json"),
              let type = record["type"] as? String else { return .unknown("No install record") }
        let config = readJSON(path + "/gemini-extension.json")
        let source = record["source"] as? String ?? ""
        switch type {
        case "link":
            return .unknown("Linked folder; nothing to update")
        case "local":
            let installed = config?["version"] as? String
            guard let latest = readJSON(source + "/gemini-extension.json")?["version"] as? String, let installed else {
                return .unknown("Can't read the source folder's version")
            }
            return latest == installed ? .upToDate : .updateAvailable("v\(installed) → v\(latest)")
        case "git", "github-release":
            let ref = (record["ref"] as? String).flatMap { $0.isEmpty ? nil : $0 }
            // Like Gemini CLI, a move only counts once the new source can be checked; until then, check the old one.
            if let moved = config?["migratedTo"] as? String, !moved.isEmpty {
                if type == "git" { return .updateAvailable("Moved to \(moved)") }
                if let (owner, repo) = githubRepo(moved),
                   await githubReleaseTag(owner: owner, repo: repo, ref: ref,
                                          allowPreRelease: record["allowPreRelease"] as? Bool == true) != nil {
                    return .updateAvailable("Moved to \(moved)")
                }
            }
            if type == "github-release" {
                guard let installed = record["releaseTag"] as? String,
                      let (owner, repo) = githubRepo(source),
                      let latest = await githubReleaseTag(owner: owner, repo: repo, ref: ref,
                                                          allowPreRelease: record["allowPreRelease"] as? Bool == true) else {
                    return .unknown("Couldn't check")   // includes GitHub's 60-an-hour API limit
                }
                return latest == installed ? .upToDate : .updateAvailable("\(installed) → \(latest)")
            }
            let q = Shell.quote(path)
            let head = await Shell.run("\(git) -C \(q) rev-parse HEAD", timeout: 30)
            guard head.ok, !head.output.isEmpty else { return .unknown("Not a git checkout") }
            let origin = await Shell.run("\(git) -C \(q) remote get-url origin", timeout: 30)
            let url = origin.ok && !origin.output.isEmpty ? origin.output : source
            return await remoteHeadStatus(url: url, ref: ref ?? "HEAD", localSHA: head.output) { "New version (\($0))" }
        default:
            return .unknown("Unknown install type \(type)")
        }
    }

    /// `owner/repo`, `https://github.com/owner/repo(.git)` or `git@github.com:owner/repo(.git)`; nil for other hosts.
    static func githubRepo(_ source: String) -> (String, String)? {
        var s = source
        if s.hasPrefix("git@github.com:") { s.removeFirst("git@github.com:".count) } else if s.hasPrefix("git@") { return nil }
        for prefix in ["https://github.com/", "http://github.com/", "github.com/"] where s.hasPrefix(prefix) {
            s.removeFirst(prefix.count)
        }
        let parts = s.split(separator: "/")
        guard !s.contains(":"), parts.count >= 2 else { return nil }
        let repo = parts[1].hasSuffix(".git") ? String(parts[1].dropLast(4)) : String(parts[1])
        return (String(parts[0]), repo)
    }

    /// Same release lookup as Gemini CLI: a pinned tag, else "latest", else (or with pre-releases) the newest release.
    static func githubReleaseTag(owner: String, repo: String, ref: String?, allowPreRelease: Bool,
                                 api: String = "https://api.github.com") async -> String? {
        func fetch(_ path: String) async -> Any? {
            let url = Shell.quote("\(api)/repos/\(owner)/\(repo)/\(path)")
            let result = await Shell.run("curl -fsS --max-time 20 -H 'Accept: application/vnd.github+json' \(url)", timeout: 30)
            return result.ok ? try? JSONSerialization.jsonObject(with: Data(result.output.utf8)) : nil
        }
        if let ref { return (await fetch("releases/tags/\(ref)") as? [String: Any])?["tag_name"] as? String }
        if !allowPreRelease, let latest = (await fetch("releases/latest") as? [String: Any])?["tag_name"] as? String {
            return latest
        }
        return ((await fetch("releases?per_page=1") as? [[String: Any]])?.first)?["tag_name"] as? String
    }

    static func isCaskInstall(_ item: UpdatableItem) -> Bool { item.path?.contains("/Caskroom/claude-code/") == true }

    // MARK: - Check

    static func check(_ kind: SourceKind, items: [UpdatableItem]) async -> [String: ItemStatus] {
        switch kind {
        case .plugin, .gitSkill: return await eachItem(items) { await gitStatus($0.path) }
        case .uv: return await checkGraphifySkill(await checkUV(items), items)
        case .pipx: return await checkGraphifySkill(await checkPipx(items), items)
        case .npm: return await checkNPM(items)
        case .claudeCode: return await checkClaudeCode(items)
        case .codexPlugin: return await eachItem(items) { await codexMarketplaceStatus($0.path ?? "") }
        case .codexCLI: return await checkCodexCLI(items)
        case .geminiExtension: return await eachItem(items) { await geminiExtensionStatus($0.path ?? "") }
        case .antigravityPlugin: return await eachItem(items) { await antigravityPluginStatus($0.path ?? "") }
        case .antigravityCLI: return await checkAgy(items)
        }
    }

    /// Checks every item at the same time.
    static func eachItem(_ items: [UpdatableItem],
                         _ status: @escaping @Sendable (UpdatableItem) async -> ItemStatus) async -> [String: ItemStatus] {
        await withTaskGroup(of: (String, ItemStatus).self, returning: [String: ItemStatus].self) { group in
            for item in items { group.addTask { (item.id, await status(item)) } }
            var out: [String: ItemStatus] = [:]
            for await (id, status) in group { out[id] = status }
            return out
        }
    }

    static func gitStatus(_ path: String?) async -> ItemStatus {
        guard let path, FileManager.default.fileExists(atPath: path + "/.git") else {
            if let path, let status = await snapshotStatus(path) { return status }
            return .unknown("Not a git checkout")
        }
        let q = Shell.quote(path)
        let fetch = await Shell.run("\(git) -C \(q) fetch --quiet", timeout: 90)
        guard fetch.ok else { return .failed(fetch.lastLine(or: "git fetch failed")) }
        let count = await Shell.run("\(git) -C \(q) rev-list --count 'HEAD..@{u}'")
        guard count.ok, let n = Int(count.output) else {
            return .failed(count.lastLine(or: "No upstream branch"))
        }
        guard n > 0 else { return .upToDate }
        // First line is the summary; the commit subjects after it show what the pull would bring in.
        var info = "\(n) new commit\(n == 1 ? "" : "s")"
        let log = await Shell.run("\(git) -C \(q) log --oneline --no-decorate -n 5 'HEAD..@{u}'")
        if log.ok, !log.output.isEmpty { info += "\n" + log.output }
        return .updateAvailable(info)
    }

    /// Official marketplaces are installed as snapshots, not git clones: `.gcs-sha` holds the
    /// source commit, and known_marketplaces.json names the GitHub repo it came from.
    static func snapshotStatus(_ path: String) async -> ItemStatus? {
        guard let sha = try? String(contentsOfFile: path + "/.gcs-sha", encoding: .utf8)
            .trimmingCharacters(in: .whitespacesAndNewlines), !sha.isEmpty else { return nil }
        let name = (path as NSString).lastPathComponent
        guard let data = FileManager.default.contents(atPath: knownMarketplacesFile),
              let all = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              let source = (all[name] as? [String: Any])?["source"] as? [String: Any],
              source["source"] as? String == "github", let repo = source["repo"] as? String else {
            return .unknown("Snapshot install; source repo unknown")
        }
        return await remoteHeadStatus(url: "https://github.com/\(repo).git", ref: (source["ref"] as? String) ?? "HEAD",
                                      localSHA: sha) { "New version on GitHub (\($0))" }
    }

    /// Compares a local commit with what `ref` (branch, tag or HEAD) points to upstream.
    /// `offer` builds the message from the new short SHA, which makes each upstream version a new offer
    /// (see Skip This Update).
    static func remoteHeadStatus(url: String, ref: String, localSHA: String,
                                 offer: (String) -> String) async -> ItemStatus {
        // A ref pinned to a commit can't move, so there's nothing to look up.
        if isCommitSHA(ref) { return ref == localSHA ? .upToDate : .updateAvailable("Pinned commit \(ref.prefix(7))") }
        // A plain pattern doesn't list the peeled `^{}` line, so ask for it too.
        let patterns = ref == "HEAD" ? "HEAD" : "\(Shell.quote(ref)) \(Shell.quote(ref + "^{}"))"
        // `--` ends git's options, so a URL from an install record or a remote is never read as `--upload-pack=…`
        // (protocol.file.allow=never in `git` already blocks the local form of that; this closes the rest).
        let remote = await Shell.run("\(git) ls-remote -- \(Shell.quote(url)) \(patterns)", timeout: 60)
        guard remote.ok else { return .failed(remote.lastLine(or: "git ls-remote failed")) }
        guard let head = pickRemoteSHA(remote.output, ref: ref) else { return .failed("git ls-remote found no \(ref)") }
        return head == localSHA ? .upToDate : .updateAvailable(offer(String(head.prefix(7))))
    }

    /// Picks `ref`'s commit from `git ls-remote <url> <ref>` output, which also lists any ref merely ending in
    /// the name (refs/heads/x/main). Order follows git's own: a tag (peeled `^{}` first, so an annotated tag
    /// gives its commit), then a branch.
    static func pickRemoteSHA(_ output: String, ref: String) -> String? {
        var shas: [String: String] = [:]
        for line in output.split(whereSeparator: \.isNewline) {
            let parts = line.split(whereSeparator: \.isWhitespace)
            if parts.count == 2 { shas[String(parts[1])] = String(parts[0]) }
        }
        let candidates = ref == "HEAD" ? ["HEAD"]
            : ["refs/tags/\(ref)^{}", "refs/tags/\(ref)", "refs/heads/\(ref)", ref + "^{}", ref]   // last two: a fully qualified ref
        return candidates.lazy.compactMap { shas[$0] }.first
    }

    /// The install record is `{source_type, source, ref_name, sparse_paths, revision}`; only git sources can move.
    static func codexMarketplaceStatus(_ path: String) async -> ItemStatus {
        guard let record = readJSON(path + "/.codex-marketplace-install.json") else { return .unknown("No install record") }
        guard record["source_type"] as? String == "git", let source = record["source"] as? String, !source.isEmpty else {
            return .unknown("Local marketplace; nothing to update")
        }
        guard let revision = record["revision"] as? String, !revision.isEmpty else {
            return .unknown("Installed revision unknown")
        }
        let ref = (record["ref_name"] as? String).flatMap { $0.isEmpty ? nil : $0 } ?? "HEAD"
        return await remoteHeadStatus(url: codexSourceURL(source), ref: ref, localSHA: revision) { "New version (\($0))" }
    }

    /// `owner/repo` (or `github.com/owner/repo`) is GitHub shorthand; anything else (https, ssh, git@) is a URL as written.
    static func codexSourceURL(_ source: String) -> String {
        if source.hasPrefix("github.com/") { return "https://" + source }
        let parts = source.split(separator: "/", omittingEmptySubsequences: false)
        let shorthand = parts.count == 2 && !source.contains(":") && !source.hasPrefix(".") && parts.allSatisfy { !$0.isEmpty }
        return shorthand ? "https://github.com/\(source).git" : source
    }

    static func isCommitSHA(_ ref: String) -> Bool {
        ref.count == 40 && ref.allSatisfy(\.isHexDigit)
    }

    /// graphify's skill files are copied by `graphify install`, not by upgrading the CLI, so they can
    /// lag behind it. Flagging that as an update makes Update run the `graphify install` post command.
    // shortcut: graphify only, since it's the one tool known to ship its skill separately; generalize if another does.
    static func checkGraphifySkill(_ statuses: [String: ItemStatus], _ items: [UpdatableItem],
                                   versionFile: String = home + "/.claude/skills/graphify/.graphify_version") async -> [String: ItemStatus] {
        var out = statuses
        for item in items where item.name == "graphifyy" && out[item.id] == .upToDate {
            guard let skill = try? String(contentsOfFile: versionFile, encoding: .utf8)
                .trimmingCharacters(in: .whitespacesAndNewlines), !skill.isEmpty else { continue }
            let cli = await Shell.run("graphify --version", timeout: 30)
            guard cli.ok, let last = cli.output.split(whereSeparator: \.isWhitespace).last else { continue }
            let version = last.hasPrefix("v") ? String(last.dropFirst()) : String(last)
            if version != skill {
                out[item.id] = .updateAvailable("Skill files are v\(skill); graphify is v\(version)")
            }
        }
        return out
    }

    /// The cask tracks the stable channel; its API version is what `brew upgrade` installs.
    static func caskLatestVersion(_ cask: String = "claude-code") async -> String? {
        let api = await Shell.run("curl -fsS --max-time 20 \(Shell.quote("https://formulae.brew.sh/api/cask/\(cask).json"))", timeout: 30)
        guard api.ok, let object = try? JSONSerialization.jsonObject(with: Data(api.output.utf8)) as? [String: Any],
              let latest = object["version"] as? String, !latest.isEmpty else { return nil }
        return latest
    }

    static func checkClaudeCode(_ items: [UpdatableItem]) async -> [String: ItemStatus] {
        var out: [String: ItemStatus] = [:]
        for item in items {
            guard isCaskInstall(item) else {
                out[item.id] = .unknown("Not installed with Homebrew; update it with `claude update`")
                continue
            }
            guard let latest = await caskLatestVersion() else {
                out[item.id] = .unknown("Couldn't check")
                continue
            }
            out[item.id] = item.detail == "v" + latest ? .upToDate : .updateAvailable("\(item.detail) → v\(latest)")
        }
        return out
    }

    static func npmLatestVersion(_ package: String) async -> String? {
        let api = await Shell.run("curl -fsS --max-time 20 \(Shell.quote("https://registry.npmjs.org/\(package)/latest"))", timeout: 30)
        guard api.ok, let object = try? JSONSerialization.jsonObject(with: Data(api.output.utf8)) as? [String: Any],
              let latest = object["version"] as? String, !latest.isEmpty else { return nil }
        return latest
    }

    /// A cask install follows the cask; npm and the install script both ship the npm release.
    static func codexLatestVersion(_ item: UpdatableItem) async -> String? {
        item.path?.contains("/Caskroom/codex/") == true ? await caskLatestVersion("codex") : await npmLatestVersion("@openai/codex")
    }

    static func checkAgy(_ items: [UpdatableItem]) async -> [String: ItemStatus] {
        guard let latest = await agyLatestVersion() else { return statuses(items, .unknown("Couldn't check")) }
        return Dictionary(uniqueKeysWithValues: items.map { item in
            (item.id, item.detail == "v" + latest ? ItemStatus.upToDate : .updateAvailable("\(item.detail) → v\(latest)"))
        })
    }

    static func checkCodexCLI(_ items: [UpdatableItem]) async -> [String: ItemStatus] {
        var out: [String: ItemStatus] = [:]
        for item in items {
            guard let latest = await codexLatestVersion(item) else {
                out[item.id] = .unknown("Couldn't check")
                continue
            }
            out[item.id] = item.detail == "v" + latest ? .upToDate : .updateAvailable("\(item.detail) → v\(latest)")
        }
        return out
    }

    /// `uv tool upgrade` never moves past an exact pin (installed as `tool==X`). A wildcard (`==0.3.*`)
    /// or a range (`==1,<2`) can still move, so those aren't treated as pins.
    static func exactPin(in line: String) -> String? {
        guard let req = line.range(of: "[required: ==") else { return nil }
        let version = line[req.upperBound...].prefix { $0 != "]" }
        guard !version.isEmpty, !version.contains("*"), !version.contains(",") else { return nil }
        return "==" + version
    }

    static func checkUV(_ items: [UpdatableItem]) async -> [String: ItemStatus] {
        let result = await Shell.run("uv tool list --outdated --show-version-specifiers", timeout: 180)
        guard result.ok else {
            return statuses(items, .unknown("Can't check: update uv to get `uv tool list --outdated`"))
        }
        var latest: [String: String] = [:]
        var pinned: [String: String] = [:]
        for line in result.lines {
            guard let range = line.range(of: "[latest: ") else { continue }
            let name = String(line.split(separator: " ").first ?? "")
            let version = String(line[range.upperBound...].prefix { $0 != "]" })
            latest[name] = version.hasPrefix("v") ? version : "v" + version
            if let pin = exactPin(in: line) { pinned[name] = pin }
        }
        var out: [String: ItemStatus] = [:]
        for item in items {
            if let version = latest[item.name], let pin = pinned[item.name] {
                out[item.id] = .pinned("Pinned to \(pin) · \(version) available")
            } else if let version = latest[item.name] {
                out[item.id] = .updateAvailable("\(item.detail) → \(version)")
            } else {
                out[item.id] = .upToDate
            }
        }
        return out
    }

    static func checkPipx(_ items: [UpdatableItem]) async -> [String: ItemStatus] {
        await eachItem(items) { item in
            let cmd = "pipx runpip \(Shell.quote(item.name)) list --outdated --format json"
            let result = await Shell.run(cmd, timeout: 120)
            guard result.ok,
                  let data = extractJSON(result.output, open: "[", close: "]"),
                  let rows = try? JSONSerialization.jsonObject(with: data) as? [[String: Any]] else {
                return .unknown("Couldn't check")
            }
            let target = normalizePackage(item.name)
            if let hit = rows.first(where: { normalizePackage($0["name"] as? String ?? "") == target }),
               let version = hit["latest_version"] as? String {
                return .updateAvailable("\(item.detail) → v\(version)")
            }
            return .upToDate
        }
    }

    static func checkNPM(_ items: [UpdatableItem]) async -> [String: ItemStatus] {
        // `npm outdated` exits 1 when something is outdated, so don't rely on the status.
        let result = await Shell.run("npm outdated -g --json", timeout: 180)
        if result.output.isEmpty { return statuses(items, .upToDate) }
        guard let data = extractJSON(result.output, open: "{", close: "}"),
              let object = try? JSONSerialization.jsonObject(with: data) as? [String: Any] else {
            return statuses(items, .unknown("Couldn't check"))
        }
        var out: [String: ItemStatus] = [:]
        for item in items {
            if let info = object[item.name] as? [String: Any], let latest = info["latest"] as? String,
               "v\(latest)" != item.detail {
                out[item.id] = .updateAvailable("\(item.detail) → v\(latest)")
            } else {
                out[item.id] = .upToDate
            }
        }
        return out
    }

    // MARK: - Update

    static func update(_ item: UpdatableItem, postCommand: String?) async -> UpdateResult {
        let t = Transcript()
        let q = Shell.quote(item.name)
        var ok = false

        switch item.kind {
        case .plugin:
            if await Shell.run("command -v claude").ok {
                ok = await t.run("claude plugin marketplace update \(q)").ok
                if ok {
                    for plugin in item.plugins {
                        if !(await t.run("claude plugin update \(Shell.quote(plugin))").ok) { ok = false }
                    }
                }
            } else if let path = item.path {
                t.note("`claude` wasn't found on PATH, so only the marketplace checkout was pulled.")
                ok = await t.run("\(git) -C \(Shell.quote(path)) pull --ff-only").ok
            } else {
                t.note("`claude` wasn't found on PATH.")
            }
        case .gitSkill:
            if let path = item.path {
                ok = await t.run("\(git) -C \(Shell.quote(path)) pull --ff-only").ok
            }
        case .uv:
            ok = await t.run("uv tool upgrade \(q)").ok
        case .pipx:
            ok = await t.run("pipx upgrade \(q)").ok
        case .npm:
            ok = await t.run("npm install -g \(Shell.quote(item.name + "@latest"))").ok
        case .claudeCode:
            let before = await claudeVersion()
            if isCaskInstall(item) {
                let latest = await caskLatestVersion()
                ok = await t.run("brew upgrade --cask claude-code").ok
                // brew can exit 0 without installing anything when its own data is stale. Only complain
                // when a newer version was actually on offer; Update Now on a current install is fine.
                if ok, let before, let latest, latest != before, await claudeVersion() == before {
                    t.note("Still at \(before) although \(latest) is out, so nothing was installed.")
                    ok = false
                }
            } else {
                ok = await t.run("claude update").ok
            }
        case .codexPlugin:
            ok = await t.run("codex plugin marketplace upgrade \(q)").ok
        case .geminiExtension:
            // Checked here, not read from item.status: the caller has already marked the item .updating.
            var before: ItemStatus?
            if let path = item.path { before = await geminiExtensionStatus(path) }
            ok = await t.run("gemini extensions update \(q)", timeout: 300).ok
            // `gemini extensions update` exits 0 even when it fails or a consent prompt ([Y/n]) got no answer
            // (stdin is closed), so only a re-check shows whether anything was installed.
            if ok, let path = item.path, case .updateAvailable(let offer)? = before {
                switch await geminiExtensionStatus(path) {
                case .upToDate: break
                case .updateAvailable(let now) where now != offer: break
                case .updateAvailable:
                    t.note("Gemini CLI didn't install it; it may need your consent. Run `gemini extensions update \(item.name)` in a terminal.")
                    ok = false
                default:
                    t.note("Couldn't confirm the update; check `gemini extensions list`.")
                    ok = false
                }
            }
        case .antigravityPlugin:
            // Installing again over the existing checkout updates it in place and keeps its enabled state;
            // uninstalling first would lose the plugin if the install then failed.
            var installURL: String?
            if let path = item.path {
                let origin = await Shell.run("\(git) -C \(Shell.quote(path)) remote get-url origin", timeout: 30)
                if origin.ok { installURL = antigravityInstallURL(origin.output) }
            }
            guard let path = item.path, let url = installURL else {
                t.note("\(item.name) wasn't installed from a git repo PullDay can reinstall from.")
                break
            }
            ok = await t.run("agy plugin install \(Shell.quote(url))", timeout: 300).ok
            if ok, case .updateAvailable = await antigravityPluginStatus(path) {
                t.note("agy finished, but \(item.name) is still behind its repo.")
                ok = false
            }
        case .antigravityCLI:
            let before = await agyVersion()
            let latest = await agyLatestVersion()
            ok = await t.run("agy update", timeout: 300).ok
            if ok, let before, let latest, latest != before, await agyVersion() == before {
                t.note("Still at \(before) although \(latest) is out, so nothing was installed.")
                ok = false
            }
        case .codexCLI:
            // `codex update` picks Homebrew, npm or the install script itself.
            let before = await codexVersion()
            let latest = await codexLatestVersion(item)
            ok = await t.run("codex update").ok
            if ok, let before, let latest, latest != before, await codexVersion() == before {
                t.note("Still at \(before) although \(latest) is out, so nothing was installed.")
                ok = false
            }
        }

        if ok, let command = postCommand?.trimmingCharacters(in: .whitespacesAndNewlines), !command.isEmpty {
            if !(await t.run(command).ok) {
                return UpdateResult(ok: false, summary: "Updated, but the post-update command failed", output: t.text)
            }
        }

        let summary: String
        if ok {
            switch item.kind {
            case .plugin: summary = "Updated. Restart Claude Code to load it"
            case .codexPlugin: summary = "Updated. Restart Codex to load it"
            case .geminiExtension: summary = "Updated. Restart Gemini CLI to load it"
            case .antigravityPlugin: summary = "Updated. Restart Antigravity to load it"
            case .claudeCode, .codexCLI, .antigravityCLI: summary = "Updated. New terminal sessions use it"
            default: summary = "Updated"
            }
        } else {
            summary = t.lastFailure ?? "Update failed"
        }
        return UpdateResult(ok: ok, summary: summary, output: t.text)
    }

    // MARK: - Helpers

    static func statuses(_ items: [UpdatableItem], _ status: ItemStatus) -> [String: ItemStatus] {
        Dictionary(uniqueKeysWithValues: items.map { ($0.id, status) })
    }

    static func isDirectory(_ path: String) -> Bool {
        var isDir: ObjCBool = false
        return FileManager.default.fileExists(atPath: path, isDirectory: &isDir) && isDir.boolValue
    }

    static func tilde(_ path: String) -> String {
        path.hasPrefix(home) ? "~" + path.dropFirst(home.count) : path
    }

    static func normalizePackage(_ name: String) -> String {
        name.lowercased().replacingOccurrences(of: "_", with: "-").replacingOccurrences(of: ".", with: "-")
    }

    /// Pulls the JSON out of output that may have warnings around it.
    static func extractJSON(_ text: String, open: Character, close: Character) -> Data? {
        guard let start = text.firstIndex(of: open), let end = text.lastIndex(of: close), start < end else { return nil }
        return String(text[start...end]).data(using: .utf8)
    }
}
