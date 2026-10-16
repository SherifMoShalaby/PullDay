import ServiceManagement
import SwiftUI
import UserNotifications

struct SettingsView: View {
    @EnvironmentObject private var state: AppState

    var body: some View {
        TabView(selection: $state.settingsTab) {
            GeneralSettings()
                .tabItem { Label("General", systemImage: "gearshape") }
                .tag(SettingsTab.general)
            SourcesSettings()
                .tabItem { Label("Sources", systemImage: "tray.2") }
                .tag(SettingsTab.sources)
            ItemsSettings()
                .tabItem { Label("Items", systemImage: "list.bullet") }
                .tag(SettingsTab.items)
        }
        .padding(16)
        .frame(width: 580, height: 480)
    }
}

private struct GeneralSettings: View {
    @EnvironmentObject private var prefs: Prefs
    @StateWrapper private var launchAtLogin = SMAppService.mainApp.status == .enabled
    @StateWrapper private var loginError: String?
    @StateWrapper private var notificationsDenied = false

    var body: some View {
        Form {
            Section {
                Picker("Check for updates", selection: $prefs.checkInterval) {
                    Text("Every 6 hours").tag(TimeInterval(6 * 3600))
                    Text("Every 12 hours").tag(TimeInterval(12 * 3600))
                    Text("Daily").tag(TimeInterval(24 * 3600))
                    Text("Weekly").tag(TimeInterval(7 * 24 * 3600))
                    Text("Only when I ask").tag(TimeInterval(0))
                }
                Toggle("Install updates automatically", isOn: $prefs.autoUpdate)
                    .disabled(prefs.checkInterval == 0)
                Toggle("Show notifications", isOn: $prefs.notify)
                if prefs.notify && notificationsDenied {
                    HStack {
                        Text("Notifications are turned off for PullDay in System Settings.")
                            .font(.caption).foregroundStyle(.orange)
                        Spacer()
                        Button("Open Settings…") {
                            NSWorkspace.shared.open(URL(string: "x-apple.systempreferences:com.apple.Notifications-Settings.extension")!)
                        }
                        .controlSize(.small)
                    }
                }
            } footer: {
                Text("Scheduled checks also run when PullDay starts and after your Mac wakes, if one is due. Ignored items are never touched.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }

            Section {
                Toggle("Open PullDay at login", isOn: $launchAtLogin)
                    .onChange(of: launchAtLogin) { enabled in setLaunchAtLogin(enabled) }
                if let loginError {
                    Text(loginError).font(.caption).foregroundStyle(.red)
                }
            } footer: {
                Text("Works best when PullDay.app is in your Applications folder.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
        }
        .formStyle(.grouped)
        .task { await refreshNotificationStatus() }
        .onReceive(NotificationCenter.default.publisher(for: NSApplication.didBecomeActiveNotification)) { _ in
            Task { await refreshNotificationStatus() }
        }
    }

    private func refreshNotificationStatus() async {
        let status = await UNUserNotificationCenter.current().notificationSettings().authorizationStatus
        notificationsDenied = status == .denied
    }

    private func setLaunchAtLogin(_ enabled: Bool) {
        do {
            if enabled {
                try SMAppService.mainApp.register()
            } else {
                try SMAppService.mainApp.unregister()
            }
            loginError = nil
        } catch {
            loginError = error.localizedDescription
            let actual = SMAppService.mainApp.status == .enabled
            if actual != launchAtLogin { launchAtLogin = actual }
        }
    }
}

private struct SourcesSettings: View {
    @EnvironmentObject private var prefs: Prefs
    @EnvironmentObject private var state: AppState

    var body: some View {
        Form {
            Section("Agents") {
                ForEach(Agent.allCases) { agent in
                    HStack {
                        VStack(alignment: .leading, spacing: 3) {
                            Text(agent.title)
                            Text(Agent.skillRootReaders.filter { $0.agents.contains(agent) }.map { "~/" + $0.root }
                                    .joined(separator: ", "))
                                .font(.caption)
                                .foregroundStyle(.secondary)
                        }
                        Spacer()
                        Text(agent.isDetected ? "Found" : "Not found")
                            .font(.caption)
                            .foregroundStyle(agent.isDetected ? Color.green : Color.secondary)
                    }
                }
            }
            Section("Sources") {
                ForEach(SourceKind.allCases) { kind in
                    Toggle(isOn: binding(for: kind)) {
                        VStack(alignment: .leading, spacing: 3) {
                            Label(kind.title, systemImage: kind.symbol)
                            Text(LocalizedStringKey(kind.explanation))
                                .font(.caption)
                                .foregroundStyle(.secondary)
                                .fixedSize(horizontal: false, vertical: true)
                        }
                    }
                    .padding(.vertical, 2)
                }
            }
        }
        .formStyle(.grouped)
        .onChange(of: prefs.enabledKinds) { _ in
            Task { await state.refresh() }
        }
    }

    private func binding(for kind: SourceKind) -> Binding<Bool> {
        Binding(
            get: { prefs.enabledKinds.contains(kind.rawValue) },
            set: { on in
                if on { prefs.enabledKinds.insert(kind.rawValue) } else { prefs.enabledKinds.remove(kind.rawValue) }
            }
        )
    }
}

private struct ItemsSettings: View {
    @EnvironmentObject private var prefs: Prefs
    @EnvironmentObject private var state: AppState

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            Text("Ignore anything PullDay shouldn't touch, and add a command to run after an item updates. For example, `graphify install` copies graphify's new skill files into Claude Code.")
                .font(.callout)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)

            if state.items.isEmpty {
                Spacer()
                Text("No items found yet. Run a check from the menu bar.")
                    .foregroundStyle(.secondary)
                    .frame(maxWidth: .infinity)
                Spacer()
            } else {
                List(state.items) { item in
                    VStack(alignment: .leading, spacing: 6) {
                        HStack(spacing: 8) {
                            Image(systemName: item.kind.symbol).foregroundStyle(.secondary).frame(width: 16)
                            Text(item.name).fontWeight(.medium)
                            Text(([item.kind.title] + Agent.allCases.filter(item.agents.contains).map(\.title))
                                    .joined(separator: " · "))
                                .font(.caption).foregroundStyle(.secondary)
                            Spacer()
                            Toggle("Ignore", isOn: ignoreBinding(item)).toggleStyle(.checkbox)
                        }
                        TextField("Post-update command (optional)", text: commandBinding(item))
                            .textFieldStyle(.roundedBorder)
                            .font(.system(.callout, design: .monospaced))
                    }
                    .padding(.vertical, 4)
                }
            }
        }
    }

    private func ignoreBinding(_ item: UpdatableItem) -> Binding<Bool> {
        Binding(
            get: { prefs.ignored.contains(item.id) },
            set: { state.setIgnored(item.id, $0) }
        )
    }

    private func commandBinding(_ item: UpdatableItem) -> Binding<String> {
        Binding(
            get: { prefs.postCommands[item.id] ?? "" },
            set: { value in
                let trimmed = value.trimmingCharacters(in: .whitespaces)
                prefs.postCommands[item.id] = trimmed.isEmpty ? nil : value
            }
        )
    }
}
