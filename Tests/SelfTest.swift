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
        remoteRefs()
        sourceURLs()
        gitSkillDiscovery()
        await migration()
        await codexRecords()
        await geminiUpdateGuard()
        await backgroundChildDoesNotHang()
        print(failures == 0 ? "SelfTest: all checks passed" : "SelfTest: \(failures) failed")
        exit(failures == 0 ? 0 : 1)
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
        unsetenv("ZDOTDIR")
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
