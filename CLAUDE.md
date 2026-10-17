# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

# PullDay

## What this is
A native macOS menu bar app (SwiftUI + AppKit, macOS 13+) that keeps the skills, plugins and extensions of the user's AI coding agents (Claude Code, Codex, Gemini CLI, OpenCode, Cursor) up to date. It discovers installed items, checks them for updates on a schedule, and updates them automatically or with one click.

## Build
No Xcode project. The app is built with plain `swiftc` from the Command Line Tools:

```bash
./build.sh            # → build/PullDay.app (universal arm64 + x86_64, ad-hoc signed)
./build.sh --install  # also copies to /Applications and launches
./build.sh --test     # compiles and runs Tests/SelfTest.swift (no app build)
./build.sh --dmg      # also packages build/PullDay.dmg (SIGN_IDENTITY / NOTARY_PROFILE to sign and notarize)
```

Flags: `-swift-version 5 -parse-as-library -target <arch>-apple-macos13.0`. Releases (a DMG and CHANGELOG notes on GitHub Releases, built by `.github/workflows/release.yml` from a `v*` tag) are described in `RELEASING.md`; user-visible changes go under `## Unreleased` in `CHANGELOG.md`. Keep Swift 5 language mode unless you deliberately migrate to strict concurrency.

Quick compile check while iterating (faster than the universal build):
```bash
swiftc -typecheck -swift-version 5 -parse-as-library -target arm64-apple-macos13.0 Sources/*.swift
```

There's no XCTest target or linter. `Tests/SelfTest.swift` is an assert-based self-test of the logic files (`Scanner`, `Shell`, `Models`, `Prefs`; never `App.swift`, which has the app's `@main`), using temp-folder fixtures and fake binaries, no network; add a check there when you add parsing or decision logic. Verify with `./build.sh --test`, typechecking, then `./build.sh --install` and exercising the menu; every command the app runs and its output lands in `~/Library/Logs/PullDay.log` (`tail -f` it while testing). To try the scanner's shell commands by hand, run them through `zsh -l -c '…' </dev/null`, which is how the app runs them.

## Layout
```
Sources/
  App.swift          @main app (MenuBarExtra, .window style), AppDelegate, Notifier, WindowManager
  AppState.swift     @MainActor ObservableObject: items, phase, log, scheduling, refresh(), update()
  Prefs.swift        @MainActor ObservableObject backed by UserDefaults (interval, autoUpdate, notify, enabled sources, ignored ids, skipped update offers, post-update commands, lastRun)
  Scanner.swift      Pure logic: discover / check / update per SourceKind (nonisolated, static)
  Shell.swift        Runs commands through the user's login shell; Transcript (update log); LogFile
  Models.swift       SourceKind, Agent, ItemStatus, UpdatableItem, UpdateResult, LogEntry
  MenuView.swift     The menu bar panel and ItemRow
  SettingsView.swift General / Sources / Items tabs
  LogView.swift      Log window
Resources/AppIcon.svg   the icon's source; AppIcon.icns is rendered from it
Resources/AppIcon.icns
Info.plist           LSUIElement=true (no Dock icon), bundle id dev.pullday.app
build.sh
Tests/SelfTest.swift   ./build.sh --test
Tests/models.golden    kinds, agents, skill roots, item ids; checked by the self-test and by cargo test
Tests/scanner.golden   one fixture home scanned by both (ids, details, agents, statuses); the same check
rust/                  Cargo workspace for the Linux app; pullday-core ports the logic files (models, shell, every source kind, settings in ~/.config/pullday/config.json, the schedule; `cargo run --example scan` runs a check as the app would)
```

`rust/` is the Linux port (Tauri v2 + a Rust core), built and tested on Linux (`cargo test` passes on macOS too): `cd rust && cargo test`; CI runs fmt, clippy, test and a release build on ubuntu-24.04 and macos-15. The Swift files stay the reference; a change that alters a golden file needs `WRITE_GOLDEN=1 ./build.sh --test` and the same change in `rust/pullday-core`. The core's only dependency is serde_json.

## Key design decisions
- **Sources** (`SourceKind`): `plugin` (`~/.claude/plugins/marketplaces/*`, installed plugins read from `~/.claude/plugins/installed_plugins.json`), `gitSkill` (git repos under every skill root in `Agent.skillRootReaders`: `~/.claude/skills`, `~/.agents/skills`, `~/.gemini/skills`, `~/.config/opencode/skills`, `~/.cursor/skills`; symlinks resolved, walks up to the repo root but never treats a root or any ancestor of one as the repo), `uv`, `pipx`, `npm` (npm off by default), `claudeCode` (the `claude` CLI on PATH; one item `claude-code`), `codexPlugin` (`~/.codex/.tmp/marketplaces/*`, each with a `.codex-marketplace-install.json` `{source_type, source, ref_name, sparse_paths, revision}`; installed plugins from `~/.codex/plugins/cache/<mk>/`), `codexCLI` (the `codex` CLI; one item `codex`), `geminiExtension` (`~/.gemini/extensions/*`: `gemini-extension.json` name/version/migratedTo plus `.gemini-extension-install.json` `{source, type: git|local|link|github-release, ref, releaseTag, allowPreRelease}`). There's no Gemini CLI updater (the CLI updates itself). `antigravityPlugin` (`agy plugin list --json`: name, enabled, version, path under `~/.gemini/config/plugins/`), `antigravityCLI` (the `agy` command; one item `agy`).
- **Checks**: git sources → `git fetch` + `rev-list --count HEAD..@{u}`; snapshot marketplaces (no `.git`, e.g. `claude-plugins-official`) → compare `.gcs-sha` with `git ls-remote` on the GitHub repo named in `~/.claude/plugins/known_marketplaces.json`; uv → `uv tool list --outdated`; pipx → `pipx runpip <pkg> list --outdated --format json`; npm → `npm outdated -g --json` (exits 1 when outdated, so status is ignored); claudeCode → for a Homebrew cask install (path under `/Caskroom/claude-code/`), `claude --version` vs `.version` from `https://formulae.brew.sh/api/cask/claude-code.json` (stable channel; `brew outdated` reads a possibly stale local cache), otherwise reported as not checkable; codexPlugin → `remoteHeadStatus` on the install record's `source` (`owner/repo` = GitHub) and `ref_name` (default HEAD) vs `revision`, local sources not checkable; geminiExtension mirrors Gemini CLI's own `checkForExtensionUpdate`: git → `rev-parse HEAD` vs `remoteHeadStatus` on origin and `ref`, github-release → `releaseTag` vs the GitHub API (`releases/tags/<ref>`, else `releases/latest`, else or with `allowPreRelease` `releases?per_page=1`; any API failure, rate limit included, is `.unknown`), local → version in the source folder, link → nothing to update, `migratedTo` → an update; antigravityPlugin → a plugin installed from a git repo is a (shallow) checkout tracking its upstream, so `gitStatus`; others are not checkable; antigravityCLI → `agy --version` vs `version` in the installer's manifest (`…run.app/manifests/darwin_<arch>.json`); codexCLI → `codex --version` vs the cask JSON (`/Caskroom/codex/`) or npm `@openai/codex` latest. Remote ref checks (`remoteHeadStatus`, also used for Claude snapshots) query `<ref>` and `<ref>^{}` and prefer the peeled tag, then the tag, then the branch.
- **Updates**: plugin → `claude plugin marketplace update <mk>` then `claude plugin update <plugin>@<mk>` for each installed plugin (falls back to `git pull --ff-only` if `claude` isn't on PATH); gitSkill → `git pull --ff-only`; uv → `uv tool upgrade`; pipx → `pipx upgrade`; npm → `npm install -g <pkg>@latest`; claudeCode → `brew upgrade --cask claude-code` (or `claude update` for a non-Homebrew install), failing if `claude --version` didn't change. codexPlugin → `codex plugin marketplace upgrade <mk>` (refreshes installed plugin caches too); codexCLI → `codex update`, with the same no-change guard; antigravityPlugin → `agy plugin install <origin URL>` over the existing checkout (verified on agy 1.4.0: it updates in place and keeps the enabled state; never uninstall + install), only for an `https://`, `ssh://` or `git@` origin, failing if the re-check is still behind; antigravityCLI → `agy update` with the no-change guard; geminiExtension → `gemini extensions update <name>`, then a re-check: a consent prompt ([Y/n]) gets no answer with stdin closed and can exit 0 having done nothing, and Gemini exits 0 even on errors: the update counts only if the re-check is up to date or a different offer (same offer → a run-it-in-a-terminal note, can't check → "couldn't confirm"). PullDay never answers consent prompts itself. The Claude desktop app's sessions run their own bundled Claude Code, so this only affects the terminal `claude`. Updates run sequentially.
- **Post-update commands** per item id (default: `uv:graphifyy` and `pipx:graphifyy` → `graphify install`, because upgrading the CLI doesn't refresh the skill files in `~/.claude/skills`).
  If graphify's skill files (`~/.claude/skills/graphify/.graphify_version`) are older than `graphify --version`, the uv/pipx check reports graphifyy as an update, so Update runs that post command (`Scanner.checkGraphifySkill`).
- **Skipping**: "Skip This Update" stores the item's `updateAvailable` message in `Prefs.skippedUpdates`; while a check returns the same message the item shows `.skipped`, and a different offer or "up to date" drops the skip (`ItemStatus.resolvingSkip`). Skipped and pinned items are never counted as updates.
- **Agents** (`Agent`: claude, codex, gemini, opencode, cursor): `UpdatableItem.agents` is the set of agents that load an item (a git skill gets the readers of every root it was found in; plugins and `claude-code` → Claude; uv/pipx/npm → none). The menu has a section per installed agent (its `homeDir` exists) with its own "Update all (n)", plus "Other" for items no installed agent loads; a shared skill appears under each agent. `ItemGroup.id` is `"<agent>|<kind>"`.
- **New kinds**: `Prefs` turns on default-on kinds missing from `knownKinds` once (seeded with `Prefs.legacyKinds`, never `allCases`).
- **Item ids**: `"<kind>:<name>"`, except git skills use `"gitSkill:<repo path>"` to avoid name collisions.
- **Shell**: GUI apps don't inherit the terminal PATH, so every command runs as `$SHELL -l -c` with Homebrew, `~/.local/bin`, `~/.claude/local`, newest nvm Node etc. appended to PATH. `GIT_TERMINAL_PROMPT=0` and `ssh BatchMode` so nothing hangs on auth; stdin is /dev/null; per-command timeout. Output is read as it arrives, and a run ends within 2s of the shell exiting even if a background child it started still holds the pipe.
- **Scheduling**: a 15-minute timer + wake notification call `runIfDue()`, which runs when `now - lastRun >= checkInterval` (0 = manual only). On launch it always refreshes, and applies the scheduled behavior if a run was due. The top-level skill roots and `~/.claude/plugins/marketplaces` are watched (DispatchSource); adding, removing or renaming an entry triggers a debounced `refresh(recordRun: false)`, which doesn't move `lastRun`.
- **Auto-update** defaults to off (checks and notifications still run). Git skills whose repo is outside every agent's folder (the parent of each skill root: `~/.claude`, `~/.agents`, `~/.gemini`, `~/.config/opencode`, `~/.cursor`; reached through a symlink, usually the user's own project) are `manualOnly`: the schedule never pulls them, it only notifies.
- **Hardening**: `$SHELL` is used only if it's sh/bash/zsh/dash/ksh (`Shell.quote` is POSIX), else `/bin/zsh`; every git call goes through `Scanner.git` (no hooks, no fsmonitor, no `file://`); npm globals with a `resolved` field (linked, `file:`, git) are skipped; a timeout SIGTERMs the shell's whole process group.
- **Windows**: Settings and Log are plain `NSWindow`s via `WindowManager`, not SwiftUI `Window`/`Settings` scenes, because scene windows behave unpredictably in an LSUIElement menu-bar app and `SettingsLink`/`showSettingsWindow:` differ across macOS versions.
- **Login item**: `SMAppService.mainApp`. **Notifications**: `UNUserNotificationCenter`, banners shown even when active.
- **Name**: the app was called SkillSync until the rename. On the first launch as PullDay (`dev.pullday.app`), `Prefs.copySettings` copies the old `dev.skillsync.app` settings once and leaves them in place. The menu bar glyph (`MenuBarLabel.glyph`) is the icon's barbell and arrow drawn in code as a template image.
- **Log**: in-memory (last 200) + `~/Library/Logs/PullDay.log` (reset past 2 MB).

## Conventions
- Keep `Scanner` free of UI/actor state; all `@Published` mutation happens on `AppState` (MainActor).
- Quote every interpolated shell value with `Shell.quote`.
- Use `@StateWrapper`, never `@State`: on the Command Line Tools SDK `@State` resolves to a SwiftUI macro whose plugin only ships with Xcode. Other SwiftUI macros (`#Preview`, `@Entry`, `@Animatable`) fail the same way.
- New source type = add a `SourceKind` case (title, symbol, explanation, enabledByDefault, agents), the three switch arms in `Scanner` (discover / check / update), and a self-test check for any parsing or decision logic.
- Don't add third-party dependencies; the build is a single `swiftc` call.
