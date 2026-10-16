import Foundation

struct ShellResult: Sendable {
    let status: Int32
    let output: String
    var timedOut = false

    var ok: Bool { status == 0 && !timedOut }
    var lines: [String] { output.split(whereSeparator: \.isNewline).map(String.init) }

    func lastLine(or fallback: String) -> String {
        if timedOut { return "Timed out" }
        return lines.last.map { $0.trimmingCharacters(in: .whitespaces) } ?? fallback
    }
}

/// Runs commands through the user's login shell, so tools installed with
/// Homebrew, uv, pipx, npm, nvm etc. are found even though this is a GUI app.
enum Shell {
    static let home = FileManager.default.homeDirectoryForCurrentUser.path

    static let extraPaths: [String] = {
        var paths = [
            "\(home)/.local/bin",
            "\(home)/.claude/local",
            "/opt/homebrew/bin",
            "/opt/homebrew/sbin",
            "/usr/local/bin",
            "\(home)/.cargo/bin",
            "\(home)/.npm-global/bin",
            "\(home)/.bun/bin",
            "\(home)/.volta/bin",
            "/usr/bin", "/bin", "/usr/sbin", "/sbin",
        ]
        // Newest nvm-managed Node, since nvm is usually only loaded in .zshrc.
        let nvm = "\(home)/.nvm/versions/node"
        if let versions = try? FileManager.default.contentsOfDirectory(atPath: nvm),
           let newest = versions.filter({ $0.hasPrefix("v") }).sorted(by: { $0.compare($1, options: .numeric) == .orderedAscending }).last {
            paths.append("\(nvm)/\(newest)/bin")
        }
        return paths
    }()

    static var loginShell: String {
        // `quote` is POSIX-only, so a non-POSIX shell (fish, nu, …) falls back to zsh.
        let posix: Set<String> = ["sh", "bash", "zsh", "dash", "ksh"]
        if let shell = ProcessInfo.processInfo.environment["SHELL"], !shell.isEmpty,
           posix.contains((shell as NSString).lastPathComponent),
           FileManager.default.isExecutableFile(atPath: shell) {
            return shell
        }
        return "/bin/zsh"
    }

    static func run(_ command: String, timeout: TimeInterval = 300) async -> ShellResult {
        await withCheckedContinuation { continuation in
            DispatchQueue.global(qos: .utility).async {
                continuation.resume(returning: runSync(command, timeout: timeout))
            }
        }
    }

    static func runSync(_ command: String, timeout: TimeInterval) -> ShellResult {
        let process = Process()
        process.executableURL = URL(fileURLWithPath: loginShell)
        // Append (not prepend) so whatever the user's profile sets up wins.
        let path = extraPaths.joined(separator: ":")
        process.arguments = ["-l", "-c", "export PATH=\"$PATH:\(path)\"; \(command)"]

        var env = ProcessInfo.processInfo.environment
        env["TERM"] = "dumb"
        env["NO_COLOR"] = "1"
        env["GIT_TERMINAL_PROMPT"] = "0"                 // never hang on a password prompt
        env["GIT_SSH_COMMAND"] = "ssh -o BatchMode=yes"
        env["PIP_DISABLE_PIP_VERSION_CHECK"] = "1"
        env["npm_config_fund"] = "false"
        env["npm_config_update_notifier"] = "false"
        process.environment = env

        let pipe = Pipe()
        process.standardOutput = pipe
        process.standardError = pipe
        process.standardInput = FileHandle.nullDevice

        do {
            try process.run()
        } catch {
            return ShellResult(status: -1, output: "Couldn't start \(loginShell): \(error.localizedDescription)")
        }
        // Process starts the shell as its own process-group leader, so signalling -pid
        // on timeout also reaches the commands it spawned (git, npm, …), not just the shell.
        let pid = process.processIdentifier

        let lock = NSLock()
        var didTimeOut = false
        let killer = DispatchWorkItem {
            if process.isRunning {
                lock.lock(); didTimeOut = true; lock.unlock()
                kill(-pid, SIGTERM)
            }
        }
        DispatchQueue.global().asyncAfter(deadline: .now() + timeout, execute: killer)

        // Read as output arrives rather than to end-of-file: a child the command left running in the background
        // (a post-update command starting a daemon, say) can hold the pipe open long after the shell exits.
        let reader = pipe.fileHandleForReading
        var data = Data()
        let endOfFile = DispatchSemaphore(value: 0)
        reader.readabilityHandler = { handle in
            let chunk = handle.availableData
            if chunk.isEmpty {
                handle.readabilityHandler = nil
                endOfFile.signal()
            } else {
                lock.lock(); data.append(chunk); lock.unlock()
            }
        }
        process.waitUntilExit()
        killer.cancel()
        // End-of-file normally follows the exit at once; give up on it if a background child still holds the pipe.
        // The pipe closes the handle when it's freed; closing it here could race a read still in flight.
        if endOfFile.wait(timeout: .now() + 2) == .timedOut {
            reader.readabilityHandler = nil
        }

        lock.lock(); let timedOut = didTimeOut; let output = String(decoding: data, as: UTF8.self); lock.unlock()
        let trimmed = output.trimmingCharacters(in: .whitespacesAndNewlines)
        return ShellResult(status: process.terminationStatus, output: trimmed, timedOut: timedOut)
    }

    /// Single-quotes a value for safe use inside a shell command.
    static func quote(_ value: String) -> String {
        "'" + value.replacingOccurrences(of: "'", with: "'\\''") + "'"
    }
}

/// Collects the commands run for one update, for the log.
final class Transcript {
    private var parts: [String] = []
    private(set) var lastFailure: String?

    func run(_ command: String, timeout: TimeInterval = 600) async -> ShellResult {
        let result = await Shell.run(command, timeout: timeout)
        let body = result.output.isEmpty ? "(no output)" : result.output
        parts.append("$ \(command)\n\(body)\n→ \(result.timedOut ? "timed out" : "exit \(result.status)")")
        if !result.ok { lastFailure = result.lastLine(or: "Command failed: \(command)") }
        return result
    }

    func note(_ text: String) {
        parts.append(text)
        lastFailure = text
    }

    var text: String { parts.joined(separator: "\n\n") }
}

enum LogFile {
    static let url = FileManager.default.homeDirectoryForCurrentUser
        .appendingPathComponent("Library/Logs/PullDay.log")

    static func append(_ entry: LogEntry) {
        let stamp = ISO8601DateFormatter().string(from: entry.date)
        let text = "[\(stamp)] \(entry.success ? "OK  " : "FAIL") \(entry.title)\n\(entry.output)\n\n"
        guard let data = text.data(using: .utf8) else { return }

        let fm = FileManager.default
        // Keep the file small: start over once it passes 2 MB.
        if let size = (try? fm.attributesOfItem(atPath: url.path))?[.size] as? Int, size > 2_000_000 {
            try? fm.removeItem(at: url)
        }
        if let handle = try? FileHandle(forWritingTo: url) {
            handle.seekToEndOfFile()
            handle.write(data)
            try? handle.close()
        } else {
            try? data.write(to: url)
        }
    }
}
