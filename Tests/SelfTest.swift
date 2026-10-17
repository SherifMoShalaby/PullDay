import Foundation

// Assert-based checks for Scanner/Prefs logic that's easy to break silently. No network: fixtures
// live in a temp folder and `gemini` is a fake script. Run with `./build.sh --test`.

var failures = 0

func check(_ ok: Bool, _ what: String, line: Int = #line) {
    if !ok { failures += 1; print("FAIL (SelfTest.swift:\(line)) \(what)") }
}

let fm = FileManager.default
// Unresolved temp path, like a real home: resolvingSymlinksInPath turns /private/var back into /var.
let tmp = fm.temporaryDirectory.appendingPathComponent("pullday-selftest-\(getpid())").path

func mkdir(_ path: String) { try? fm.createDirectory(atPath: tmp + "/" + path, withIntermediateDirectories: true) }
func write(_ path: String, _ text: String) { mkdir((path as NSString).deletingLastPathComponent); fm.createFile(atPath: tmp + "/" + path, contents: Data(text.utf8)) }
func link(_ path: String, to target: String) { try? fm.createSymbolicLink(atPath: tmp + "/" + path, withDestinationPath: tmp + "/" + target) }

@main struct SelfTest {
    static func main() async {
        defer { try? fm.removeItem(atPath: tmp) }
        modelsGolden()
        await scannerGolden()
        remoteRefs()
        sourceURLs()
        gitSkillDiscovery()
        await migration()
        await codexRecords()
        await geminiUpdateGuard()
        await antigravity()
        await optionLikeNames()
        uvPins()
        unsetenv("ZDOTDIR")
        await backgroundChildDoesNotHang()
        print(failures == 0 ? "SelfTest: all checks passed" : "SelfTest: \(failures) failed")
        exit(failures == 0 ? 0 : 1)
    }

    /// The Rust port (rust/pullday-core) checks the same file, so the two stay in step.
    static func modelsGolden() {
        func names(_ agents: Set<Agent>) -> String { Agent.allCases.filter(agents.contains).map(\.rawValue).joined(separator: ",") }
        var lines = SourceKind.allCases.map {
            ["kind", $0.rawValue, $0.title, $0.symbol, $0.explanation, "\($0.enabledByDefault)", names($0.agents)].joined(separator: "\t")
        }
        lines += Agent.allCases.map { ["agent", $0.rawValue, $0.title, $0.homeDir].joined(separator: "\t") }
        lines += Agent.skillRootReaders.map { ["root", $0.root, names($0.agents)].joined(separator: "\t") }
        var items = SourceKind.allCases.flatMap { k in ["x", "graphifyy"].map { UpdatableItem(kind: k, name: $0, detail: "") } }
        items.append(UpdatableItem(kind: .gitSkill, name: "x", detail: "", id: "gitSkill:/home/u/.claude/skills/x"))
        lines += items.map { ["item", $0.id, names($0.agents)].joined(separator: "\t") }
        golden("models.golden", lines)
    }

    /// Compares with a file in Tests/ that the Rust tests check too; WRITE_GOLDEN=1 rewrites it after a deliberate change.
    static func golden(_ name: String, _ lines: [String], line: Int = #line) {
        let dump = lines.joined(separator: "\n") + "\n"
        let path = (#filePath as NSString).deletingLastPathComponent + "/" + name
        if ProcessInfo.processInfo.environment["WRITE_GOLDEN"] != nil { fm.createFile(atPath: path, contents: Data(dump.utf8)) }
        check((try? String(contentsOfFile: path, encoding: .utf8)) == dump, "matches Tests/\(name) (rerun with WRITE_GOLDEN=1 after a deliberate change)", line: line)
    }

    /// The same fixture home is scanned by rust/pullday-core/tests/parity.rs; both must print Tests/scanner.golden.
    /// Only sources whose folders Swift takes as parameters (and that need no network) are in it.
    static func scannerGolden() async {
        let base = tmp + "/parity"
        let dirs = ["home/.claude/skills/claudeonly/.git", "home/repos/both/.git", "home/.agents/skills", "home/.gemini/.git",
                    "home/.gemini/skills/plain", "home/.config/.git", "home/.config/opencode/skills/plain", "home/dotfiles/.git",
                    "home/dotfiles/cursor/skills/plain", "codex/mk/norecord", "codex/mk/.staging", "codex/cache/local/superpowers"]
        let links = [("home/.claude/skills/both", "home/repos/both"), ("home/.agents/skills/both", "home/repos/both"),
                     ("home/.cursor", "home/dotfiles/cursor")]
        let local = "{\"source\":\"\(base)/gemini/src\",\"type\":\"local\"}"
        let files = [
            ("codex/mk/local/.codex-marketplace-install.json", #"{"source_type":"local","source":"/x","revision":"abc"}"#),
            ("codex/mk/norev/.codex-marketplace-install.json", #"{"source_type":"git","source":"o/r","ref_name":"main"}"#),
            ("gemini/src/gemini-extension.json", #"{"name":"src","version":"2.0.0"}"#),
            ("gemini/ext/current/gemini-extension.json", #"{"name":"current","version":"2.0.0"}"#),
            ("gemini/ext/current/.gemini-extension-install.json", local),
            ("gemini/ext/old/gemini-extension.json", #"{"name":"old","version":"1.0.0"}"#),
            ("gemini/ext/old/.gemini-extension-install.json", local),
            ("gemini/ext/linked/gemini-extension.json", #"{"name":"linked"}"#),
            ("gemini/ext/linked/.gemini-extension-install.json", #"{"source":"/x","type":"link"}"#),
            ("gemini/ext/moved/gemini-extension.json", #"{"name":"moved","migratedTo":"https://github.com/n/r"}"#),
            ("gemini/ext/moved/.gemini-extension-install.json", #"{"source":"o/r","type":"git"}"#),
            ("gemini/ext/gitplain/gemini-extension.json", #"{"name":"gitplain","version":"1.0.0"}"#),
            ("gemini/ext/gitplain/.gemini-extension-install.json", #"{"source":"o/r","type":"git"}"#),
            ("gemini/ext/odd/gemini-extension.json", #"{"name":"odd"}"#),
            ("gemini/ext/odd/.gemini-extension-install.json", #"{"type":"tarball"}"#),
            ("gemini/ext/bare/gemini-extension.json", #"{"name":"bare","version":"1"}"#),
            ("gemini/ext/nameless/gemini-extension.json", #"{"version":"1"}"#),
        ]
        for dir in dirs { mkdir("parity/" + dir) }
        for (path, target) in links { link("parity/" + path, to: "parity/" + target) }
        for (path, text) in files { write("parity/" + path, text) }
        let agyList = #"[{"name": "ponytail", "enabled": true, "version": "5.3.1", "path": "/home/u/.gemini/config/plugins/ponytail"},"#
            + #" {"name": "alpha", "enabled": false, "path": "/home/u/.gemini/config/plugins/alpha"}]"#

        func text(_ status: ItemStatus?) -> String {
            guard let status else { return "-" }
            switch status {
            case .unknown(let why): return why.map { "unknown: " + $0 } ?? "unknown"
            case .checking: return "checking"
            case .upToDate: return "upToDate"
            case .updateAvailable(let offer): return offer.map { "updateAvailable: " + $0 } ?? "updateAvailable"
            case .pinned(let s): return "pinned: " + s
            case .skipped(let s): return "skipped: " + s
            case .updating: return "updating"
            case .updated(let s): return "updated: " + s
            case .failed(let s): return "failed: " + s
            }
        }
        func row(_ item: UpdatableItem, _ status: ItemStatus?) -> String {
            let agents = Agent.allCases.filter(item.agents.contains).map(\.rawValue).joined(separator: ",")
            return ["item", item.id, item.detail, agents, "\(item.manualOnly)", text(status)].joined(separator: "\t")
        }
        // git must not walk up out of the fixture (a temp folder inside a repo would give gitplain that repo's HEAD).
        setenv("GIT_CEILING_DIRECTORIES", base, 1)
        defer { unsetenv("GIT_CEILING_DIRECTORIES") }
        var lines = Scanner.discoverGitSkills(home: base + "/home").map { row($0, nil) }
        let codex = Scanner.discoverCodexPlugins(dir: base + "/codex/mk", cacheDir: base + "/codex/cache")
        let codexStatuses = await Scanner.check(.codexPlugin, items: codex)
        lines += codex.map { row($0, codexStatuses[$0.id]) }
        for item in Scanner.discoverGeminiExtensions(dir: base + "/gemini/ext") {
            lines.append(row(item, await Scanner.geminiExtensionStatus(item.path ?? "")))
        }
        for item in Scanner.antigravityPlugins(fromList: agyList) {
            lines.append(row(item, await Scanner.antigravityPluginStatus(item.path ?? "")))
        }
        // The fixture's place differs per run and machine; Swift's tilde() is relative to the real home.
        // The ~ forms first, for a temp folder inside the real home.
        let resolved = (base as NSString).resolvingSymlinksInPath
        let forms = [Scanner.tilde(resolved), Scanner.tilde(base), resolved, base]
        golden("scanner.golden", lines.map { line in
            forms.reduce(line) { $0.replacingOccurrences(of: $1, with: "<tmp>") }
                .replacingOccurrences(of: "<tmp>/home", with: "~")
        })
    }

    static func remoteRefs() {
        let ls = """
        1111111111111111111111111111111111111111\trefs/heads/feature/main
        2222222222222222222222222222222222222222\trefs/heads/main
        3333333333333333333333333333333333333333\trefs/tags/v1
        4444444444444444444444444444444444444444\trefs/tags/v1^{}
        5555555555555555555555555555555555555555\tHEAD
        """
        check(Scanner.pickRemoteSHA(ls, ref: "main")?.hasPrefix("2222") == true, "a branch, not a branch merely ending in its name")
        check(Scanner.pickRemoteSHA(ls, ref: "v1")?.hasPrefix("4444") == true, "an annotated tag gives its commit (peeled ^{})")
        check(Scanner.pickRemoteSHA(ls, ref: "refs/tags/v1")?.hasPrefix("4444") == true, "a fully qualified annotated tag gives its commit")
        check(Scanner.pickRemoteSHA(ls, ref: "refs/heads/main")?.hasPrefix("2222") == true, "a fully qualified branch")
        check(Scanner.pickRemoteSHA(ls, ref: "HEAD")?.hasPrefix("5555") == true, "HEAD")
        check(Scanner.pickRemoteSHA(ls, ref: "nope") == nil, "an unknown ref finds nothing")
        check(Scanner.isCommitSHA(String(repeating: "a", count: 40)) && !Scanner.isCommitSHA("main"), "commit SHA detection")
    }

    static func sourceURLs() {
        check(Scanner.codexSourceURL("openai/plugins") == "https://github.com/openai/plugins.git", "Codex owner/repo shorthand")
        check(Scanner.codexSourceURL("github.com/a/b") == "https://github.com/a/b", "Codex github.com/ without a scheme")
        check(Scanner.codexSourceURL("git@github.com:a/b.git") == "git@github.com:a/b.git", "Codex ssh source kept as written")
        check(Scanner.codexSourceURL("./local/x") == "./local/x", "Codex local path kept as written")
        let repos: [String: String?] = [
            "o/r": "o/r", "https://github.com/o/r.git": "o/r", "git@github.com:o/r.git": "o/r", "github.com/o/r": "o/r",
            "git@gitlab.com:o/r": nil, "https://gitlab.com/o/r": nil,
        ]
        for (source, expected) in repos {
            check(Scanner.githubRepo(source).map { "\($0.0)/\($0.1)" } == expected, "githubRepo(\(source))")
        }
    }

    /// Consumer sets, manual-only for outside repos, and never treating a skill root or an ancestor as the repo.
    static func gitSkillDiscovery() {
        let home = tmp + "/home"
        mkdir("home/.claude/skills/claudeonly/.git")
        mkdir("home/repos/both/.git")
        link("home/.claude/skills/both", to: "home/repos/both")
        mkdir("home/.agents/skills")
        link("home/.agents/skills/both", to: "home/repos/both")
        mkdir("home/.gemini/.git")                        // a dotfiles repo at an agent's home
        mkdir("home/.gemini/skills/plain")
        mkdir("home/.config/.git")                        // ~/.config as a repo
        mkdir("home/.config/opencode/skills/plain")
        mkdir("home/dotfiles/.git")                       // stow-style: ~/.cursor links into a dotfiles repo
        mkdir("home/dotfiles/cursor/skills/plain")
        link("home/.cursor", to: "home/dotfiles/cursor")

        let items = Scanner.discoverGitSkills(home: home)
        let byName = Dictionary(uniqueKeysWithValues: items.map { ($0.name, $0) })
        check(Set(byName.keys) == ["claudeonly", "both"], "only real skill repos are found, got \(byName.keys.sorted())")
        check(byName["claudeonly"]?.agents == [.claude, .opencode], "~/.claude/skills → Claude Code and OpenCode")
        check(byName["claudeonly"]?.manualOnly == false, "a repo inside ~/.claude is auto-updatable")
        check(byName["both"]?.agents == [.claude, .codex, .gemini, .opencode, .cursor], "a repo in two roots gets both readers")
        check(byName["both"]?.manualOnly == true, "a repo outside every agent folder is manual updates only")
        check(byName["claudeonly"]?.id == "gitSkill:" + (home as NSString).resolvingSymlinksInPath + "/.claude/skills/claudeonly",
              "item id stays gitSkill:<repo path>")
    }

    /// New default-on kinds turn on once; one the user turns off stays off; npm stays off.
    @MainActor static func migration() async {
        let suite = "dev.pullday.selftest.\(getpid())"
        let d = UserDefaults(suiteName: suite)!
        defer { d.removePersistentDomain(forName: suite) }
        d.set(["plugin"], forKey: "enabledKinds")
        d.set(["plugin"], forKey: "knownKinds")
        let first = Prefs(defaults: d)
        check(first.enabledKinds.contains("uv") && first.enabledKinds.contains("codexCLI"), "new kinds are turned on")
        check(!first.enabledKinds.contains("npm"), "npm stays off")
        first.enabledKinds.remove("uv")
        check(!Prefs(defaults: d).enabledKinds.contains("uv"), "a kind turned off stays off")

        let legacySuite = suite + ".legacy"
        let e = UserDefaults(suiteName: legacySuite)!
        defer { e.removePersistentDomain(forName: legacySuite) }
        e.set(["plugin"], forKey: "enabledKinds")   // no knownKinds: shipped before the migration existed
        let legacy = Prefs(defaults: e).enabledKinds
        check(!legacy.contains("uv") && legacy.contains("codexPlugin"), "legacy kinds untouched, newer kinds added")
        check(Prefs(defaults: e).enabledKinds == legacy, "the migration runs once")

        // Settings saved under the old SkillSync name are copied once, on the first launch, and left in place.
        let oldName = suite + ".old", newName = suite + ".new"
        let old = UserDefaults(suiteName: oldName)!, new = UserDefaults(suiteName: newName)!
        defer { old.removePersistentDomain(forName: oldName); new.removePersistentDomain(forName: newName) }
        old.set(true, forKey: "autoUpdate")
        old.set(["plugin", "uv"], forKey: "enabledKinds")
        Prefs.copySettings(fromDomain: oldName, into: new)
        check(new.bool(forKey: "autoUpdate") && new.stringArray(forKey: "enabledKinds") == ["plugin", "uv"], "old settings are copied")
        new.set(["plugin"], forKey: "knownKinds")   // what the first Prefs init writes
        new.set(false, forKey: "autoUpdate")
        Prefs.copySettings(fromDomain: oldName, into: new)
        check(!new.bool(forKey: "autoUpdate"), "the copy happens only on the first launch")
        check(old.bool(forKey: "autoUpdate"), "the old domain is left as it is")
    }

    static func codexRecords() async {
        write("codex/mk/local/.codex-marketplace-install.json", #"{"source_type":"local","source":"/x","revision":"abc"}"#)
        write("codex/mk/norev/.codex-marketplace-install.json", #"{"source_type":"git","source":"o/r","ref_name":"main"}"#)
        mkdir("codex/mk/norecord")
        mkdir("codex/mk/.staging")
        mkdir("codex/cache/local/superpowers")
        let items = Scanner.discoverCodexPlugins(dir: tmp + "/codex/mk", cacheDir: tmp + "/codex/cache")
        check(items.map(\.name) == ["local", "norecord", "norev"], ".staging is skipped")
        check(items.first?.detail == "superpowers", "installed plugins come from the cache folder")
        let statuses = await Scanner.check(.codexPlugin, items: items)
        check(statuses["codexPlugin:local"] == .unknown("Local marketplace; nothing to update"), "local marketplace")
        check(statuses["codexPlugin:norev"] == .unknown("Installed revision unknown"), "missing revision")
        check(statuses["codexPlugin:norecord"] == .unknown("No install record"), "missing record")
    }

    /// `gemini extensions update` exits 0 even when it installed nothing; the re-check must catch that,
    /// with the item marked .updating as AppState.update passes it.
    static func geminiUpdateGuard() async {
        write("gemini/src/gemini-extension.json", #"{"name":"src","version":"2.0.0"}"#)
        write("gemini/ext/demo/.gemini-extension-install.json", "{\"source\":\"\(tmp)/gemini/src\",\"type\":\"local\"}")
        write("gemini/bin/gemini", """
            #!/bin/sh
            [ "$SELFTEST_GEMINI_INSTALLS" = 1 ] && cp "\(tmp)/gemini/src/gemini-extension.json" "\(tmp)/gemini/ext/demo/gemini-extension.json"
            exit 0
            """)
        chmod(tmp + "/gemini/bin/gemini", 0o755)
        // Commands run through a login shell, whose profile (path_helper, Homebrew) can put a real `gemini` first.
        // A zsh with its own ZDOTDIR prepends the fake one after every profile has run.
        write("gemini/zdot/.zprofile", "export PATH=\"\(tmp)/gemini/bin:$PATH\"\n")
        setenv("SHELL", "/bin/zsh", 1)
        setenv("ZDOTDIR", tmp + "/gemini/zdot", 1)

        func run(installs: Bool) async -> UpdateResult {
            write("gemini/ext/demo/gemini-extension.json", #"{"name":"demo","version":"1.0.0"}"#)
            setenv("SELFTEST_GEMINI_INSTALLS", installs ? "1" : "0", 1)
            var item = Scanner.discoverGeminiExtensions(dir: tmp + "/gemini/ext")[0]
            item.status = .updating
            return await Scanner.update(item, postCommand: nil)
        }
        let noop = await run(installs: false)
        check(!noop.ok && noop.summary.contains("consent"), "a no-op update fails with the consent note, got: \(noop.summary)")
        let real = await run(installs: true)
        check(real.ok, "a real update succeeds, got: \(real.summary)")
    }

    /// `agy plugin list --json` as Antigravity CLI 1.4.0 prints it (captured from a real install), plus the Go
    /// logger's stderr noise; the plugin update must never hand `agy` a local path or an option.
    static func antigravity() async {
        let list = """
        ERROR: logging before google.Init: I1016 21:58:38.362951       1 cli.go:27] starting
        [
          {"name": "ponytail", "enabled": true, "version": "5.3.1", "source": "antigravity",
           "path": "/home/u/.gemini/config/plugins/ponytail", "skills": ["ponytail", "ponytail-review"]},
          {"name": "alpha", "enabled": false, "source": "antigravity", "path": "/home/u/.gemini/config/plugins/alpha"}
        ]
        """
        let items = Scanner.antigravityPlugins(fromList: list)
        check(items.map(\.name) == ["alpha", "ponytail"], "agy plugins are parsed out of the log noise and sorted")
        check(items.last?.detail == "v5.3.1" && items.last?.path == "/home/u/.gemini/config/plugins/ponytail", "version and path")
        check(items.first?.detail == "Version unknown · disabled", "a disabled plugin says so")
        check(items.allSatisfy { $0.agents == [.antigravity] && $0.id.hasPrefix("antigravityPlugin:") }, "agent and id")
        check(Scanner.antigravityPlugins(fromList: "agy: not logged in").isEmpty, "no JSON, no items")

        for url in ["https://github.com/o/r.git", "git@github.com:o/r.git", "ssh://git@host/o/r"] {
            check(Scanner.antigravityInstallURL(url + "\n") == url, "\(url) can be reinstalled from")
        }
        for url in ["/home/u/src/plugin", "./plugin", "-oProxyCommand=touch /tmp/x", "file:///tmp/r", "https://x y"] {
            check(Scanner.antigravityInstallURL(url) == nil, "\(url) is never passed to agy")
        }

        // A fake agy on PATH (the gemini check set up ZDOTDIR to put tmp/gemini/bin first).
        write("gemini/bin/agy", """
            #!/bin/sh
            case "$1" in
              --version) echo "ERROR: logging before google.Init: I1016 cli.go:27] x" >&2; echo "1.4.0";;
              plugin) [ "$2" = install ] && touch "\(tmp)/agy-install-ran";;
            esac
            exit 0
            """)
        chmod(tmp + "/gemini/bin/agy", 0o755)
        check(await Scanner.agyVersion() == "1.4.0", "agy's version is read past its log line")
        check(await Scanner.discoverAntigravityCLI().first?.detail == "v1.4.0", "the agy item shows its version")

        // A plugin whose origin is a local path: the update refuses and agy is never run.
        mkdir("agy/plugins/local/.git")
        _ = await Shell.run("git -C \(Shell.quote(tmp + "/agy/plugins/local")) init -q && git -C \(Shell.quote(tmp + "/agy/plugins/local")) remote add origin /some/local/path")
        var local = UpdatableItem(kind: .antigravityPlugin, name: "local", detail: "v1", path: tmp + "/agy/plugins/local")
        local.status = .updating
        let refused = await Scanner.update(local, postCommand: nil)
        check(!refused.ok && !fm.fileExists(atPath: tmp + "/agy-install-ran"), "a local-path origin is never reinstalled, got: \(refused.summary)")
        check(await Scanner.antigravityPluginStatus(tmp + "/agy/plugins") == .unknown("Not installed from a git repo"), "a non-git plugin can't be checked")
    }

    /// Exact pins and URL requirements (the specify-cli line is from a real uv) aren't offered as updates.
    static func uvPins() {
        let output = """
        graphifyy v0.5.1 [latest: 0.6.0]
        - graphify
        pinned v1.0.0 [required: ==1.0.0] [latest: 1.2.0]
        wild v0.3.1 [required: ==0.3.*] [latest: 0.4.0]
        specify-cli v0.4.3 [required:  git+https://github.com/github/spec-kit.git@v0.4.3] [latest: 1.2.1]
        """
        let items = ["graphifyy", "pinned", "wild", "specify-cli", "current"].map { UpdatableItem(kind: .uv, name: $0, detail: "v0.5.1") }
        let s = Scanner.uvStatuses(output, items)
        check(s["uv:graphifyy"] == .updateAvailable("v0.5.1 → v0.6.0"), "an update")
        check(s["uv:pinned"] == .pinned("Pinned to ==1.0.0 · v1.2.0 available"), "an exact pin")
        check(s["uv:wild"]?.isUpdateAvailable == true, "a wildcard can still move")
        check(s["uv:specify-cli"] == .pinned("Installed from git+https://github.com/github/spec-kit.git@v0.4.3 · v1.2.1 on PyPI"),
              "a git install isn't offered the PyPI release, got: \(String(describing: s["uv:specify-cli"]))")
        check(s["uv:current"] == .upToDate, "not listed: up to date")
    }

    /// A name like `--all` (from an extension's manifest, say) would be read as an option, so update runs nothing.
    static func optionLikeNames() async {
        let item = UpdatableItem(kind: .geminiExtension, name: "--all", detail: "", path: tmp + "/nowhere")
        let result = await Scanner.update(item, postCommand: "touch \(Shell.quote(tmp + "/post-ran"))")
        check(!result.ok && result.summary.contains("won't pass the name --all") && !result.output.contains("$ ")
              && !fm.fileExists(atPath: tmp + "/post-ran"), "an option-looking name runs no command, got: \(result.summary)")
    }

    /// A command that leaves a background child holding the output pipe (a post-update command that starts a
    /// daemon, say) must not keep the run waiting for that child: the run ends soon after the shell exits.
    static func backgroundChildDoesNotHang() async {
        let start = Date()
        let result = await Shell.run("(sleep 20 &); echo started", timeout: 60)
        let seconds = Date().timeIntervalSince(start)
        check(seconds < 10, "a background child holding the pipe doesn't block the run, took \(Int(seconds))s")
        check(result.ok && result.output.contains("started"), "output written before the shell exits is kept, got: \(result.output)")
        let big = await Shell.run("head -c 300000 /dev/zero | tr '\\0' x", timeout: 60)
        check(big.output.count == 300_000, "output larger than the pipe buffer arrives whole, got \(big.output.count) bytes")
    }
}
