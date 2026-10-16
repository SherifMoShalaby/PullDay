# Contributing to PullDay

Thanks for helping. PullDay is small on purpose: a native menu bar app in about 2,000 lines of Swift, no dependencies, no Xcode project. Please keep it that way.

## Build and test

You need macOS 13 or later and the Xcode Command Line Tools (`xcode-select --install`).

```bash
./build.sh --test      # compile and run Tests/SelfTest.swift (no network)
./build.sh             # build build/PullDay.app (universal, ad-hoc signed)
./build.sh --install   # also copy it to /Applications and launch it
```

While you iterate, a typecheck is the fastest feedback:

```bash
swiftc -typecheck -swift-version 5 -parse-as-library -target arm64-apple-macos13.0 Sources/*.swift
```

Every command the app runs, and its output, lands in `~/Library/Logs/PullDay.log`. Keep `tail -f` on it while you test. To try a scanner command by hand, run it the way the app does: `zsh -l -c '…' </dev/null`.

## How the code is laid out

```
Sources/
  App.swift          @main app (MenuBarExtra), AppDelegate, Notifier, WindowManager, the menu bar glyph
  AppState.swift     @MainActor state: items, scheduling, folder watching, refresh(), update()
  Prefs.swift        Settings backed by UserDefaults
  Scanner.swift      Pure logic: discover, check and update each kind of source
  Shell.swift        Runs commands through the login shell; the update transcript; the log file
  Models.swift       SourceKind, Agent, ItemStatus, UpdatableItem
  MenuView.swift     The menu bar panel
  SettingsView.swift Settings tabs
  LogView.swift      Log window
Tests/SelfTest.swift Assert-based self-test of Scanner, Shell, Models and Prefs
Resources/           AppIcon.svg (the source) and AppIcon.icns
```

`CLAUDE.md` describes every source's check and update in detail, along with the design decisions behind them.

## Adding a source

1. Add a `SourceKind` case in `Models.swift` with its `title`, `symbol`, `explanation`, `enabledByDefault` and `agents`.
2. Add its arms to the three switches in `Scanner.swift`: `discover`, `check` and `update`.
3. Add a check to `Tests/SelfTest.swift` for any parsing or decision logic, using temp-folder fixtures or a fake binary, never the network.

New default-on sources are turned on once for existing users by the `knownKinds` migration in `Prefs`, so there's nothing else to do.

## Adding an agent

Add an `Agent` case with its `title` and `homeDir`, and list the skill folders it reads in `Agent.skillRootReaders`. The menu section, the Settings → Sources list and git-skill discovery follow from that.

## Conventions

- Swift 5 language mode, 4-space indentation. Match the style of the code around your change.
- Keep `Scanner` free of UI and actor state; only `AppState` mutates published state.
- Quote every value interpolated into a shell command with `Shell.quote`. Run git through `Scanner.git`, which turns off repository hooks and `file://` remotes.
- Never answer a password or consent prompt on the user's behalf.
- Use `@StateWrapper`, never `@State`: with the Command Line Tools SDK, `@State` is a SwiftUI macro whose plugin only ships with Xcode. `#Preview`, `@Entry` and `@Animatable` fail the same way.
- No third-party dependencies.

## Pull requests

- One change per pull request, with a sentence on why.
- `./build.sh --test` passes, and `./build.sh` builds without new warnings.
- If you changed what PullDay finds or runs, say how you checked it against a real install, and update the README.
- Add a line for users under `## Unreleased` in `CHANGELOG.md` if they'd notice the change. [RELEASING.md](RELEASING.md) covers the rest of a release.
