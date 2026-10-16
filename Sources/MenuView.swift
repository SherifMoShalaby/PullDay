import AppKit
import SwiftUI

/// One kind of item under one agent's section; `agent` nil is the "Other" section.
private struct ItemGroup: Identifiable {
    let agent: Agent?
    let kind: SourceKind
    let items: [UpdatableItem]
    /// The first group of its agent's section, which carries the section header.
    let startsSection: Bool
    var id: String { "\(agent?.rawValue ?? "other")|\(kind.rawValue)" }
}

struct MenuContentView: View {
    @EnvironmentObject private var state: AppState
    @EnvironmentObject private var prefs: Prefs

    /// An item is listed under every installed agent that loads it, so a shared skill shows up more than once.
    /// Items no installed agent loads (uv/pipx/npm tools, skills for missing agents) go under "Other".
    private var groups: [ItemGroup] {
        let detected = Agent.allCases.filter(\.isDetected)
        var result: [ItemGroup] = []
        for agent in detected.map(Optional.some) + [nil] {
            var first = true
            for kind in SourceKind.allCases {
                let list = state.visibleItems.filter { item in
                    item.kind == kind && (agent.map { item.agents.contains($0) } ?? item.agents.isDisjoint(with: detected))
                }
                guard !list.isEmpty else { continue }
                result.append(ItemGroup(agent: agent, kind: kind, items: list, startsSection: first))
                first = false
            }
        }
        return result
    }

    var body: some View {
        VStack(spacing: 0) {
            header
            Divider()
            if !state.updatesAvailable.isEmpty {
                banner
                Divider()
            }
            if groups.isEmpty {
                emptyState
            } else {
                list
            }
            Divider()
            footer
        }
        .frame(width: 380)
    }

    // MARK: Sections

    private var header: some View {
        HStack(alignment: .center, spacing: 10) {
            VStack(alignment: .leading, spacing: 2) {
                Text("PullDay").font(.headline)
                statusLine.font(.caption).foregroundStyle(.secondary)
            }
            Spacer()
            if state.isBusy {
                ProgressView().controlSize(.small)
            }
            Button {
                Task { await state.refresh() }
            } label: {
                Image(systemName: "arrow.clockwise")
            }
            .buttonStyle(.borderless)
            .disabled(state.isBusy)
            .help("Check for updates now")
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 10)
    }

    @ViewBuilder private var statusLine: some View {
        switch state.phase {
        case .scanning: Text("Checking for updates…")
        case .updating: Text("Updating…")
        case .idle:
            if let last = prefs.lastRun {
                Text("Checked \(last, style: .relative) ago")
            } else {
                Text("Not checked yet")
            }
        }
    }

    private var banner: some View {
        let count = state.updatesAvailable.count
        return HStack(spacing: 8) {
            Image(systemName: "arrow.down.circle.fill").foregroundStyle(Color.accentColor)
            Text("\(count) update\(count == 1 ? "" : "s") available").font(.callout.weight(.medium))
            Spacer()
            Button("Update All") {
                Task { await state.update(state.updatesAvailable.map(\.id)) }
            }
            .buttonStyle(.borderedProminent)
            .controlSize(.small)
            .disabled(state.isBusy)
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 8)
        .background(Color.accentColor.opacity(0.08))
    }

    private var list: some View {
        let groups = groups
        let rows = groups.reduce(0) { $0 + $1.items.count }
        let sections = groups.filter(\.startsSection).count
        let height = min(CGFloat(rows) * 46 + CGFloat(groups.count) * 28 + CGFloat(sections) * 32 + 8, 440)
        return ScrollView {
            VStack(alignment: .leading, spacing: 0) {
                ForEach(groups) { group in
                    if group.startsSection {
                        sectionHeader(group.agent, pending: groups.filter { $0.agent == group.agent }
                            .flatMap(\.items).filter(\.status.isUpdateAvailable))
                    }
                    Text(group.kind.title.uppercased())
                        .font(.caption2.weight(.semibold))
                        .foregroundStyle(.secondary)
                        .padding(.horizontal, 14)
                        .padding(.top, 10)
                        .padding(.bottom, 4)
                    ForEach(group.items) { item in
                        ItemRow(item: item)
                    }
                }
            }
            .padding(.bottom, 6)
        }
        .frame(height: height)
    }

    private func sectionHeader(_ agent: Agent?, pending: [UpdatableItem]) -> some View {
        HStack {
            Text(agent?.title ?? "Other").font(.subheadline.weight(.semibold))
            Spacer()
            if !pending.isEmpty {
                Button("Update all (\(pending.count))") {
                    Task { await state.update(pending.map(\.id)) }
                }
                .controlSize(.mini)
                .disabled(state.isBusy)
            }
        }
        .padding(.horizontal, 14)
        .padding(.top, 12)
        .frame(minHeight: 32)
    }

    private var emptyState: some View {
        VStack(spacing: 8) {
            if state.phase == .scanning {
                ProgressView()
                Text("Looking for skills and tools…").foregroundStyle(.secondary)
            } else {
                Image(systemName: "tray").font(.system(size: 28)).foregroundStyle(.secondary)
                Text("Nothing to keep up to date yet").font(.callout.weight(.medium))
                Text("PullDay looks in your agents' plugin, extension and skills folders, and your uv and pipx tools. Turn sources on or off in Settings.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .multilineTextAlignment(.center)
            }
        }
        .padding(24)
        .frame(maxWidth: .infinity)
    }

    private var footer: some View {
        HStack(spacing: 16) {
            Button { WindowManager.shared.showSettings() } label: {
                Label("Settings", systemImage: "gearshape")
            }
            Button { WindowManager.shared.showLog() } label: {
                Label("Log", systemImage: "list.bullet.rectangle")
            }
            Spacer()
            Button("Quit") { NSApp.terminate(nil) }
        }
        .buttonStyle(.borderless)
        .font(.callout)
        .padding(.horizontal, 14)
        .padding(.vertical, 9)
    }
}

struct ItemRow: View {
    let item: UpdatableItem
    @EnvironmentObject private var state: AppState
    @StateWrapper private var hovering = false

    var body: some View {
        HStack(spacing: 10) {
            Image(systemName: item.kind.symbol)
                .frame(width: 18)
                .foregroundStyle(.secondary)
            VStack(alignment: .leading, spacing: 2) {
                Text(item.name)
                    .font(.system(size: 13, weight: .medium))
                    .lineLimit(1)
                Text(subtitle)
                    .font(.caption)
                    .foregroundStyle(subtitleColor)
                    .lineLimit(1)
                    .truncationMode(.middle)
                    .help(fullSubtitle)
            }
            Spacer(minLength: 6)
            trailing
            Menu {
                actions
            } label: {
                Image(systemName: "ellipsis")
            }
            .menuStyle(.borderlessButton)
            .menuIndicator(.hidden)
            .fixedSize()
            .opacity(hovering ? 1 : 0.35)
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 6)
        .frame(minHeight: 46)
        .background(hovering ? Color.primary.opacity(0.06) : Color.clear)
        .contentShape(Rectangle())
        .onHover { hovering = $0 }
        .contextMenu { actions }
    }

    /// The tooltip shows the whole message, e.g. the commit list under "2 new commits".
    private var fullSubtitle: String {
        if case .updateAvailable(let info?) = item.status { return info }
        return subtitle
    }

    private var subtitle: String {
        switch item.status {
        case .updateAvailable(let info): return info.map { String($0.prefix { $0 != "\n" }) } ?? "Update available"
        case .failed(let message): return message
        case .updated(let summary): return summary
        case .checking: return "Checking…"
        case .updating: return "Updating…"
        case .unknown(let reason): return reason.map { "\(item.detail) · \($0)" } ?? item.detail
        case .pinned(let message): return "\(item.detail) · \(message)"
        case .skipped(let offer): return "\(item.detail) · Skipped: \(offer.prefix { $0 != "\n" })"
        case .upToDate: return item.detail
        }
    }

    private var subtitleColor: Color {
        switch item.status {
        case .failed: return .orange
        case .updateAvailable: return .accentColor
        case .updated: return .green
        default: return .secondary
        }
    }

    @ViewBuilder private var trailing: some View {
        switch item.status {
        case .checking, .updating:
            ProgressView().controlSize(.small)
        case .updateAvailable:
            Button("Update") { Task { await state.update([item.id]) } }
                .controlSize(.small)
                .disabled(state.isBusy)
        case .upToDate, .updated:
            Image(systemName: "checkmark.circle.fill").foregroundStyle(.green).help("Up to date")
        case .failed:
            Image(systemName: "exclamationmark.triangle.fill").foregroundStyle(.orange)
        case .skipped:
            Image(systemName: "forward.end.circle").foregroundStyle(.secondary)
                .help("You skipped this update. PullDay will offer the next one.")
        case .pinned:
            Image(systemName: "pin.fill").foregroundStyle(.secondary)
                .help("Installed with an exact version pin, so PullDay won't update it")
        case .unknown:
            Image(systemName: "questionmark.circle").foregroundStyle(.secondary).help("Couldn't tell if there's an update")
        }
    }

    @ViewBuilder private var actions: some View {
        Button("Update Now") { Task { await state.update([item.id]) } }
            .disabled(state.isBusy)
        if case .updateAvailable(_?) = item.status {
            Button("Skip This Update") { state.skipUpdate(item.id) }
        }
        if case .skipped = item.status {
            Button("Stop Skipping") { state.stopSkipping(item.id) }
        }
        if let path = item.path {
            Button("Show in Finder") { NSWorkspace.shared.open(URL(fileURLWithPath: path)) }
        }
        Button("Post-Update Command…") { WindowManager.shared.showSettings(tab: .items) }
        Button("Show Log") { WindowManager.shared.showLog() }
        Divider()
        Button("Ignore") { state.setIgnored(item.id, true) }
    }
}
