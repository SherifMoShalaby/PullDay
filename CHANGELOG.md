# Changelog

What changed in each release. New entries go under **Unreleased**; releasing renames that heading to the version and date. Each GitHub release shows its version's section.

## Unreleased

- A command that leaves a process running in the background (for example a post-update command that starts a server) no longer keeps PullDay waiting, which could leave it stuck on "Updating…".

## 1.0.0 - 2026-10-16

The first public release.

- Keeps Claude Code plugin marketplaces, the `claude` and `codex` commands, Codex plugin marketplaces, Gemini CLI extensions, git-cloned skills for Claude Code, Codex, Gemini CLI, OpenCode and Cursor, and uv, pipx and npm tools up to date.
- A menu section per installed agent, each with its own Update all.
- Daily checks by default. Automatic updates are opt-in.
- Skip an update until a newer one appears, ignore items, and run a command after an item updates (graphify's `graphify install` is set up by default).
- Safe by default: git never runs repository hooks, repos linked from a skills folder are only updated when you ask, and no password or consent prompt is ever answered for you.
